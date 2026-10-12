//! Where the window was on screen, so the next run opens it there.

use std::path::Path;

use anyhow::Result;
use commandant_common::fs;
use serde::{Deserialize, Serialize};

/// The smallest window the views are laid out for, in logical pixels.
pub const MIN_WIDTH: f32 = 800.0;
pub const MIN_HEIGHT: f32 = 500.0;

/// The window's bounds when it last closed, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The bounds above are then the size it restores to.
    #[serde(default)]
    pub maximized: bool,
}

impl Placement {
    /// The saved placement, grown to the minimum size; none when nothing
    /// usable is saved, and the window opens centred instead.
    pub fn load(path: &Path) -> Option<Self> {
        let saved: Self = toml::from_str(&fs::read_optional(path).ok()??).ok()?;
        Some(Self {
            width: saved.width.max(MIN_WIDTH),
            height: saved.height.max(MIN_HEIGHT),
            ..saved
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write_private(path, &toml::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement() -> Placement {
        Placement {
            x: 40.0,
            y: 30.0,
            width: 1200.0,
            height: 800.0,
            maximized: false,
        }
    }

    #[test]
    fn a_saved_placement_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui").join("window.toml");
        placement().save(&path).unwrap();
        assert_eq!(Placement::load(&path), Some(placement()));
    }

    #[test]
    fn no_file_or_a_broken_one_means_no_placement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.toml");
        assert_eq!(Placement::load(&path), None);
        std::fs::write(&path, "width = \"wide\"").unwrap();
        assert_eq!(Placement::load(&path), None);
    }

    #[test]
    fn a_window_smaller_than_the_minimum_grows_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.toml");
        let small = Placement {
            width: 300.0,
            height: 900.0,
            ..placement()
        };
        small.save(&path).unwrap();
        let loaded = Placement::load(&path).unwrap();
        assert_eq!((loaded.width, loaded.height), (MIN_WIDTH, 900.0));
    }
}
