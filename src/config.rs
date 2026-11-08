//! Settings read from the environment.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::spotify;

pub struct Config {
    pub discord_token: String,
    /// Where conversations, reminders, and daily posts are saved.
    pub data_dir: PathBuf,
    /// Enables song recommendations when set.
    pub spotify: Option<spotify::Credentials>,
}

impl Config {
    /// Reads `DISCORD_TOKEN` (required), `BEAN_DATA_DIR` (default `data`),
    /// and optionally `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET`.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let Some(discord_token) = lookup("DISCORD_TOKEN").filter(|t| !t.trim().is_empty()) else {
            bail!("DISCORD_TOKEN is not set");
        };
        let set = |name| lookup(name).filter(|v: &String| !v.trim().is_empty());
        let data_dir = set("BEAN_DATA_DIR").unwrap_or_else(|| "data".into()).into();
        let spotify = match (set("SPOTIFY_CLIENT_ID"), set("SPOTIFY_CLIENT_SECRET")) {
            (Some(client_id), Some(client_secret)) => Some(spotify::Credentials {
                client_id,
                client_secret,
            }),
            (None, None) => None,
            _ => bail!("set both SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET, or neither"),
        };
        Ok(Self {
            discord_token,
            data_dir,
            spotify,
        })
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.data_dir.join("conversations")
    }

    pub fn reminders_path(&self) -> PathBuf {
        self.data_dir.join("reminders.json")
    }

    pub fn daily_path(&self) -> PathBuf {
        self.data_dir.join("daily.json")
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
    fn spotify_is_optional_but_needs_both_credentials() {
        assert!(config(&[("DISCORD_TOKEN", "t")]).unwrap().spotify.is_none());

        let both = [
            ("DISCORD_TOKEN", "t"),
            ("SPOTIFY_CLIENT_ID", "id"),
            ("SPOTIFY_CLIENT_SECRET", "s"),
        ];
        let spotify = config(&both).unwrap().spotify.unwrap();
        assert_eq!(
            (spotify.client_id.as_str(), spotify.client_secret.as_str()),
            ("id", "s")
        );

        assert!(config(&[("DISCORD_TOKEN", "t"), ("SPOTIFY_CLIENT_ID", "id")]).is_err());
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
