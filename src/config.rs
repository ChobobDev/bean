//! Settings read from the environment.

use std::path::PathBuf;

use anyhow::{Result, bail};

pub struct Config {
    pub discord_token: String,
    /// Where conversations and reminders are saved.
    pub data_dir: PathBuf,
}

impl Config {
    /// Reads `DISCORD_TOKEN` (required) and `BEAN_DATA_DIR` (default `data`).
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let Some(discord_token) = lookup("DISCORD_TOKEN").filter(|t| !t.trim().is_empty()) else {
            bail!("DISCORD_TOKEN is not set");
        };
        let data_dir = lookup("BEAN_DATA_DIR")
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| "data".into())
            .into();
        Ok(Self {
            discord_token,
            data_dir,
        })
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.data_dir.join("conversations")
    }

    pub fn reminders_path(&self) -> PathBuf {
        self.data_dir.join("reminders.json")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let vars: HashMap<_, _> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn requires_a_token() {
        assert!(config(&[]).is_err());
        assert!(config(&[("DISCORD_TOKEN", "  ")]).is_err());
    }

    #[test]
    fn defaults_the_data_dir() {
        let config = config(&[("DISCORD_TOKEN", "t")]).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert_eq!(
            config.conversations_dir(),
            PathBuf::from("data/conversations")
        );
        assert_eq!(
            config.reminders_path(),
            PathBuf::from("data/reminders.json")
        );
    }

    #[test]
    fn reads_a_custom_data_dir() {
        let config = config(&[("DISCORD_TOKEN", "t"), ("BEAN_DATA_DIR", "/srv/bean")]).unwrap();
        assert_eq!(
            config.conversations_dir(),
            PathBuf::from("/srv/bean/conversations")
        );
    }
}
