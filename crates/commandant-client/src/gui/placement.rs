//! Where the window was on screen, and how wide its sidebar, so the next
//! run opens it there.

use std::path::Path;

use anyhow::Result;
use commandant_common::fs;
use serde::{Deserialize, Serialize};

/// The smallest window the views are laid out for, in logical pixels.
pub const MIN_WIDTH: f32 = 800.0;
/// See [`MIN_WIDTH`].
pub const MIN_HEIGHT: f32 = 500.0;

/// The sidebar's width until it is dragged, in logical pixels.
pub const SIDEBAR_WIDTH: f32 = 240.0;
/// The narrowest the sidebar is dragged to, in logical pixels.
pub const SIDEBAR_MIN: f32 = 160.0;
/// See [`SIDEBAR_MIN`]: room is left for the chat beside it.
pub const SIDEBAR_MAX: f32 = 480.0;

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
    /// How wide the sidebar was dragged.
    #[serde(default = "sidebar_width")]
    pub sidebar: f32,
}

fn sidebar_width() -> f32 {
    SIDEBAR_WIDTH
}

/// `width` within what the sidebar is dragged to.
pub fn sidebar_within(width: f32) -> f32 {
    width.clamp(SIDEBAR_MIN, SIDEBAR_MAX)
}

impl Placement {
    /// The saved placement, grown to the minimum size with its sidebar
    /// within bounds; none when nothing
    /// usable is saved, and the window opens centred instead.
    pub fn load(path: &Path) -> Option<Self> {
        let read = fs::read_optional(path).and_then(|text| match text {
            Some(text) => Ok(Some(toml::from_str::<Self>(&text)?)),
            None => Ok(None),
        });
        let saved = match read {
            Ok(saved) => saved?,
            Err(e) => {
                eprintln!("warning: ignoring the saved window placement: {e:#}");
                return None;
            }
        };
        Some(Self {
            width: saved.width.max(MIN_WIDTH),
            height: saved.height.max(MIN_HEIGHT),
            sidebar: sidebar_within(saved.sidebar),
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
            sidebar: 300.0,
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
    fn a_placement_saved_before_the_sidebar_could_be_resized_keeps_the_default_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.toml");
        std::fs::write(
            &path,
            "x = 40.0\ny = 30.0\nwidth = 1200.0\nheight = 800.0\n",
        )
        .unwrap();
        assert_eq!(Placement::load(&path).unwrap().sidebar, SIDEBAR_WIDTH);
    }

    #[test]
    fn a_sidebar_dragged_out_of_bounds_comes_back_within_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.toml");
        for (saved, loaded) in [(20.0, SIDEBAR_MIN), (5000.0, SIDEBAR_MAX)] {
            let placement = Placement {
                sidebar: saved,
                ..placement()
            };
            placement.save(&path).unwrap();
            assert_eq!(Placement::load(&path).unwrap().sidebar, loaded);
        }
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
