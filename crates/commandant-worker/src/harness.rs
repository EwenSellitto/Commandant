//! The coding-agent harnesses a worker can host.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    Opencode,
}

impl HarnessKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
        }
    }

    /// How the worker announces the harness in its hello.
    pub fn capability(self) -> String {
        format!("harness:{}", self.name())
    }
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for HarnessKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "opencode" => Ok(Self::Opencode),
            _ => Err(format!("unknown harness {s:?} (supported: opencode)")),
        }
    }
}
