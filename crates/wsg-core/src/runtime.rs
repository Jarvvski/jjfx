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

/// Typed inputs for one Agent Runtime invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeInvocation {
    prompt: String,
    model: Option<String>,
    session_id: Option<String>,
    name: Option<String>,
    system_prompt: Option<String>,
}

impl AgentRuntimeInvocation {
    /// Creates a fresh invocation with the required workload prompt.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: None,
            session_id: None,
            name: None,
            system_prompt: None,
        }
    }

    /// Adds a model override to the invocation.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Adds an Agent Session to resume.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Adds a display name for a fresh Claude invocation.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Adds a system prompt for a fresh invocation.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }
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

    /// Builds the provider command for a typed invocation.
    ///
    /// The returned command executes the provider directly and never routes
    /// caller-provided prompt text through a shell.
    pub fn command(
        self,
        invocation: &AgentRuntimeInvocation,
        capabilities: AgentRuntimeCapabilities,
    ) -> Command {
        let mut command = Command::new(self.as_str());
        if self == Self::Claude {
            command.arg("-p");
            if let Some(model) = invocation
                .model
                .as_deref()
                .filter(|model| !model.is_empty())
            {
                command.args(["--model", model]);
            }
            if let Some(session_id) = invocation
                .session_id
                .as_deref()
                .filter(|session_id| !session_id.is_empty())
            {
                command.args(["--resume", session_id, "--fork-session"]);
            }
            command.args(["--output-format", "stream-json", "--verbose"]);
            if capabilities.forward_subagent_text() {
                command.arg("--forward-subagent-text");
            }
            command.args(["--settings", r#"{"permissions":{"defaultMode":"auto"}}"#]);
            if let Some(name) = invocation.name.as_deref().filter(|name| !name.is_empty()) {
                command.args(["--name", name]);
            }
            if invocation
                .session_id
                .as_deref()
                .filter(|session_id| !session_id.is_empty())
                .is_none()
                && let Some(system_prompt) = invocation
                    .system_prompt
                    .as_deref()
                    .filter(|prompt| !prompt.is_empty())
            {
                command.args(["--append-system-prompt", system_prompt]);
            }
            command.arg(&invocation.prompt);
        } else {
            command.args([
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "never",
            ]);
            if let Some(model) = invocation
                .model
                .as_deref()
                .filter(|model| !model.is_empty())
            {
                command.args(["--model", model]);
            }
            if capabilities.multi_agent() {
                command.args(["--enable", "multi_agent"]);
            }
            command.arg("exec");
            if let Some(session_id) = invocation
                .session_id
                .as_deref()
                .filter(|session_id| !session_id.is_empty())
            {
                command.args(["resume", "--json", "--skip-git-repo-check", session_id]);
                command.arg(&invocation.prompt);
            } else {
                command.args(["--json", "--skip-git-repo-check"]);
                let prompt = match invocation
                    .system_prompt
                    .as_deref()
                    .filter(|prompt| !prompt.is_empty())
                {
                    Some(system_prompt) => format!("{system_prompt}\n\n{}", invocation.prompt),
                    None => invocation.prompt.clone(),
                };
                command.arg(prompt);
            }
        }
        command
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
    /// Creates capabilities supplied by a runtime probe or a caller-owned adapter.
    pub const fn new(multi_agent: bool, forward_subagent_text: bool) -> Self {
        Self {
            multi_agent,
            forward_subagent_text,
        }
    }

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
