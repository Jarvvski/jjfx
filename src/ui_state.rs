//! jjfx's persisted UI state: small presentation toggles that survive a
//! relaunch (currently just whether the home view's world-graph pane is open).
//! Lives in the XDG *state* dir beside the event log - jjfx-owned runtime data,
//! deliberately separate from the user-edited `config.toml`, which jjfx never
//! writes back.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// The persisted toggles. Unknown keys are ignored (not `deny_unknown_fields`):
/// a newer jjfx's state file must not break an older one, and vice versa.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Whether the home view shows the world-graph pane under the list.
    pub world_pane: bool,
}

/// `${XDG_STATE_HOME:-~/.local/state}/jjfx/ui.toml` - the same convention as
/// the event log's path.
pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".local").join("state")
        });
    base.join("jjfx").join("ui.toml")
}

/// Load the state from its fixed path. UI state is a nicety: a missing *or*
/// garbled file yields defaults, never a startup error.
pub fn load() -> UiState {
    load_from(&path())
}

fn load_from(path: &Path) -> UiState {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Save the state to its fixed path (creating the dir if needed).
pub fn save(state: &UiState) -> anyhow::Result<()> {
    save_to(&path(), state)
}

fn save_to(path: &Path, state: &UiState) -> anyhow::Result<()> {
    let text = toml::to_string(state).context("serialising ui state")?;
    let dir = path
        .parent()
        .with_context(|| format!("ui state path {} has no parent", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // A plain write suffices: a torn file just reads back as defaults.
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jjfx-ui-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_the_world_pane_toggle() {
        let dir = scratch("rt");
        let file = dir.join("ui.toml");
        save_to(&file, &UiState { world_pane: true }).unwrap();
        assert_eq!(load_from(&file), UiState { world_pane: true });
        save_to(&file, &UiState { world_pane: false }).unwrap();
        assert_eq!(load_from(&file), UiState { world_pane: false });
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_defaults() {
        assert_eq!(
            load_from(Path::new("/nonexistent/jjfx/ui.toml")),
            UiState::default()
        );
    }

    #[test]
    fn garbled_file_is_defaults_not_an_error() {
        let dir = scratch("bad");
        let file = dir.join("ui.toml");
        fs::write(&file, "not = [valid").unwrap();
        assert_eq!(load_from(&file), UiState::default());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // A state file written by a newer jjfx must still load.
        let state: UiState = toml::from_str("world_pane = true\nfuture_toggle = 3\n").unwrap();
        assert!(state.world_pane);
    }
}
