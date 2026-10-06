//! The coding agents a worker can host, by name: what the worker starts and
//! what clients offer to start.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    Opencode,
    ClaudeCode,
}

impl HarnessKind {
    /// Every harness a worker can host.
    pub const ALL: [HarnessKind; 2] = [HarnessKind::Opencode, HarnessKind::ClaudeCode];

    pub fn name(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
            Self::ClaudeCode => "claude-code",
        }
    }

    /// One line on what it is, for choosing one.
    pub fn description(self) -> &'static str {
        match self {
            Self::Opencode => "OpenCode, installed on the node if it isn't there",
            Self::ClaudeCode => {
                "Claude Code on your Claude subscription, installed if it isn't there"
            }
        }
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
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Self::ALL.iter().map(|k| k.name()).collect();
                format!("unknown harness {s:?} (supported: {})", names.join(", "))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for kind in HarnessKind::ALL {
            assert_eq!(kind.name().parse::<HarnessKind>(), Ok(kind));
            assert!(!kind.description().is_empty());
        }
        let err = "claude".parse::<HarnessKind>().unwrap_err();
        assert!(err.contains("supported: opencode"), "{err}");
    }
}
