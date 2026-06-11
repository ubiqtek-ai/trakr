use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_budget")]
    pub monthly_budget_usd: f64,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    #[serde(default = "default_otel_port")]
    pub otel_port: u16,
}

fn default_budget() -> f64 {
    50.0
}
fn default_api_port() -> u16 {
    8787
}
fn default_otel_port() -> u16 {
    4318
}

impl Default for Config {
    fn default() -> Self {
        Config {
            monthly_budget_usd: default_budget(),
            api_port: default_api_port(),
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
    let content = r#"# ctx-trakr configuration

# Monthly spend budget in USD. Shown in the status line as the denominator.
monthly_budget_usd = 50.0

# Port for the HTTP API server (GET /spend/monthly).
api_port = 8787

# Port for the OTLP HTTP receiver.
# Set OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
# and OTEL_EXPORTER_OTLP_PROTOCOL=http/json in your Claude Code environment.
otel_port = 4318
"#;
    std::fs::write(&path, content)?;
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
            assert_eq!(cfg.api_port, 8787);
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
            assert_eq!(cfg.api_port, 8787); // still default
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
}
