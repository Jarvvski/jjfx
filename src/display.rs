//! Which display jjfx is opening tabs for. Layouts can be selected per
//! condition, so a machine on its own screen can build a different arrangement
//! from one docked to an external monitor.

use serde::Deserialize;

#[cfg(target_os = "macos")]
use crate::cmd::cmd;

/// The display situations a layout can be selected for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayCondition {
    /// Only the built-in display is attached.
    Laptop,
    /// At least one external display is attached.
    External,
}

/// Detect the display condition of the machine jjfx runs on. `None` when it
/// cannot be determined, so callers fall back to the default layout: non-macOS
/// hosts, a missing or failing `system_profiler`, or a report without displays.
pub fn attached_condition() -> Option<DisplayCondition> {
    classify(&probe()?)
}

/// The `system_profiler` display report, or `None` when the command cannot run.
#[cfg(target_os = "macos")]
fn probe() -> Option<String> {
    cmd("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .run()
        .ok()?
        .checked()
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn probe() -> Option<String> {
    None
}

/// Classify a `system_profiler SPDisplaysDataType -json` report. A display is
/// external when it names a connection type other than `spdisplays_internal`
/// and does not identify as one of Apple's built-in panels.
pub fn classify(json: &str) -> Option<DisplayCondition> {
    let report: ProfilerReport = serde_json::from_str(json).ok()?;
    let displays = report
        .gpus
        .iter()
        .flat_map(|gpu| gpu.spdisplays_ndrvs.iter())
        .collect::<Vec<_>>();
    if displays.is_empty() {
        return None;
    }
    Some(if displays.iter().any(|display| is_external(display)) {
        DisplayCondition::External
    } else {
        DisplayCondition::Laptop
    })
}

/// Is this display entry an external monitor? Unknown entries (no connection
/// type, no display type) are not external, so an unrecognised report keeps the
/// laptop layout rather than silently claiming a monitor.
fn is_external(display: &ProfilerDisplay) -> bool {
    if display
        .spdisplays_display_type
        .as_deref()
        .is_some_and(|kind| kind.starts_with("spdisplays_built-in"))
    {
        return false;
    }
    match display.spdisplays_connection_type.as_deref() {
        Some(connection) => connection != "spdisplays_internal",
        None => false,
    }
}

/// The parts of the report jjfx reads. `system_profiler` keys are not valid
/// Rust identifiers and are matched verbatim.
#[derive(Debug, Deserialize)]
struct ProfilerReport {
    #[serde(rename = "SPDisplaysDataType", default)]
    gpus: Vec<ProfilerGpu>,
}

#[derive(Debug, Deserialize)]
struct ProfilerGpu {
    #[serde(default)]
    spdisplays_ndrvs: Vec<ProfilerDisplay>,
}

#[derive(Debug, Deserialize)]
struct ProfilerDisplay {
    spdisplays_connection_type: Option<String>,
    spdisplays_display_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERNAL: &str = r#"
    {
      "SPDisplaysDataType" : [
        {
          "spdisplays_ndrvs" : [
            {
              "_name" : "Color LCD",
              "spdisplays_connection_type" : "spdisplays_internal",
              "spdisplays_display_type" : "spdisplays_built-in-liquid-retina-xdr"
            }
          ]
        }
      ]
    }
    "#;

    const EXTERNAL: &str = r#"
    {
      "SPDisplaysDataType" : [
        {
          "spdisplays_ndrvs" : [
            {
              "_name" : "DELL U2723QE",
              "spdisplays_connection_type" : "spdisplays_displayport_dongle",
              "spdisplays_display_type" : "spdisplays_display"
            }
          ]
        }
      ]
    }
    "#;

    fn report(displays: &[&str]) -> String {
        format!(
            r#"{{"SPDisplaysDataType":[{{"spdisplays_ndrvs":[{}]}}]}}"#,
            displays.join(",")
        )
    }

    #[test]
    fn a_built_in_display_alone_is_the_laptop_condition() {
        assert_eq!(
            classify(INTERNAL),
            Some(DisplayCondition::Laptop),
            "the built-in-only report classifies as laptop"
        );
    }

    #[test]
    fn any_external_display_selects_the_external_condition() {
        assert_eq!(
            classify(&report(&[
                r#"{"spdisplays_connection_type":"spdisplays_internal","spdisplays_display_type":"spdisplays_built-in-liquid-retina-xdr"}"#,
                r#"{"spdisplays_connection_type":"spdisplays_displayport_dongle","spdisplays_display_type":"spdisplays_display"}"#,
            ])),
            Some(DisplayCondition::External)
        );
        // Clamshell: the built-in panel is absent and only the monitor remains.
        assert_eq!(classify(EXTERNAL), Some(DisplayCondition::External));
    }

    #[test]
    fn a_built_in_panel_is_not_external_even_with_an_odd_connection_type() {
        assert_eq!(
            classify(&report(&[
                r#"{"spdisplays_connection_type":"spdisplays_displayport","spdisplays_display_type":"spdisplays_built-in-liquid-retina-xdr"}"#,
            ])),
            Some(DisplayCondition::Laptop)
        );
    }

    #[test]
    fn an_unrecognised_display_is_not_external() {
        assert_eq!(
            classify(&report(&[r#"{"_name":"Mystery"}"#])),
            Some(DisplayCondition::Laptop)
        );
        // A report with no displays carries no signal at all.
        assert_eq!(classify(r#"{"SPDisplaysDataType":[]}"#), None);
        assert_eq!(classify("not json"), None);
    }
}
