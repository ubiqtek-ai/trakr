use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_budget")]
    pub monthly_budget_usd: f64,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    #[serde(default)]
    pub api_enabled: bool,
    #[serde(default = "default_sync_interval_secs")]
    pub sync_interval_secs: u64,
    #[serde(default)]
    pub otel_enabled: bool,
    #[serde(default = "default_otel_port")]
    pub otel_port: u16,
}

fn default_budget() -> f64 {
    50.0
}
fn default_api_port() -> u16 {
    8788
}
fn default_sync_interval_secs() -> u64 {
    30
}
fn default_otel_port() -> u16 {
    4318
}

impl Default for Config {
    fn default() -> Self {
        Config {
            monthly_budget_usd: default_budget(),
            api_port: default_api_port(),
            api_enabled: false,
            sync_interval_secs: default_sync_interval_secs(),
            otel_enabled: false,
            otel_port: default_otel_port(),
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".trakr").join("config.toml"))
}

/// Load config from `~/.trakr/config.toml`, returning defaults if the file doesn't exist.
pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    let config: Config = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("invalid config at {}: {}", path.display(), e))?;
    Ok(config)
}

/// Write a default config file if one doesn't exist yet.
pub fn write_default_config() -> Result<()> {
    let path = config_path()?;
    if path.exists() {
        return Ok(());
    }
    let content = r#"# trakr configuration

# Monthly spend budget in USD. Shown in the status line as the denominator.
monthly_budget_usd = 50.0

# How often the daemon re-parses Claude transcripts to update spend (seconds).
sync_interval_secs = 30

# HTTP API server (GET /spend/monthly). Disabled by default.
# Enable if you want other processes (e.g. a UI) to query spend over HTTP.
api_enabled = false
api_port = 8788

# OTEL telemetry receiver — captures background API calls (title/summary generation)
# that are not visible in session transcripts, closing ~9% of the spend gap.
# Note: does not work on enterprise Claude Code accounts.
# Enable with: trakr otel enable
otel_enabled = false
otel_port = 4318
"#;
    std::fs::write(&path, content)?;
    Ok(())
}

/// Overwrite `~/.trakr/config.toml` with the given config struct.
///
/// Replaces the file entirely — any hand-written comments will be lost.
/// Use this only from programmatic config-mutation commands (e.g. `trakr otel enable`).
pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path()?;
    let body = toml::to_string(config)
        .map_err(|e| anyhow::anyhow!("serialising config: {}", e))?;
    std::fs::write(&path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HOME_LOCK;
    use std::fs;
    use tempfile::TempDir;

    fn with_home<F: FnOnce() -> Result<()>>(tmp: &TempDir, f: F) -> Result<()> {
        let _guard = HOME_LOCK.lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let result = f();
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn defaults_when_no_file() {
        let tmp = TempDir::new().unwrap();
        with_home(&tmp, || {
            let cfg = load_config()?;
            assert_eq!(cfg.monthly_budget_usd, 50.0);
            assert_eq!(cfg.api_port, 8788);
            assert!(!cfg.api_enabled);
            assert!(!cfg.otel_enabled);
            assert_eq!(cfg.otel_port, 4318);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn loads_custom_budget() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join(".trakr");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("config.toml"), "monthly_budget_usd = 100.0\n").unwrap();

        with_home(&tmp, || {
            let cfg = load_config()?;
            assert_eq!(cfg.monthly_budget_usd, 100.0);
            assert_eq!(cfg.api_port, 8788); // still default
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn write_default_config_creates_file() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join(".trakr");
        fs::create_dir_all(&base).unwrap();

        with_home(&tmp, || {
            write_default_config()?;
            assert!(base.join("config.toml").exists());
            // Should be idempotent.
            write_default_config()?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn save_config_round_trips_otel_flag() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join(".trakr");
        fs::create_dir_all(&base).unwrap();

        with_home(&tmp, || {
            let mut cfg = load_config()?;
            assert!(!cfg.otel_enabled);
            cfg.otel_enabled = true;
            cfg.otel_port = 9999;
            save_config(&cfg)?;

            let reloaded = load_config()?;
            assert!(reloaded.otel_enabled);
            assert_eq!(reloaded.otel_port, 9999);
            Ok(())
        })
        .unwrap();
    }
}
