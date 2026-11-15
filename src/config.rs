//! Restricted TOML subset configuration loader for `arkos.toml`.

use crate::exit::{Error, Result};
use std::path::Path;

/// Runtime configuration loaded from `arkos.toml` or defaults.
#[derive(Debug, Clone)]
pub struct Config {
    pub snapshots_keep: u32,
    pub detail_retention_days: u32,
    pub prefer_mft: bool,
    pub follow_junctions: bool,
    pub busy_timeout_ms: u64,
    pub detail_over_bytes: u64,
    pub excludes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            snapshots_keep: 4,
            detail_retention_days: 90,
            prefer_mft: true,
            follow_junctions: false,
            busy_timeout_ms: 5_000,
            detail_over_bytes: 1_048_576,
            excludes: Vec::new(),
        }
    }
}

impl Config {
    /// Load configuration from `path`, returning defaults when absent.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(Error::Io {
                    context: "read config",
                    source,
                })
            }
        };
        Self::parse(&text)
    }

    /// Parse a simple key=value TOML subset.
    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg = Self::default();
        for (idx, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                return Err(Error::Cli(format!(
                    "arkos.toml:{}: expected key = value",
                    idx + 1
                )));
            };
            let key = key.trim();
            let val = val.trim();
            match key {
                "snapshots_keep" => {
                    let n: u32 = val
                        .parse()
                        .map_err(|_| Error::Cli("snapshots_keep must be an integer".into()))?;
                    if !(1..=12).contains(&n) {
                        return Err(Error::Cli("snapshots_keep must be between 1 and 12".into()));
                    }
                    cfg.snapshots_keep = n;
                }
                "detail_retention_days" => {
                    let n: u32 = val.parse().map_err(|_| {
                        Error::Cli("detail_retention_days must be an integer".into())
                    })?;
                    if !(7..=365).contains(&n) {
                        return Err(Error::Cli(
                            "detail_retention_days must be between 7 and 365".into(),
                        ));
                    }
                    cfg.detail_retention_days = n;
                }
                "prefer_mft" => {
                    cfg.prefer_mft = parse_bool(key, val)?;
                }
                "follow_junctions" => {
                    // Safety invariant: junctions are never followed in M0.
                    let requested = parse_bool(key, val)?;
                    if requested {
                        return Err(Error::Refused(
                            "follow_junctions=true is refused in M0".into(),
                        ));
                    }
                    cfg.follow_junctions = false;
                }
                "busy_timeout_ms" => {
                    cfg.busy_timeout_ms = val
                        .parse()
                        .map_err(|_| Error::Cli("busy_timeout_ms must be an integer".into()))?;
                }
                "detail_over_bytes" => {
                    cfg.detail_over_bytes = val
                        .parse()
                        .map_err(|_| Error::Cli("detail_over_bytes must be an integer".into()))?;
                }
                "excludes" => {
                    cfg.excludes = parse_string_array(val)?;
                }
                other => {
                    return Err(Error::Cli(format!("unknown config key: {other}")));
                }
            }
        }
        Ok(cfg)
    }
}

fn parse_bool(key: &str, val: &str) -> Result<bool> {
    match val {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::Cli(format!("{key} must be true or false"))),
    }
}

fn parse_string_array(val: &str) -> Result<Vec<String>> {
    let trimmed = val.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(Error::Cli("excludes must be a string array".into()));
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|item| {
            let s = item.trim();
            if s.len() >= 2
                && ((s.starts_with('"') && s.ends_with('"'))
                    || (s.starts_with('\'') && s.ends_with('\'')))
            {
                Ok(s[1..s.len() - 1].to_owned())
            } else {
                Err(Error::Cli(format!("invalid exclude glob literal: {s}")))
            }
        })
        .collect()
}
