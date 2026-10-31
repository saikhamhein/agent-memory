use agent_memory_server::{api, config::Config, model::hash, store::Store};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use uuid::Uuid;

mod migrate;

#[derive(Parser)]
#[command(version, about = "Shared agent memory HTTP service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Serve an existing store; missing databases fail rather than silently starting empty.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Explicitly initialize an empty v2 store.
    Init {
        #[arg(long)]
        database: PathBuf,
    },
    /// Migrate a v1 store to v2 in one transaction. Run it with the service stopped.
    Migrate {
        #[arg(long)]
        database: PathBuf,
        /// Print the report and roll back.
        #[arg(long)]
        dry_run: bool,
        /// Pin this slug's memory (repeatable).
        #[arg(long = "pin", value_name = "SLUG")]
        pins: Vec<String>,
    },
    /// Make a consistent SQLite backup. Refuses to overwrite the output.
    Backup {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Generate an API token and the SHA256 value to place in server configuration.
    Token,
    /// Validate configuration and database access without listening.
    Check {
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Init { database } => {
            Store::init(&database)?;
            println!("Store initialized");
        }
        Command::Migrate {
            database,
            dry_run,
            pins,
        } => {
            let report = migrate::migrate(&database, &mirror(&database, None), &pins, dry_run)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Backup { database, output } => {
            Store::backup(&database, &output)?;
            println!("Backup complete: {}", output.display());
        }
        Command::Token => {
            let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            println!(
                "{}",
                serde_json::json!({"token":token,"sha256":hash(token.as_bytes())})
            );
        }
        Command::Check { config } => {
            let config = Config::load(&config)?;
            let store = Store::open(
                &config.database,
                &mirror(&config.database, config.mount.as_deref()),
            )?;
            println!("{}", store.status()?);
        }
        Command::Serve { config } => {
            let config = Config::load(&config)?;
            let store = Store::open(
                &config.database,
                &mirror(&config.database, config.mount.as_deref()),
            )?;
            let listener = tokio::net::TcpListener::bind(config.listen).await?;
            eprintln!(
                "agent-memory-server {} listening on {}",
                env!("CARGO_PKG_VERSION"),
                listener.local_addr()?
            );
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            let shutdown = async move {
                tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = terminate.recv() => () }
                eprintln!("Shutdown requested; draining requests");
            };
            let server =
                axum::serve(listener, api::router(config, store)).with_graceful_shutdown(shutdown);
            // systemd supplies the final stop timeout if an external DB lock persists.
            server.await?;
        }
    }
    Ok(())
}

/// The mirror directory: `mount` from server.toml, else `mnt` next to the database.
fn mirror(database: &Path, mount: Option<&Path>) -> PathBuf {
    mount.map_or_else(|| database.with_file_name("mnt"), Path::to_path_buf)
}
