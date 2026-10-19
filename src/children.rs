//! `jjfx children`: the child sessions a session dispatched, and what each is
//! doing.
//!
//! A parent reaches this from its own shell: opencode puts the session's id in
//! `OPENCODE_SESSION_ID`, so a bare `jjfx children` answers "what are my
//! children doing" for the session that ran it, and naming a session asks for
//! another one's children.
//!
//! The children are the union of what opencode reports (`session.list` filtered
//! by parent) and what the lifecycle log has seen; each child's state is the
//! fold's. opencode also supplies the title and directory, so a caller can tell
//! which Worker a child is - and the answer survives opencode being down, since
//! the log alone still names every child and its state.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::agent::{Agent, AgentStates};
use crate::engine::{OpenCodeApi, Session, dispatching_session};
use crate::events;

/// The one usage line, shared by every refusal this command makes.
const USAGE: &str = "Usage: jjfx children [<session>] [--json]";

/// Where the child list came from, so "opencode answered" and "only the log
/// did" never look alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// opencode listed the children; titles and directories are its.
    OpenCode,
    /// opencode was unreachable, so the lifecycle log named the children.
    Log,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::OpenCode => "opencode",
            Source::Log => "log",
        }
    }
}

/// One child session: the state the fold knows, plus whatever opencode adds.
#[derive(Debug, PartialEq, Eq)]
struct Child {
    session_id: String,
    agent: Agent,
    title: Option<String>,
    directory: Option<PathBuf>,
}

/// Dispatch `jjfx children`: the children of `session`, or of the session that
/// ran this command when none is named.
pub fn run(session: Option<&str>, json: bool) -> Result<()> {
    let parent = session
        .map(str::trim)
        .filter(|session| !session.is_empty())
        .map(str::to_owned)
        .or_else(dispatching_session)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no session named and {} is unset\n{USAGE}",
                crate::engine::OPENCODE_SESSION_ENV
            )
        })?;

    // The whole log, not a tail window: the fold needs a child's start to know
    // it is a child at all.
    let (events, _) = events::replay(&events::log_path());
    let states = AgentStates::replay(events);
    let (children, source) = collect(&parent, &states, &OpenCodeApi::new());

    let mut out = io::stdout().lock();
    if json {
        write_json(&mut out, &parent, &children, source)?;
    } else {
        write_prose(&mut out, &parent, &children, source)?;
    }
    Ok(())
}

/// The children of `parent`: what the fold tracks, enriched by what opencode
/// reports, plus any child opencode knows that the log has not seen.
fn collect(parent: &str, states: &AgentStates, api: &OpenCodeApi) -> (Vec<Child>, Source) {
    let mut children: BTreeMap<String, Child> = states
        .children_of(parent)
        .into_iter()
        .map(|child| {
            let session_id = child.session_id().to_owned();
            let child = Child {
                session_id: session_id.clone(),
                agent: child.agent(),
                title: None,
                directory: None,
            };
            (session_id, child)
        })
        .collect();

    let source = match api.child_sessions(parent) {
        Ok(sessions) => {
            for session in sessions {
                enrich(
                    children
                        .entry(session.id().to_owned())
                        .or_insert_with(|| Child {
                            session_id: session.id().to_owned(),
                            // opencode knows a child the log has no events for: its
                            // state is absent rather than guessed.
                            agent: Agent::default(),
                            title: None,
                            directory: None,
                        }),
                    &session,
                );
            }
            Source::OpenCode
        }
        Err(_) => Source::Log,
    };

    (children.into_values().collect(), source)
}

/// Copies opencode's metadata for one child onto its row.
fn enrich(child: &mut Child, session: &Session) {
    child.title = session.title().map(str::to_owned);
    child.directory = session.directory().map(Path::to_path_buf);
}

fn write_prose(
    out: &mut impl Write,
    parent: &str,
    children: &[Child],
    source: Source,
) -> io::Result<()> {
    if children.is_empty() {
        return writeln!(
            out,
            "no child sessions of {parent} (from {})",
            source.label()
        );
    }
    writeln!(
        out,
        "{} child session(s) of {parent} (from {})",
        children.len(),
        source.label()
    )?;
    for child in children {
        writeln!(
            out,
            "{:<15} {}  {}",
            child.agent.state,
            child.session_id,
            child.title.as_deref().unwrap_or("-")
        )?;
    }
    Ok(())
}

fn write_json(
    out: &mut impl Write,
    parent: &str,
    children: &[Child],
    source: Source,
) -> io::Result<()> {
    let children: Vec<_> = children
        .iter()
        .map(|child| {
            json!({
                "session": child.session_id.as_str(),
                "state": child.agent.state.to_string(),
                "title": child.title.as_deref(),
                "directory": child.directory.as_ref().map(|dir| dir.display().to_string()),
            })
        })
        .collect();
    writeln!(
        out,
        "{}",
        json!({ "parent": parent, "source": source.label(), "children": children })
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(session_id: &str, state: crate::agent::AgentState) -> Child {
        Child {
            session_id: session_id.to_owned(),
            agent: Agent {
                state,
                kind: crate::agent::AgentKind::OpenCode,
            },
            title: Some(format!("pool:worker:{session_id}")),
            directory: Some(PathBuf::from("/w")),
        }
    }

    #[test]
    fn prose_names_every_child_with_its_state() {
        let children = [
            child("ses_a", crate::agent::AgentState::Working),
            child("ses_b", crate::agent::AgentState::Waiting),
        ];
        let mut out = Vec::new();

        write_prose(&mut out, "ses_parent", &children, Source::OpenCode).expect("writes");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("ses_parent"), "{text}");
        assert!(text.contains("working"), "{text}");
        assert!(text.contains("waiting"), "{text}");
        assert!(text.contains("ses_a"), "{text}");
        assert!(text.contains("ses_b"), "{text}");
    }

    #[test]
    fn json_carries_the_parent_the_source_and_each_state() {
        let children = [child("ses_a", crate::agent::AgentState::Working)];
        let mut out = Vec::new();

        write_json(&mut out, "ses_parent", &children, Source::Log).expect("writes");

        let value: serde_json::Value =
            serde_json::from_slice(&out).expect("the report is valid JSON");
        assert_eq!(value["parent"], "ses_parent");
        assert_eq!(value["source"], "log");
        assert_eq!(value["children"][0]["session"], "ses_a");
        assert_eq!(value["children"][0]["state"], "working");
        assert_eq!(value["children"][0]["title"], "pool:worker:ses_a");
    }

    #[test]
    fn an_empty_child_list_still_names_the_parent_and_the_source() {
        let mut out = Vec::new();
        write_prose(&mut out, "ses_parent", &[], Source::OpenCode).expect("writes");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("no child sessions"), "{text}");
        assert!(text.contains("ses_parent"), "{text}");
    }
}
