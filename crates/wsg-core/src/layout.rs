//! Pane layouts for the kitty tabs jjfx opens: an ordered plan of panes, each
//! with a role, an anchor to split from, a direction, and a bias.
//!
//! The plan is data only. The executing driver owns the command for each role
//! (the configured first-pane command, the agent invocation, or a plain shell),
//! so the same plan can build a workspace tab and a worker Mount tab. The
//! built-in plan reproduces the layout jjfx has always built when no config is
//! supplied.

use serde::{Deserialize, Serialize};

/// The default share of a split given to the new pane.
pub const DEFAULT_BIAS: u8 = 50;

fn default_bias() -> u8 {
    DEFAULT_BIAS
}

/// What a pane runs. The driver maps each role to its own command policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneRole {
    /// The tab's own pane: the configured first-pane command, else a shell.
    First,
    /// The pane running the coding agent.
    Agent,
    /// A plain shell pane.
    Shell,
}

impl PaneRole {
    /// The role's name, as written in config and as tagged on the kitty window
    /// (`--var jjfx_role=<name>`) so a pane can be found again by role.
    pub fn as_str(self) -> &'static str {
        match self {
            PaneRole::First => "first",
            PaneRole::Agent => "agent",
            PaneRole::Shell => "shell",
        }
    }
}

/// How a pane splits away from its anchor. The names match kitty's `--location`
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitLocation {
    /// Side by side with the anchor.
    Vsplit,
    /// Stacked above or below the anchor.
    Hsplit,
}

impl SplitLocation {
    /// The value kitty's `--location=<value>` expects.
    pub fn as_kitty_value(self) -> &'static str {
        match self {
            SplitLocation::Vsplit => "vsplit",
            SplitLocation::Hsplit => "hsplit",
        }
    }
}

/// One pane in a [`PaneLayout`], in build order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanePlacement {
    /// What this pane runs.
    pub role: PaneRole,
    /// Index of the earlier pane this one splits from. Defaults to the tab's
    /// own pane.
    #[serde(default)]
    pub from: usize,
    /// Split direction. Absent only for the tab's own pane; required after.
    #[serde(default)]
    pub location: Option<SplitLocation>,
    /// Percentage of the split given to this new pane (kitty `--bias`).
    #[serde(default = "default_bias")]
    pub bias: u8,
}

/// An ordered pane plan for one tab. The first entry is the tab's own window;
/// every later entry splits from an earlier one, so the plan is a tree rooted
/// at index 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneLayout {
    /// Panes in build order; never empty for a valid plan.
    pub panes: Vec<PanePlacement>,
}

/// The terminal settings a mounted tab is built from, resolved by the frontend
/// from its config: the first-pane command plus the pane layout selected for
/// the current display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSettings {
    /// Command (program + args) for the tab's first pane; empty leaves a shell.
    pub first_pane_command: Vec<String>,
    /// The pane layout the tab is built from.
    pub layout: PaneLayout,
}

impl PaneLayout {
    /// The layout jjfx builds without configuration: a shell column on the
    /// left, split into the configured first pane over a second shell, and the
    /// agent filling the rest.
    pub fn builtin() -> Self {
        Self {
            panes: vec![
                PanePlacement {
                    role: PaneRole::First,
                    from: 0,
                    location: None,
                    bias: DEFAULT_BIAS,
                },
                PanePlacement {
                    role: PaneRole::Agent,
                    from: 0,
                    location: Some(SplitLocation::Vsplit),
                    bias: 81,
                },
                PanePlacement {
                    role: PaneRole::Shell,
                    from: 0,
                    location: Some(SplitLocation::Hsplit),
                    bias: DEFAULT_BIAS,
                },
            ],
        }
    }

    /// Reject plans that cannot be built. Called when config loads so a bad
    /// layout fails before anything opens.
    pub fn validate(&self) -> Result<(), String> {
        if self.panes.is_empty() {
            return Err("a layout needs at least one pane".to_string());
        }
        for (index, pane) in self.panes.iter().enumerate() {
            if pane.bias == 0 || pane.bias == 100 {
                return Err(format!("pane {index} bias must be between 1 and 99"));
            }
            if index == 0 {
                if pane.location.is_some() {
                    return Err(
                        "the first pane cannot have a `location`; it is the tab's own pane"
                            .to_string(),
                    );
                }
                if pane.from != 0 {
                    return Err("the first pane cannot split from another pane".to_string());
                }
                continue;
            }
            if pane.location.is_none() {
                return Err(format!(
                    "pane {index} needs a `location` (vsplit or hsplit)"
                ));
            }
            if pane.from >= index {
                return Err(format!(
                    "pane {index} must split from an earlier pane (0 to {})",
                    index - 1
                ));
            }
        }
        Ok(())
    }

    /// Index of the pane that takes focus after the tab is built: the agent
    /// when the plan has one, else the tab's own pane.
    pub fn focus_pane(&self) -> usize {
        self.panes
            .iter()
            .position(|pane| pane.role == PaneRole::Agent)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_plan_matches_the_historical_layout() {
        let layout = PaneLayout::builtin();
        assert_eq!(layout.focus_pane(), 1);
        assert_eq!(
            layout.panes,
            vec![
                PanePlacement {
                    role: PaneRole::First,
                    from: 0,
                    location: None,
                    bias: 50,
                },
                PanePlacement {
                    role: PaneRole::Agent,
                    from: 0,
                    location: Some(SplitLocation::Vsplit),
                    bias: 81,
                },
                PanePlacement {
                    role: PaneRole::Shell,
                    from: 0,
                    location: Some(SplitLocation::Hsplit),
                    bias: 50,
                },
            ]
        );
        layout.validate().expect("the built-in plan is valid");
    }

    #[test]
    fn role_names_round_trip_through_config() {
        for role in [PaneRole::First, PaneRole::Agent, PaneRole::Shell] {
            let layout: PaneLayout =
                toml::from_str(&format!("[[panes]]\nrole = \"{}\"\n", role.as_str()))
                    .expect("role name parses");
            assert_eq!(layout.panes[0].role, role);
        }
    }

    #[test]
    fn a_plan_parses_role_anchor_direction_and_bias() {
        let layout: PaneLayout = toml::from_str(
            r#"
            [[panes]]
            role = "first"

            [[panes]]
            role = "agent"
            location = "vsplit"
            bias = 70

            [[panes]]
            role = "agent"
            from = 1
            location = "hsplit"
            "#,
        )
        .expect("plan parses");
        assert_eq!(layout.panes.len(), 3);
        assert_eq!(layout.panes[0].from, 0);
        assert_eq!(layout.panes[0].bias, 50);
        assert_eq!(layout.panes[1].location, Some(SplitLocation::Vsplit));
        assert_eq!(layout.panes[2].from, 1);
        assert_eq!(layout.panes[2].bias, 50);
        layout.validate().expect("plan is valid");
    }

    #[test]
    fn unknown_pane_keys_are_rejected() {
        let err = toml::from_str::<PaneLayout>(
            r#"
            [[panes]]
            role = "first"
            locaton = "vsplit"
            "#,
        )
        .expect_err("typo is an error");
        assert!(err.to_string().contains("locaton"), "{err}");
    }

    #[test]
    fn an_empty_plan_is_invalid() {
        let layout = PaneLayout { panes: Vec::new() };
        assert!(
            layout
                .validate()
                .expect_err("empty")
                .contains("at least one")
        );
    }

    #[test]
    fn the_first_pane_cannot_split() {
        for panes in [
            vec![PanePlacement {
                role: PaneRole::First,
                from: 0,
                location: Some(SplitLocation::Vsplit),
                bias: 50,
            }],
            vec![
                PanePlacement {
                    role: PaneRole::First,
                    from: 0,
                    location: None,
                    bias: 50,
                },
                PanePlacement {
                    role: PaneRole::First,
                    from: 1,
                    location: Some(SplitLocation::Vsplit),
                    bias: 50,
                },
            ],
        ] {
            let layout = PaneLayout { panes };
            assert!(layout.validate().is_err());
        }
    }

    #[test]
    fn later_panes_need_a_location_and_an_earlier_anchor() {
        let no_location = PaneLayout {
            panes: vec![
                PanePlacement {
                    role: PaneRole::First,
                    from: 0,
                    location: None,
                    bias: 50,
                },
                PanePlacement {
                    role: PaneRole::Shell,
                    from: 0,
                    location: None,
                    bias: 50,
                },
            ],
        };
        assert!(
            no_location
                .validate()
                .expect_err("location")
                .contains("location")
        );

        let forward_anchor = PaneLayout {
            panes: vec![
                PanePlacement {
                    role: PaneRole::First,
                    from: 0,
                    location: None,
                    bias: 50,
                },
                PanePlacement {
                    role: PaneRole::Shell,
                    from: 1,
                    location: Some(SplitLocation::Vsplit),
                    bias: 50,
                },
            ],
        };
        assert!(
            forward_anchor
                .validate()
                .expect_err("forward anchor")
                .contains("earlier")
        );
    }

    #[test]
    fn bias_must_leave_room_for_both_panes() {
        for bias in [0, 100] {
            let layout = PaneLayout {
                panes: vec![
                    PanePlacement {
                        role: PaneRole::First,
                        from: 0,
                        location: None,
                        bias: 50,
                    },
                    PanePlacement {
                        role: PaneRole::Shell,
                        from: 0,
                        location: Some(SplitLocation::Vsplit),
                        bias,
                    },
                ],
            };
            assert!(layout.validate().expect_err("bias").contains("bias"));
        }
    }

    #[test]
    fn focus_prefers_the_agent_and_falls_back_to_the_first_pane() {
        let no_agent: PaneLayout = toml::from_str(
            r#"
            [[panes]]
            role = "shell"

            [[panes]]
            role = "shell"
            location = "vsplit"
            "#,
        )
        .expect("plan parses");
        assert_eq!(no_agent.focus_pane(), 0);
    }
}
