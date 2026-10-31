use agent_memory_server::{api, config::Config, model::hash, store::Store};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use uuid::Uuid;

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
    /// Explicitly initialize a store, or migrate an existing store's journal mode.
    Init {
        #[arg(long)]
        database: PathBuf,
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

/// Placeholder until workstream M adds `config.mount`: `<database dir>/mnt`.
fn mount(database: &std::path::Path) -> PathBuf {
    database.with_file_name("mnt")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Init { database } => {
            Store::init(&database)?;
            println!("Store initialized");
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
            let store = Store::open(&config.database, &mount(&config.database))?;
            println!("{}", store.status()?);
        }
        Command::Serve { config } => {
            let config = Config::load(&config)?;
            let store = Store::open(&config.database, &mount(&config.database))?;
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
