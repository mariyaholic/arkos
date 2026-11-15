//! Path resolution for Arkos (`ARKOS_HOME` or `%LOCALAPPDATA%\mariyaholic\shimatsu`).

use crate::exit::{Error, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Testable environment snapshot for deterministic path resolution.
#[derive(Debug, Clone)]
pub struct PathEnvironment {
    pub windows: bool,
    pub vars: BTreeMap<String, OsString>,
}

impl PathEnvironment {
    /// Capture relevant variables from the current process environment.
    pub fn capture() -> Self {
        let vars = ["ARKOS_HOME", "LOCALAPPDATA", "HOME", "XDG_DATA_HOME"]
            .into_iter()
            .filter_map(|k| std::env::var_os(k).map(|v| (k.to_owned(), v)))
            .collect();
        Self {
            windows: cfg!(windows),
            vars,
        }
    }

    fn absolute(&self, key: &str) -> Result<Option<PathBuf>> {
        match self.vars.get(key).filter(|v| !v.is_empty()) {
            Some(v) => {
                let p = PathBuf::from(v);
                if !p.is_absolute() {
                    return Err(Error::Cli(format!("{key} must be an absolute path")));
                }
                Ok(Some(p))
            }
            None => Ok(None),
        }
    }
}

/// Resolved filesystem locations owned by Arkos and the Shimatsu ledger.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Paths {
    pub home: PathBuf,
    pub db: PathBuf,
    pub locks: PathBuf,
    pub config: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    /// Resolve paths strictly inside the configured application data root.
    pub fn resolve(env: &PathEnvironment) -> Result<Self> {
        let home = if let Some(explicit) = env.absolute("ARKOS_HOME")? {
            explicit
        } else if env.windows {
            env.absolute("LOCALAPPDATA")?
                .ok_or_else(|| Error::Cli("missing LOCALAPPDATA and ARKOS_HOME".into()))?
                .join("mariyaholic")
                .join("shimatsu")
        } else if let Some(xdg) = env.absolute("XDG_DATA_HOME")? {
            xdg.join("mariyaholic").join("shimatsu")
        } else {
            env.absolute("HOME")?
                .ok_or_else(|| Error::Cli("missing HOME and ARKOS_HOME".into()))?
                .join(".local/share/mariyaholic/shimatsu")
        };
        Ok(Self::from_home(home))
    }

    /// Construct standard subdirectories from an explicit root directory.
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref().to_path_buf();
        Self {
            db: home.join("shimatsu.sqlite"),
            locks: home.join("locks"),
            config: home.join("arkos.toml"),
            logs: home.join("logs"),
            home,
        }
    }

    /// Ensure required directories exist on disk.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [&self.home, &self.locks, &self.logs] {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                context: "create arkos directory",
                source,
            })?;
        }
        Ok(())
    }
}
