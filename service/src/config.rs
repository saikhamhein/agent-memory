use serde::Deserialize;
use std::{collections::HashSet, net::SocketAddr, path::PathBuf};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Recall,
    Observe,
    Review,
    Promote,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub name: String,
    pub sha256: String,
    pub capabilities: HashSet<Capability>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub database: PathBuf,
    pub tokens: Vec<Token>,
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.database.is_absolute(),
            "database must be an absolute path"
        );
        anyhow::ensure!(
            !self.tokens.is_empty(),
            "At least one API token is required"
        );
        let mut hashes = HashSet::new();
        let mut names = HashSet::new();
        for token in &self.tokens {
            anyhow::ensure!(
                !token.name.trim().is_empty() && names.insert(&token.name),
                "Token names must be nonempty and unique"
            );
            anyhow::ensure!(
                token.sha256.len() == 64
                    && token
                        .sha256
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "Token SHA256 must be 64 lowercase hexadecimal characters"
            );
            anyhow::ensure!(hashes.insert(&token.sha256), "Duplicate token hash");
            anyhow::ensure!(!token.capabilities.is_empty(), "Token has no capabilities");
        }
        Ok(())
    }
    /// The read-only mirror lives in `mnt` next to the database.
    pub fn mount_dir(&self) -> PathBuf {
        self.database.with_file_name("mnt")
    }
    /// One line per recall, next to the database.
    pub fn recall_log(&self) -> PathBuf {
        self.database.with_file_name("recall-log.jsonl")
    }
}
