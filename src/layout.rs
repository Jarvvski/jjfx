//! Selecting the pane layout for a tab: named layouts from config, chosen by
//! display condition, with the `JJFX_LAYOUT` environment override.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use wsg_core::PaneLayout;

use crate::config::{DisplayLayouts, TerminalConfig};
use crate::display::DisplayCondition;

/// The zero-config layout: a 19% left column with the first pane over a second
/// shell, and the agent filling the rest - what jjfx has always built.
static BUILTIN: LazyLock<PaneLayout> = LazyLock::new(PaneLayout::builtin);

/// The layout profiles and display mapping from config, plus the optional
/// `JJFX_LAYOUT` force.
#[derive(Debug, Clone, Default)]
pub struct LayoutSettings {
    profiles: BTreeMap<String, PaneLayout>,
    by_display: DisplayLayouts,
    forced: Option<String>,
}

impl LayoutSettings {
    /// Read the authored selection settings and the `JJFX_LAYOUT` override.
    pub fn from_config(terminal: &TerminalConfig) -> Self {
        Self::new(
            terminal.layouts.clone(),
            terminal.layout_by_display.clone(),
            forced_layout_name(),
        )
    }

    fn new(
        profiles: BTreeMap<String, PaneLayout>,
        by_display: DisplayLayouts,
        forced: Option<String>,
    ) -> Self {
        Self {
            profiles,
            by_display,
            forced,
        }
    }

    /// The layout to build for `condition`: the forced profile when
    /// `JJFX_LAYOUT` names one, else the condition's mapping, else `default`,
    /// else the built-in layout.
    pub fn resolve(&self, condition: Option<DisplayCondition>) -> Result<&PaneLayout> {
        if let Some(forced) = self.forced.as_deref() {
            return self
                .profile(forced)
                .ok_or_else(|| anyhow!("JJFX_LAYOUT names unknown layout `{forced}`"));
        }
        let named = match condition {
            Some(DisplayCondition::Laptop) => self.by_display.laptop.as_deref(),
            Some(DisplayCondition::External) => self.by_display.external.as_deref(),
            None => None,
        }
        .or(self.by_display.fallback.as_deref());
        match named {
            Some(name) => self
                .profile(name)
                .ok_or_else(|| anyhow!("terminal.layout_by_display names unknown layout `{name}`")),
            None => Ok(&BUILTIN),
        }
    }

    fn profile(&self, name: &str) -> Option<&PaneLayout> {
        self.profiles.get(name)
    }
}

/// The `JJFX_LAYOUT` override, when it names a non-blank layout.
fn forced_layout_name() -> Option<String> {
    std::env::var("JJFX_LAYOUT")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsg_core::{PanePlacement, PaneRole};

    fn profile(role: PaneRole, bias: u8) -> PaneLayout {
        PaneLayout {
            panes: vec![
                PanePlacement {
                    role: PaneRole::First,
                    from: 0,
                    location: None,
                    bias: 50,
                },
                PanePlacement {
                    role,
                    from: 0,
                    location: Some(wsg_core::SplitLocation::Vsplit),
                    bias,
                },
            ],
        }
    }

    fn settings(
        profiles: Vec<(&str, PaneLayout)>,
        by_display: DisplayLayouts,
        forced: Option<&str>,
    ) -> LayoutSettings {
        LayoutSettings::new(
            profiles
                .into_iter()
                .map(|(name, layout)| (name.to_string(), layout))
                .collect(),
            by_display,
            forced.map(str::to_string),
        )
    }

    #[test]
    fn no_configuration_resolves_to_the_builtin_layout() {
        let settings = LayoutSettings::default();
        for condition in [
            None,
            Some(DisplayCondition::Laptop),
            Some(DisplayCondition::External),
        ] {
            let resolved = settings.resolve(condition).expect("builtin resolves");
            assert_eq!(resolved, &PaneLayout::builtin(), "{condition:?}");
        }
    }

    #[test]
    fn each_display_condition_selects_its_named_layout() {
        let settings = settings(
            vec![
                ("narrow", profile(PaneRole::Agent, 70)),
                ("wide", profile(PaneRole::Agent, 81)),
            ],
            DisplayLayouts {
                laptop: Some("narrow".to_string()),
                external: Some("wide".to_string()),
                fallback: None,
            },
            None,
        );
        assert_eq!(
            settings
                .resolve(Some(DisplayCondition::Laptop))
                .unwrap()
                .panes[1]
                .bias,
            70
        );
        assert_eq!(
            settings
                .resolve(Some(DisplayCondition::External))
                .unwrap()
                .panes[1]
                .bias,
            81
        );
    }

    #[test]
    fn an_unmapped_condition_falls_back_to_default_then_builtin() {
        let mapped = settings(
            vec![("fallback", profile(PaneRole::Agent, 60))],
            DisplayLayouts {
                laptop: Some("fallback".to_string()),
                external: None,
                fallback: Some("fallback".to_string()),
            },
            None,
        );
        assert_eq!(
            mapped
                .resolve(Some(DisplayCondition::External))
                .unwrap()
                .panes[1]
                .bias,
            60
        );
        assert_eq!(mapped.resolve(None).unwrap().panes[1].bias, 60);

        let unmapped = LayoutSettings::default();
        assert_eq!(unmapped.resolve(None).unwrap(), &PaneLayout::builtin());
    }

    #[test]
    fn the_forced_layout_wins_over_the_display() {
        let settings = settings(
            vec![
                ("narrow", profile(PaneRole::Agent, 70)),
                ("wide", profile(PaneRole::Agent, 81)),
            ],
            DisplayLayouts {
                laptop: Some("narrow".to_string()),
                external: Some("wide".to_string()),
                fallback: None,
            },
            Some("narrow"),
        );
        assert_eq!(
            settings
                .resolve(Some(DisplayCondition::External))
                .unwrap()
                .panes[1]
                .bias,
            70
        );
    }

    #[test]
    fn unknown_profile_names_are_errors() {
        let forced = settings(Vec::new(), DisplayLayouts::default(), Some("missing"));
        let error = forced.resolve(None).expect_err("unknown forced layout");
        assert!(error.to_string().contains("JJFX_LAYOUT"), "{error}");

        let dangling = settings(
            Vec::new(),
            DisplayLayouts {
                laptop: Some("missing".to_string()),
                external: None,
                fallback: None,
            },
            None,
        );
        assert!(dangling.resolve(Some(DisplayCondition::Laptop)).is_err());
    }
}
