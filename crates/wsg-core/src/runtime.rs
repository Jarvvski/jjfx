//! Agent Runtime identity and execution capability probing.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::Command;

use thiserror::Error;

use crate::WireAgent;

/// The Agent Runtime recorded for a Worker and selected for a Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    /// Anthropic's Claude Code runtime.
    Claude,
    /// OpenAI's Codex runtime.
    Codex,
}

impl fmt::Display for AgentRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AgentRuntime {
    pub(crate) fn parse(value: &WireAgent) -> Option<Self> {
        match value.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub(crate) fn from_configured(value: Option<&WireAgent>) -> Result<Self, String> {
        let configured = value.map_or("", WireAgent::as_str).trim();
        if configured.is_empty() {
            return Ok(Self::Claude);
        }
        match configured.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(configured.to_owned()),
        }
    }

    /// Returns the compatible persisted spelling for this runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Probes this runtime in `workspace`, requiring its executable to start.
    pub fn probe(
        self,
        workspace: impl AsRef<Path>,
    ) -> Result<AgentRuntimeCapabilities, AgentRuntimeProbeError> {
        let (arguments, capability) = match self {
            Self::Claude => (["--help"].as_slice(), Capability::ForwardSubagentText),
            Self::Codex => (["features", "list"].as_slice(), Capability::MultiAgent),
        };
        let output = Command::new(self.as_str())
            .args(arguments)
            .current_dir(workspace)
            .output()
            .map_err(|source| {
                if source.kind() == io::ErrorKind::NotFound {
                    AgentRuntimeProbeError::ExecutableNotFound { runtime: self }
                } else {
                    AgentRuntimeProbeError::Spawn {
                        runtime: self,
                        source,
                    }
                }
            })?;

        if !output.status.success() {
            return Ok(AgentRuntimeCapabilities::default());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let supported = match capability {
            Capability::ForwardSubagentText => stdout.contains("--forward-subagent-text"),
            Capability::MultiAgent => stdout
                .lines()
                .any(|line| line.split_whitespace().next() == Some("multi_agent")),
        };
        Ok(AgentRuntimeCapabilities::from(capability, supported))
    }
}

#[derive(Debug, Clone, Copy)]
enum Capability {
    MultiAgent,
    ForwardSubagentText,
}

/// Optional capabilities discovered from an Agent Runtime executable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentRuntimeCapabilities {
    multi_agent: bool,
    forward_subagent_text: bool,
}

impl AgentRuntimeCapabilities {
    fn from(capability: Capability, supported: bool) -> Self {
        match capability {
            Capability::MultiAgent => Self {
                multi_agent: supported,
                ..Self::default()
            },
            Capability::ForwardSubagentText => Self {
                forward_subagent_text: supported,
                ..Self::default()
            },
        }
    }

    /// Returns whether Codex exposes the multi-agent feature.
    pub fn multi_agent(self) -> bool {
        self.multi_agent
    }

    /// Returns whether Claude Code accepts forwarded subagent text.
    pub fn forward_subagent_text(self) -> bool {
        self.forward_subagent_text
    }
}

/// Errors that prevent an Agent Runtime capability probe from starting.
#[derive(Debug, Error)]
pub enum AgentRuntimeProbeError {
    /// The selected runtime executable is not available in `PATH`.
    #[error("{runtime} executable not found in PATH")]
    ExecutableNotFound { runtime: AgentRuntime },
    /// The selected runtime executable could not be started.
    #[error("cannot start {runtime} capability probe: {source}")]
    Spawn {
        runtime: AgentRuntime,
        #[source]
        source: io::Error,
    },
}
