use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A release stream. Each channel has its own binaries, data dir, socket and DB (D40).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
    Dev,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Dev => "dev",
        }
    }

    /// Suffix for per-channel directory names: `~/.genie`, `~/.genie-beta`, `~/.genie-dev` (01 §1.2).
    pub fn dir_suffix(self) -> &'static str {
        match self {
            Channel::Stable => "",
            Channel::Beta => "-beta",
            Channel::Dev => "-dev",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Channel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stable" => Ok(Channel::Stable),
            "beta" => Ok(Channel::Beta),
            "dev" => Ok(Channel::Dev),
            other => Err(format!(
                "unknown channel `{other}` (expected stable, beta or dev)"
            )),
        }
    }
}
