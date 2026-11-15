//! Exit taxonomy (0..=6) and unified error type for Arkos.

use std::fmt;

/// Shared 0-6 process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Exit {
    Ok = 0,
    Io = 1,
    Cli = 2,
    Input = 3,
    Refused = 4,
    Unsupported = 5,
    Busy = 6,
}

impl Exit {
    /// Numeric OS exit code.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// Canonical list of all exit variants for uniqueness checks.
    pub fn all() -> [Exit; 7] {
        [
            Exit::Ok,
            Exit::Io,
            Exit::Cli,
            Exit::Input,
            Exit::Refused,
            Exit::Unsupported,
            Exit::Busy,
        ]
    }
}

/// Errors returned by Arkos operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Cli(String),

    #[error("{0}")]
    Input(String),

    #[error("{0}")]
    Refused(String),

    #[error("{0}")]
    Unsupported(String),

    #[error("{0}")]
    Busy(String),

    #[error("I/O during {context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

impl Error {
    /// Map the error to its 0-6 exit code.
    pub fn exit(&self) -> Exit {
        match self {
            Self::Cli(_) => Exit::Cli,
            Self::Input(_) => Exit::Input,
            Self::Refused(_) => Exit::Refused,
            Self::Unsupported(_) => Exit::Unsupported,
            Self::Busy(_) => Exit::Busy,
            Self::Io { .. } | Self::Db(_) => Exit::Io,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Small helper to display exit code alongside its symbolic name.
impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}({})", self, self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::Exit;
    use std::collections::HashSet;

    #[test]
    fn exit_codes_are_unique_0_to_6() {
        let codes: HashSet<i32> = Exit::all().iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), 7);
        for c in 0..=6 {
            assert!(codes.contains(&c));
        }
    }
}
