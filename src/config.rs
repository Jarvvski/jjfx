//! jjfx's own settings. The user-global file is read once at startup; the
//! repository-local file (`<repo>/.jjfx/config.toml`) is read per repository,
//! for the settings whose correct value is a property of the repository rather
//! than the user. Both are distinct from jj config (read via the `jj` CLI) and
//! the event log. Absent file -> defaults; malformed file -> an error naming the
//! offending file, surfaced before the TUI takes over the screen.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeProfile, PaneLayout, PoolCapacity, SpendCap,
    SpendCapMode, SpendWindow, WireAgent,
};
use anyhow::Context;
use serde::Deserialize;

/// The whole jjfx config tree.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How jjfx launches the coding agent in each workspace.
    pub agent: AgentConfig,
    /// Named Agent Runtime Profiles, keyed by name. Defined here, user-globally,
    /// so a repository-local pool declaration can *reference* one without
    /// granting a capability it did not already have (ADR 0014).
    pub profiles: BTreeMap<String, ProfileConfig>,
    /// Which terminal instance hosts workspace sessions, and how to reach it.
    pub terminal: TerminalConfig,
    /// How the forge pipeline opens and maintains pull requests.
    pub forge: ForgeConfig,
    /// Where Dispatch discovers Ready Tickets, and the settings for the chosen
    /// source. Selected here rather than from the Agent Runtime, so a tracker
    /// that has no runtime-shaped path can still be configured.
    pub tickets: TicketsConfig,
    /// The optional cap on the aggregate spend Dispatch may run up.
    pub spend: SpendConfig,
    /// The optional ceiling on how many Runs Dispatch may keep in flight.
    pub dispatch: DispatchConfig,
    /// How long a Run may make no progress before the list surfaces it as stuck.
    pub watchdog: WatchdogConfig,
    /// Which AI reviewer the review loop runs, and how many re-review passes it
    /// makes. `reviewer = "none"` (the default) leaves review human-only.
    pub review: ReviewConfig,
    /// How the cross-workspace migration-order check finds migrations.
    pub migrations: MigrationsConfig,
    /// The repo-owned build check the forge runs against the welded tree before
    /// it pushes.
    pub checks: ChecksConfig,
    /// Whether new workspaces are colocated with Git: `jj workspace add` creates
    /// a Git worktree so each agent workspace has a working `.git`, which lets
    /// tools such as `gh stack` run inside it. On by default; set false to create
    /// non-colocated workspaces. A repository that is not itself colocated never
    /// colocates, whatever this says.
    pub colocate: bool,
    /// Legacy workspace settings are accepted and ignored after lifecycle hooks
    /// moved into the repository's `.jjfx` scripts.
    #[serde(rename = "workspace")]
    _legacy_workspace: Option<toml::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent: AgentConfig::default(),
            profiles: BTreeMap::new(),
            terminal: TerminalConfig::default(),
            forge: ForgeConfig::default(),
            tickets: TicketsConfig::default(),
            spend: SpendConfig::default(),
            dispatch: DispatchConfig::default(),
            watchdog: WatchdogConfig::default(),
            review: ReviewConfig::default(),
            migrations: MigrationsConfig::default(),
            checks: ChecksConfig::default(),
            // Colocation is on by default, matching jj 0.46's own default.
            colocate: true,
            _legacy_workspace: None,
        }
    }
}

impl Config {
    /// The command run in a workspace's agent pane, wrapped in the user's login
    /// interactive shell as `$SHELL -l -i -c <command>`. The shell sources
    /// login and interactive files, so configured aliases and the user's PATH
    /// are available. Falls back to `/bin/sh` when `$SHELL` is unset.
    pub fn agent_command(&self) -> Vec<String> {
        login_shell_command(&self.agent.command)
    }

    /// The command run in a workspace tab's first (top-left) pane, wrapped in
    /// the login interactive shell like [`Self::agent_command`] and followed by
    /// a fallback shell so the pane survives the command exiting. Empty when the
    /// setting is absent, leaving that pane a plain shell.
    pub fn first_pane_command(&self) -> Vec<String> {
        self.terminal
            .first_pane_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(|command| login_shell_command(&format!("{command}; exec \"$SHELL\"")))
            .unwrap_or_default()
    }

    /// Reject layouts and display mappings that cannot be built, so a bad
    /// config fails here rather than when a tab opens.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, profile) in &self.profiles {
            profile
                .validate()
                .map_err(|error| anyhow::anyhow!("profiles.{name}: {error}"))?;
        }
        for (name, layout) in &self.terminal.layouts {
            layout
                .validate()
                .map_err(|error| anyhow::anyhow!("terminal.layouts.{name}: {error}"))?;
        }
        for (condition, name) in [
            ("laptop", self.terminal.layout_by_display.laptop.as_deref()),
            (
                "external",
                self.terminal.layout_by_display.external.as_deref(),
            ),
            (
                "default",
                self.terminal.layout_by_display.fallback.as_deref(),
            ),
        ] {
            if let Some(name) = name
                && !self.terminal.layouts.contains_key(name)
            {
                anyhow::bail!(
                    "terminal.layout_by_display.{condition} names unknown layout `{name}`"
                );
            }
        }
        self.tickets.validate()?;
        self.spend.cap()?;
        self.dispatch.cap()?;
        self.review.validate()?;
        if self.migrations.pattern.trim().is_empty() {
            anyhow::bail!("migrations.pattern cannot be blank");
        }
        self.checks.validate()?;
        Ok(())
    }

    /// The build check the forge runs for `repo_root`: the repository's own
    /// `[checks]` when it sets one, otherwise the user-global value.
    ///
    /// `[checks]` is the one setting whose correct value belongs to the
    /// repository - its build command - and the user config is loaded once and
    /// applied to every repository jjfx is pointed at, so a check configured for
    /// one repository would otherwise run in all of them.
    pub fn checks_for(&self, repo_root: &Path) -> anyhow::Result<ChecksConfig> {
        Ok(self
            .repo_config(repo_root)?
            .checks
            .unwrap_or_else(|| self.checks.clone()))
    }

    /// The repository-local settings for `repo_root`, read and validated against
    /// this user-global config. The single entry point for the repository file,
    /// so `[checks]` and `[pools]` are read - and fail - together.
    pub fn repo_config(&self, repo_root: &Path) -> anyhow::Result<RepoConfig> {
        load_repo(repo_root, self)
    }

    /// The settings the TUI and Dispatch both act on, loaded once and handed
    /// together: where Ready Tickets come from, what Dispatch may spend, how many
    /// Runs it may keep in flight, and how long a Run may stall before the list
    /// surfaces it as stuck.
    pub fn dispatch_settings(&self) -> DispatchSettings {
        DispatchSettings {
            tickets: self.tickets.clone(),
            spend: self.spend,
            dispatch: self.dispatch,
            watchdog: self.watchdog,
        }
    }
}

/// The jjfx settings a frontend acts on, loaded once and handed together.
///
/// These are the settings Dispatch and the Worker Pool list both need, so they
/// travel as one value rather than as a growing argument list. Each carries its
/// own validation; [`Config::dispatch_settings`] only collects them.
#[derive(Debug, Clone, Default)]
pub struct DispatchSettings {
    /// Where Ready Tickets are discovered.
    pub tickets: TicketsConfig,
    /// The optional aggregate spend cap.
    pub spend: SpendConfig,
    /// The optional ceiling on concurrent Runs.
    pub dispatch: DispatchConfig,
    /// How long a Run may make no progress before the watchdog flags it.
    pub watchdog: WatchdogConfig,
}

/// Wrap a shell command in `$SHELL -l -i -c <command>` so login and interactive
/// files have run before it starts (aliases and the user's PATH are available).
fn login_shell_command(command: &str) -> Vec<String> {
    vec![
        login_shell_program(),
        "-l".to_string(),
        "-i".to_string(),
        "-c".to_string(),
        command.to_string(),
    ]
}

/// The user's login shell program, or `/bin/sh` when `$SHELL` is unset.
fn login_shell_program() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// How jjfx launches the coding agent in a workspace.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// The shell command run in a workspace's agent pane. Because it runs in the
    /// user's login interactive shell, this may name an alias such as `cx`.
    pub command: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
        }
    }
}

/// A named Agent Runtime Profile: the runtime, and the provider and model its
/// Runs launch with.
///
/// Defined user-globally so a repository-local pool declaration can *reference*
/// one by name. The capability rule (ADR 0014) is what puts it here: a
/// repository-local file may select among the profiles the user granted, and
/// may never define a runtime, a model or credentials of its own.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    /// The Agent Runtime: `claude`, `codex`, or `opencode`.
    pub runtime: String,
    /// The provider identifier the runtime needs alongside the model, e.g.
    /// `anthropic`. Optional.
    #[serde(default)]
    pub provider: Option<String>,
    /// The model identifier, e.g. `claude-sonnet-4-20250514`. Optional, leaving
    /// the runtime to pick its own default model.
    #[serde(default)]
    pub model: Option<String>,
}

impl ProfileConfig {
    /// The Agent Runtime Profile this config describes.
    pub fn profile(&self) -> anyhow::Result<AgentRuntimeProfile> {
        let runtime = AgentRuntime::from_configured(Some(&WireAgent::new(self.runtime.trim())))
            .map_err(|message| anyhow::anyhow!("{message}"))?;
        let mut profile = AgentRuntimeProfile::new(runtime);
        if let Some(model) = non_blank(self.model.as_deref()) {
            let mut selection = AgentModel::new(model);
            if let Some(provider) = non_blank(self.provider.as_deref()) {
                selection = selection.with_provider(provider);
            }
            profile = profile.with_model(selection);
        }
        Ok(profile)
    }

    /// Reject a profile a Run cannot launch with, so a bad config fails at
    /// startup rather than mid-Dispatch.
    fn validate(&self) -> anyhow::Result<()> {
        if self.runtime.trim().is_empty() {
            anyhow::bail!("runtime cannot be blank");
        }
        for (key, value) in [
            ("provider", self.provider.as_deref()),
            ("model", self.model.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                anyhow::bail!("{key} cannot be blank; remove it instead");
            }
        }
        if non_blank(self.provider.as_deref()).is_some()
            && non_blank(self.model.as_deref()).is_none()
        {
            anyhow::bail!("provider requires model");
        }
        self.profile()?;
        Ok(())
    }
}

/// `value` trimmed, or `None` when it is absent or blank.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// How the forge's final step manages pull requests. jjfx submits PRs natively
/// over `gh` (ADR 0007) - no third-party CLI - and these settings gate it.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeConfig {
    /// Whether the forge creates/updates PRs at all. On by default; set false to
    /// stop the pipeline after push and open PRs yourself.
    pub pull_requests: bool,
    /// Open newly-created PRs as drafts. On by default; set false to open them
    /// ready for review.
    pub draft: bool,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        // Both default on: forging a workspace opens a draft PR out of the box.
        ForgeConfig {
            pull_requests: true,
            draft: true,
        }
    }
}

/// Where `dispatch --all` and orchestration discover Ready Tickets, and the
/// settings for the chosen source.
///
/// The source is a config decision, not a function of the Agent Runtime: a
/// runtime with no native tracker can still pull work through a configured
/// source, and a new tracker needs no runtime-shaped path. `source` is required
/// when `[tickets]` is present; an absent `[tickets]` section means the default,
/// `linear`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketsConfig {
    /// Which tracker provides Ready Tickets.
    pub source: TicketSource,
    /// Settings used when `source = "linear"`.
    #[serde(default)]
    pub linear: LinearTicketsConfig,
    /// Settings used when `source = "local"`.
    #[serde(default)]
    pub local: LocalTicketsConfig,
}

impl TicketsConfig {
    /// The workflow status a Ready Ticket must be in for the configured source.
    pub fn ready_status(&self) -> &str {
        match self.source {
            TicketSource::Linear => &self.linear.status,
            TicketSource::Local => &self.local.ready_status,
        }
    }

    /// Reject settings a source cannot use, so a bad config fails here rather
    /// than mid-Dispatch.
    fn validate(&self) -> anyhow::Result<()> {
        for (name, value) in [
            ("tickets.linear.status", self.linear.status.as_str()),
            ("tickets.local.root", self.local.root.as_str()),
            (
                "tickets.local.ready_status",
                self.local.ready_status.as_str(),
            ),
            (
                "tickets.local.resolved_status",
                self.local.resolved_status.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                anyhow::bail!("{name} cannot be blank");
            }
        }
        Ok(())
    }
}

impl Default for TicketsConfig {
    fn default() -> Self {
        Self {
            source: TicketSource::Linear,
            linear: LinearTicketsConfig::default(),
            local: LocalTicketsConfig::default(),
        }
    }
}

/// The configured Ticket source. Selected by config, so it is independent of
/// the Agent Runtime a Worker Pool happens to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketSource {
    /// Discover through the Agent Runtime's native Linear access (Claude, Codex
    /// or opencode).
    Linear,
    /// Discover from local markdown under the repository.
    Local,
}

/// Settings for `source = "linear"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LinearTicketsConfig {
    /// The Linear workflow status a Ready Ticket must be in.
    pub status: String,
}

impl Default for LinearTicketsConfig {
    fn default() -> Self {
        Self {
            status: "Todo".to_owned(),
        }
    }
}

/// Settings for `source = "local"`, the repository's local markdown tickets.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalTicketsConfig {
    /// Directory under the repository root holding `<feature>/issues/*.md`.
    pub root: String,
    /// The `Status:` value marking a Ticket ready for Dispatch.
    pub ready_status: String,
    /// The `Status:` value marking a Ticket resolved.
    pub resolved_status: String,
}

impl Default for LocalTicketsConfig {
    fn default() -> Self {
        Self {
            root: ".scratch".to_owned(),
            ready_status: "ready-for-agent".to_owned(),
            resolved_status: "resolved".to_owned(),
        }
    }
}

/// An optional cap on the aggregate spend Dispatch may run up.
///
/// The aggregate is the sum of the durable per-Run cost records jjfx writes at
/// finalization, so the cap is enforced from what Runs actually cost rather than
/// from a per-Run hint. Absent `cap_usd` (the default) means no cap and no gate.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpendConfig {
    /// The cap in US dollars. Absent means no cap.
    pub cap_usd: Option<f64>,
    /// The window the cap totals over. Defaults to `week`, the last seven days.
    pub window: SpendWindow,
    /// What Dispatch does once the aggregate reaches the cap. Defaults to
    /// `block`; `warn` lets Dispatch proceed and reports the overage.
    pub on_exceed: SpendCapMode,
}

impl SpendConfig {
    /// The engine-level cap this config describes, or `None` when no cap is set.
    ///
    /// A cap that is not a positive, finite dollar amount is rejected, so a bad
    /// config fails at startup rather than mid-Dispatch.
    pub fn cap(&self) -> anyhow::Result<Option<SpendCap>> {
        let Some(dollars) = self.cap_usd else {
            return Ok(None);
        };
        if !dollars.is_finite() || dollars <= 0.0 {
            anyhow::bail!("spend.cap_usd must be a positive dollar amount");
        }
        let micro_usd = dollars * 1_000_000.0;
        if micro_usd > u64::MAX as f64 {
            anyhow::bail!("spend.cap_usd is too large");
        }
        Ok(Some(SpendCap::new(
            micro_usd.round() as u64,
            self.window,
            self.on_exceed,
        )))
    }
}

impl Default for SpendConfig {
    fn default() -> Self {
        Self {
            cap_usd: None,
            window: SpendWindow::Week,
            on_exceed: SpendCapMode::Block,
        }
    }
}

/// An optional ceiling on how many Runs Dispatch may keep in flight at once.
///
/// Absent `max_concurrent` (the default) is no cap: a Dispatch fires its whole
/// batch, exactly as it did before the governor existed.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DispatchConfig {
    /// The most Runs that may be in flight at once. Absent means no cap.
    pub max_concurrent: Option<usize>,
}

impl DispatchConfig {
    /// The engine-level cap this config describes, or `None` when there is none.
    ///
    /// A cap below one Run is rejected, so a bad config fails at startup rather
    /// than holding every Dispatch back forever.
    pub fn cap(&self) -> anyhow::Result<Option<crate::engine::ConcurrencyCap>> {
        let Some(limit) = self.max_concurrent else {
            return Ok(None);
        };
        crate::engine::ConcurrencyCap::new(limit)
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("dispatch.max_concurrent must be at least 1"))
    }
}

/// How long a Run may make no progress before the watchdog flags it as stuck.
///
/// The window is a duration rather than a switch so the same setting both tunes
/// the signal and turns it off: `stuck_after_secs = 0` disables the watchdog
/// entirely, leaving the list exactly as it was before the watchdog existed.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WatchdogConfig {
    /// Seconds of no Run-log activity before a busy Run is surfaced as stuck.
    /// Zero disables the watchdog. Defaults to thirty minutes.
    pub stuck_after_secs: u64,
}

impl WatchdogConfig {
    /// The engine-level window this config describes.
    pub fn stuck_window(&self) -> crate::engine::StuckWindow {
        crate::engine::StuckWindow::from_secs(self.stuck_after_secs)
    }
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            stuck_after_secs: crate::engine::StuckWindow::DEFAULT.as_duration().as_secs(),
        }
    }
}

/// The AI reviewer the review loop runs, and how far it iterates.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewConfig {
    /// Which reviewer produces findings. `none` (the default) keeps the review
    /// verb's human-only behaviour: nothing is run and the prompt is unchanged.
    pub reviewer: ReviewerKind,
    /// The pass cap for the re-review loop: the most reviewer passes jjfx runs
    /// before it stops. One pass reviews and addresses once.
    pub max_passes: usize,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            reviewer: ReviewerKind::None,
            max_passes: 2,
        }
    }
}

impl ReviewConfig {
    /// Reject settings the review loop cannot run, so a bad config fails here
    /// rather than mid-review.
    fn validate(&self) -> anyhow::Result<()> {
        if self.max_passes == 0 {
            anyhow::bail!("review.max_passes must be at least 1");
        }
        Ok(())
    }
}

/// The configured AI reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ReviewerKind {
    /// No reviewer: `review` behaves exactly as it did before reviewers existed.
    #[serde(rename = "none")]
    None,
    /// The CodeRabbit CLI in agent mode (`cr review --agent`).
    #[serde(rename = "coderabbit")]
    Coderabbit,
    /// The opencode GitHub Action, run repo-side.
    #[serde(rename = "opencode-action")]
    OpenCodeAction,
}

/// How the cross-workspace migration-order check (ticket 17) finds migrations.
///
/// Parallel Workers each edit their own workspace, and an ordered-migration
/// project can be individually correct per Pull Request yet wrong as a set: two
/// workspaces adding `V189` and `V190` land out of order if the higher version
/// merges first, which Flyway rejects on the next deploy when `outOfOrder` is
/// unset. The check reads each workspace's working copy for files matching
/// `pattern`, so the pattern is a setting rather than a hard-coded Flyway shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MigrationsConfig {
    /// The glob a migration file's repo-relative path must match. Defaults to
    /// Flyway's `V<version>__<description>.sql` shape; set it to your own layout
    /// so a non-Flyway project is not forced into that shape. `**` matches zero
    /// or more path segments, `*` any run of characters within one, `?` one.
    pub pattern: String,
}

impl Default for MigrationsConfig {
    fn default() -> Self {
        Self {
            pattern: "**/V*__*.sql".to_owned(),
        }
    }
}

/// The repo-owned build check the forge runs against the welded tree, before it
/// pushes (ticket 18).
///
/// A Run is `done` purely from the agent runtime's own terminal event, so the
/// whole "this ticket is finished" signal is the agent's report that it is. Two
/// changes can each be green, mergeable and at zero open threads while not
/// compiling together - both calling a member a third, still-open change adds.
/// The forge welds onto trunk before it pushes, so a check run there sees what
/// `main` will actually be, which a post-Run hook on the un-welded tree would
/// miss. jjfx never guesses the command: an unconfigured repo is a no-op, not a
/// default.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChecksConfig {
    /// The command the repository owns for a build check - `cargo check`,
    /// `./gradlew :app:backend:compileKotlin`, `bun run build`. Run through the
    /// user's shell, in the workspace, so the repo decides the toolchain. `None`
    /// (the default) means no check: the forge behaves exactly as before.
    pub command: Option<String>,
    /// Whether the check runs. On by default; set false to keep the command
    /// configured but skip the check.
    pub enabled: bool,
    /// How long the check may run before jjfx kills it and fails the step. A
    /// build that never finishes must not hold the forge open without bound.
    pub timeout_secs: u64,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            command: None,
            enabled: true,
            timeout_secs: DEFAULT_CHECK_TIMEOUT_SECS,
        }
    }
}

impl ChecksConfig {
    /// The command to run, or `None` when no check is configured or it is
    /// disabled - the forge's no-op.
    pub fn command(&self) -> Option<&str> {
        if !self.enabled {
            return None;
        }
        self.command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
    }

    /// Reject settings the check cannot run, so a bad config fails at startup
    /// rather than mid-forge.
    fn validate(&self) -> anyhow::Result<()> {
        if self.timeout_secs == 0 {
            anyhow::bail!("checks.timeout_secs must be at least 1");
        }
        if self
            .command
            .as_deref()
            .is_some_and(|command| command.trim().is_empty())
        {
            anyhow::bail!("checks.command cannot be blank; remove it to disable the check");
        }
        Ok(())
    }
}

/// Ten minutes: long enough for a cold full build, short enough that a wedged
/// check does not hold the forge open indefinitely.
const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 600;

/// Where jjfx opens workspace session tabs. Empty (the default) drives the kitty
/// jjfx runs inside, matching pre-config behaviour.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// The target kitty's `listen_on` *base* value, e.g. `unix:/tmp/kitty-visor`
    /// (exactly what you pass kitty, not the live socket). kitty appends `-<pid>`
    /// to a unix path, so jjfx resolves the actual `/tmp/kitty-visor-<pid>` at
    /// call time and routes every `kitten @` there via `--to`. `None` uses the
    /// inherited `KITTY_LISTEN_ON` (the kitty jjfx itself runs in).
    pub listen_on: Option<String>,
    /// Command jjfx runs to launch the target when its socket isn't found - the
    /// program followed by its arguments, e.g.
    /// `["/Applications/Visor.app/Contents/MacOS/kitty", "--detach", "-o",
    /// "allow_remote_control=yes", "-o", "listen_on=unix:/tmp/kitty-visor"]`.
    /// It should detach (return promptly) - jjfx then polls `listen_on` until
    /// the instance answers. Empty (the default) never auto-launches; jjfx just
    /// reports the target as not running.
    #[serde(default)]
    pub launch_command: Vec<String>,
    /// Shell command run in an opened tab's first (top-left) pane - e.g. a dev
    /// server or watcher. It runs in the login interactive shell like
    /// `agent.command`, and the pane drops back to a shell when the command
    /// exits. Absent (the default) leaves that pane a plain shell; the second
    /// left pane and the agent pane are unaffected.
    #[serde(default)]
    pub first_pane_command: Option<String>,
    /// Named pane layouts opened tabs can use, keyed by name.
    #[serde(default)]
    pub layouts: BTreeMap<String, PaneLayout>,
    /// Which named layout jjfx builds for each display condition.
    #[serde(default)]
    pub layout_by_display: DisplayLayouts,
}

/// Which named layout jjfx builds for each display condition. An unmapped
/// condition falls back to `default`, then to the built-in layout.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayLayouts {
    /// Used when only the built-in display is attached.
    pub laptop: Option<String>,
    /// Used when at least one external display is attached.
    pub external: Option<String>,
    /// Used when detection is unavailable or a condition has no entry.
    #[serde(rename = "default")]
    pub fallback: Option<String>,
}

/// `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml` - the same XDG convention as
/// the event log's state path. `None` when neither variable names a base
/// directory: with no base there is no user config, and a relative fallback
/// would resolve against the process CWD (letting a repository plant it).
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("jjfx").join("config.toml"))
}

/// Load the config from its fixed path. A missing file yields defaults; a file
/// that exists but fails to parse is an error (named so the user can find it).
pub fn load() -> anyhow::Result<Config> {
    match config_path() {
        Some(path) => load_from(&path),
        None => Ok(Config::default()),
    }
}

/// The parse step, split out so tests can drive it from a fixture path.
fn load_from(path: &std::path::Path) -> anyhow::Result<Config> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("reading config {}", path.display())),
    };
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
    config
        .validate()
        .with_context(|| format!("validating config {}", path.display()))?;
    Ok(config)
}

/// A repository's own settings, from `<repo>/.jjfx/config.toml` - alongside the
/// lifecycle hooks, because a value that is a property of the repository cannot
/// live in a file read once for every repository.
///
/// `[checks]` and `[pools]` are the repo-owned sections. `deny_unknown_fields` is
/// deliberate: a repository must not be able to silently configure jjfx beyond
/// its build check and its Worker Pools, so any other section is a loud error
/// rather than a quiet ignore. A pool may only *select* - reference a user-global
/// Agent Runtime Profile and Ticket source, set a size, cap a spend, name a
/// routing label - and `deny_unknown_fields` on [`PoolConfig`] is what makes an
/// attempt to *define* an agent command, a model or credentials a loud error.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    /// The repository's build check, replacing the user-global `[checks]`
    /// wholesale when present. `None` (no `[checks]` section) leaves the
    /// user-global value in place.
    pub checks: Option<ChecksConfig>,
    /// One declaration per named Worker Pool, keyed by name. Empty (the default)
    /// means the Repository's single implicit `default` pool and today's
    /// behaviour, unchanged.
    pub pools: BTreeMap<String, PoolConfig>,
}

impl RepoConfig {
    /// Reject repository-local settings that cannot be honoured, naming the
    /// offending pool. Takes the user-global config so a declaration can be
    /// checked against the profiles and sources it may only reference.
    fn validate(&self, user: &Config) -> anyhow::Result<()> {
        if let Some(checks) = &self.checks {
            checks.validate()?;
        }
        let mut labels: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, pool) in &self.pools {
            pool.validate(name, user)?;
            if let Some(label) = pool.label.as_deref()
                && let Some(previous) = labels.insert(label, name)
            {
                anyhow::bail!(
                    "pools `{previous}` and `{name}` both declare the routing label `{label}`"
                );
            }
        }
        Ok(())
    }
}

/// One repository-local Worker Pool declaration, from `[pools.<name>]`.
///
/// Every field is optional but the name (the table key). A declaration may
/// **select, not grant** (ADR 0014): it references an Agent Runtime Profile and
/// a Ticket source the user-global config defines, and sets the pool's size,
/// spend cap, routing label and ready status. It can never define an agent
/// command, a model, credentials or a new source kind - `deny_unknown_fields`
/// makes such a key a loud error naming the file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolConfig {
    /// The number of Workers the pool holds. Absent leaves the Repository's
    /// configured size in place.
    pub size: Option<usize>,
    /// The name of an Agent Runtime Profile the user-global `[profiles]`
    /// defines. Absent leaves the Repository default in place.
    pub profile: Option<String>,
    /// Which configured Ticket source discovers this pool's work. Absent means a
    /// pool with no Ticket source, reachable only by an explicit `--pool`.
    pub source: Option<TicketSource>,
    /// The Ticket label that routes a Ticket to this pool. Absent means the pool
    /// is reachable only by an explicit `--pool`.
    pub label: Option<String>,
    /// The `Status:` value marking a Ticket ready for this pool, defaulting to
    /// the user-global value.
    pub ready_status: Option<String>,
    /// The pool's spend cap in US dollars. Absent means the global ceiling is
    /// the only limit.
    pub cap_usd: Option<f64>,
    /// The window the pool's cap totals over. Absent defaults to the global
    /// `spend.window`.
    pub window: Option<SpendWindow>,
    /// What Dispatch does once the pool's cap is reached. Absent defaults to the
    /// global `spend.on_exceed`.
    pub on_exceed: Option<SpendCapMode>,
}

impl PoolConfig {
    /// The spend cap this declaration describes, filling `window` and
    /// `on_exceed` from the user-global `[spend]` when the pool omits them.
    ///
    /// A cap that is not a positive, finite dollar amount is rejected, so a bad
    /// declaration fails at load rather than mid-Dispatch.
    pub fn spend_cap(&self, global: &SpendConfig) -> anyhow::Result<Option<SpendCap>> {
        SpendConfig {
            cap_usd: self.cap_usd,
            window: self.window.unwrap_or(global.window),
            on_exceed: self.on_exceed.unwrap_or(global.on_exceed),
        }
        .cap()
    }

    /// Reject a declaration that cannot be honoured, naming the pool.
    fn validate(&self, name: &str, user: &Config) -> anyhow::Result<()> {
        if let Some(size) = self.size {
            PoolCapacity::new(size).map_err(|error| anyhow::anyhow!("pool `{name}`: {error}"))?;
        }
        if let Some(profile) = self.profile.as_deref() {
            if profile.trim().is_empty() {
                anyhow::bail!("pool `{name}` profile cannot be blank");
            }
            if !user.profiles.contains_key(profile) {
                anyhow::bail!(
                    "pool `{name}` names Agent Runtime Profile `{profile}`, which the user \
                     config does not define"
                );
            }
        }
        for (key, value) in [
            ("label", self.label.as_deref()),
            ("ready_status", self.ready_status.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                anyhow::bail!("pool `{name}` {key} cannot be blank");
            }
        }
        self.spend_cap(&user.spend)
            .map_err(|error| anyhow::anyhow!("pool `{name}`: {error}"))?;
        Ok(())
    }
}

/// `<repo>/.jjfx/config.toml` - the repository-local settings file, next to the
/// `setup.sh` / `teardown.sh` lifecycle hooks.
pub fn repo_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".jjfx").join("config.toml")
}

/// Read the repository-local settings. A missing file is no overrides; a file
/// that exists but fails to parse, names a section a repository may not set, or
/// holds invalid values is an error naming the file - the same loud-failure rule
/// as the user-global config. The user-global config is passed in so a pool
/// declaration can be checked against the profiles it may only reference.
pub fn load_repo(repo_root: &Path, user: &Config) -> anyhow::Result<RepoConfig> {
    let path = repo_config_path(repo_root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RepoConfig::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading config {}", path.display()));
        }
    };
    let config: RepoConfig = toml::from_str(&text).with_context(|| {
        format!(
            "parsing config {} - a repository may set only [checks] and [pools]; a pool may \
             select a user-global profile and Ticket source, never define one",
            path.display()
        )
    })?;
    config
        .validate(user)
        .with_context(|| format!("validating config {}", path.display()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        /// Parses a config from TOML, for tests that do not want to write a file.
        fn from_toml(text: &str) -> Self {
            toml::from_str(text).expect("test TOML parses")
        }
    }

    #[test]
    fn empty_config_is_all_defaults() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert!(cfg.terminal.listen_on.is_none());
        assert!(cfg.terminal.launch_command.is_empty());
        assert!(cfg.terminal.first_pane_command.is_none());
        assert!(cfg.terminal.layouts.is_empty());
        assert!(cfg.terminal.layout_by_display.laptop.is_none());
        assert!(cfg.terminal.layout_by_display.external.is_none());
        assert!(cfg.terminal.layout_by_display.fallback.is_none());
        assert_eq!(cfg.agent.command, "claude");
        // Forge PR management is on-by-default, drafts on-by-default.
        assert!(cfg.forge.pull_requests);
        assert!(cfg.forge.draft);
        // Workspace colocation is on-by-default.
        assert!(cfg.colocate);
    }

    #[test]
    fn colocation_can_be_disabled() {
        let cfg: Config = toml::from_str("colocate = false\n").expect("toml parses");
        assert!(!cfg.colocate);
    }

    #[test]
    fn migrations_default_to_the_flyway_pattern() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert_eq!(cfg.migrations.pattern, "**/V*__*.sql");
    }

    #[test]
    fn migrations_pattern_is_configurable_and_cannot_be_blank() {
        let cfg: Config =
            toml::from_str("[migrations]\npattern = \"db/migrate/*.sql\"\n").expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.migrations.pattern, "db/migrate/*.sql");

        let blank: Config =
            toml::from_str("[migrations]\npattern = \"  \"\n").expect("toml parses");
        assert!(blank.validate().is_err());
    }

    #[test]
    fn checks_are_unconfigured_by_default() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert_eq!(cfg.checks.command, None);
        assert!(cfg.checks.enabled);
        assert_eq!(cfg.checks.timeout_secs, 600);
        cfg.validate().expect("the default config is valid");
    }

    #[test]
    fn checks_are_configurable() {
        let cfg: Config =
            toml::from_str("[checks]\ncommand = \"cargo check\"\ntimeout_secs = 120\n")
                .expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.checks.command.as_deref(), Some("cargo check"));
        assert_eq!(cfg.checks.timeout_secs, 120);
    }

    #[test]
    fn an_unknown_checks_key_is_rejected() {
        let error = toml::from_str::<Config>("[checks]\ncomand = \"cargo check\"\n")
            .expect_err("deny_unknown_fields must reject a typo");
        assert!(error.to_string().contains("comand"), "{error}");
    }

    #[test]
    fn a_blank_check_command_and_a_zero_timeout_are_rejected() {
        let blank: Config = toml::from_str("[checks]\ncommand = \"  \"\n").expect("toml parses");
        assert!(blank.validate().is_err());

        let zero: Config = toml::from_str("[checks]\ntimeout_secs = 0\n").expect("toml parses");
        assert!(zero.validate().is_err());
    }

    /// A repository directory carrying a `.jjfx/config.toml`, or none at all when
    /// `text` is `None`, for the repository-local precedence tests.
    fn repo_with_config(text: Option<&str>) -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("a temporary repository");
        if let Some(text) = text {
            let dir = repo.path().join(".jjfx");
            fs::create_dir_all(&dir).expect("creating .jjfx");
            fs::write(dir.join("config.toml"), text).expect("writing the repository config");
        }
        repo
    }

    #[test]
    fn a_repository_check_overrides_the_user_global_one() {
        let config = Config::from_toml("[checks]\ncommand = \"cargo check\"\n");
        let repo = repo_with_config(Some(
            "[checks]\ncommand = \"./gradlew :app:backend:compileKotlin\"\ntimeout_secs = 1200\n",
        ));
        let checks = config
            .checks_for(repo.path())
            .expect("the repository config parses");
        assert_eq!(
            checks.command.as_deref(),
            Some("./gradlew :app:backend:compileKotlin")
        );
        assert_eq!(checks.timeout_secs, 1200);
    }

    #[test]
    fn the_user_global_check_applies_when_the_repository_sets_none() {
        let config = Config::from_toml("[checks]\ncommand = \"cargo check\"\n");
        // No repository file at all, and one that sets nothing (a comment only -
        // any section other than `[checks]` and `[pools]` is rejected, not
        // ignored).
        for repo in [
            repo_with_config(None),
            repo_with_config(Some("# nothing repo-owned here yet\n")),
        ] {
            let checks = config
                .checks_for(repo.path())
                .expect("no repository override");
            assert_eq!(checks.command.as_deref(), Some("cargo check"));
        }
    }

    #[test]
    fn an_unconfigured_repository_has_no_check() {
        let config = Config::from_toml("");
        let repo = repo_with_config(None);
        let checks = config
            .checks_for(repo.path())
            .expect("no repository override");
        assert_eq!(checks.command(), None);
    }

    #[test]
    fn a_repository_may_disable_a_user_global_check() {
        let config = Config::from_toml("[checks]\ncommand = \"cargo check\"\n");
        let repo = repo_with_config(Some("[checks]\nenabled = false\n"));
        let checks = config
            .checks_for(repo.path())
            .expect("the repository config parses");
        assert_eq!(checks.command(), None);
    }

    #[test]
    fn an_invalid_repository_config_is_an_error_naming_the_file() {
        let config = Config::from_toml("");

        // `[checks]` and `[pools]` are the repo-owned sections, so a repository
        // cannot reconfigure the agent, the terminal, or anything else.
        let repo = repo_with_config(Some("[agent]\ncommand = \"rm -rf /\"\n"));
        let error = config
            .checks_for(repo.path())
            .expect_err("an unknown section must be rejected");
        let message = format!("{error:?}");
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("agent"), "{message}");

        // A typo inside the section it may set.
        let repo = repo_with_config(Some("[checks]\ncomand = \"cargo check\"\n"));
        let error = config
            .checks_for(repo.path())
            .expect_err("an unknown checks key must be rejected");
        assert!(format!("{error:?}").contains("comand"), "{error:?}");

        // A value the check cannot run with.
        let repo = repo_with_config(Some("[checks]\ncommand = \"  \"\n"));
        let error = config
            .checks_for(repo.path())
            .expect_err("a blank command must be rejected");
        assert!(format!("{error:?}").contains("checks.command"), "{error:?}");
    }

    #[test]
    fn profiles_are_empty_by_default() {
        let cfg = Config::default();
        cfg.validate().expect("the default config is valid");
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn a_profile_defines_a_named_agent_runtime_profile() {
        let cfg: Config = toml::from_str(
            r#"
            [profiles.fast]
            runtime = "claude"
            provider = "anthropic"
            model = "claude-sonnet-4-20250514"
            "#,
        )
        .expect("toml parses");
        cfg.validate().expect("the config is valid");
        let profile = cfg.profiles["fast"].profile().expect("a valid profile");
        assert_eq!(profile.runtime(), AgentRuntime::Claude);
        let model = profile.model().expect("a model");
        assert_eq!(model.provider(), Some("anthropic"));
        assert_eq!(model.model(), "claude-sonnet-4-20250514");
    }

    #[test]
    fn a_profile_may_leave_provider_and_model_unset() {
        let cfg: Config =
            toml::from_str("[profiles.fast]\nruntime = \"codex\"\n").expect("toml parses");
        cfg.validate().expect("the config is valid");
        let profile = cfg.profiles["fast"].profile().expect("a valid profile");
        assert_eq!(profile.runtime(), AgentRuntime::Codex);
        assert!(profile.model().is_none());
    }

    #[test]
    fn an_unknown_profile_runtime_is_rejected() {
        let cfg: Config =
            toml::from_str("[profiles.fast]\nruntime = \"gpt5\"\n").expect("toml parses");
        let error = cfg.validate().expect_err("an unknown runtime is an error");
        assert!(error.to_string().contains("profiles.fast"), "{error}");
        assert!(error.to_string().contains("gpt5"), "{error}");
    }

    #[test]
    fn a_profile_provider_without_a_model_is_rejected() {
        let cfg: Config =
            toml::from_str("[profiles.fast]\nruntime = \"claude\"\nprovider = \"anthropic\"\n")
                .expect("toml parses");
        let error = cfg
            .validate()
            .expect_err("provider without a model is an error");
        assert!(error.to_string().contains("profiles.fast"), "{error}");
        assert!(error.to_string().contains("provider"), "{error}");
    }

    #[test]
    fn unknown_profile_keys_are_rejected() {
        let err = toml::from_str::<Config>("[profiles.fast]\nruntim = \"claude\"\n")
            .expect_err("a typo is an error");
        assert!(err.to_string().contains("runtim"), "{err}");
    }

    #[test]
    fn blank_profile_fields_are_rejected() {
        for text in [
            "[profiles.fast]\nruntime = \"  \"\n",
            "[profiles.fast]\nruntime = \"claude\"\nmodel = \"  \"\n",
        ] {
            let cfg: Config = toml::from_str(text).expect("toml parses");
            let error = cfg.validate().expect_err("a blank field is an error");
            assert!(
                error.to_string().contains("profiles.fast"),
                "{text:?}: {error}"
            );
        }
    }

    #[test]
    fn an_absent_pools_section_declares_no_pools() {
        // The additive stance: no `[pools]` is the single implicit `default`
        // pool and today's behaviour, so the declaration set is simply empty.
        let config = Config::from_toml("");
        for repo in [
            repo_with_config(None),
            repo_with_config(Some("# nothing repo-owned here yet\n")),
        ] {
            let repo_config = config
                .repo_config(repo.path())
                .expect("no repository override");
            assert!(repo_config.pools.is_empty());
        }
    }

    #[test]
    fn a_pools_section_parses_with_every_field_optional_but_the_name() {
        let config = Config::from_toml(
            r#"
            [profiles.fast]
            runtime = "claude"
            "#,
        );
        let repo = repo_with_config(Some(
            r#"
            [pools.bugs]
            size = 2
            profile = "fast"
            source = "local"
            label = "bug"
            ready_status = "ready-for-agent"
            cap_usd = 5.0
            window = "week"
            on_exceed = "block"

            [pools.review]
            "#,
        ));
        let repo_config = config
            .repo_config(repo.path())
            .expect("the repository config parses");
        assert_eq!(repo_config.pools.len(), 2);
        let bugs = &repo_config.pools["bugs"];
        assert_eq!(bugs.size, Some(2));
        assert_eq!(bugs.profile.as_deref(), Some("fast"));
        assert_eq!(bugs.source, Some(TicketSource::Local));
        assert_eq!(bugs.label.as_deref(), Some("bug"));
        assert_eq!(bugs.ready_status.as_deref(), Some("ready-for-agent"));
        let cap = bugs
            .spend_cap(&config.spend)
            .expect("valid")
            .expect("a cap");
        assert_eq!(cap.limit_micro_usd(), 5_000_000);
        // A bare `[pools.<name>]` is a valid pool: every field is optional but
        // the name, and a pool may declare no Ticket source at all.
        let review = &repo_config.pools["review"];
        assert!(review.size.is_none());
        assert!(review.profile.is_none());
        assert!(review.source.is_none());
        assert!(review.label.is_none());
        assert!(review.ready_status.is_none());
        assert!(review.spend_cap(&config.spend).expect("valid").is_none());
    }

    #[test]
    fn a_pool_naming_an_unknown_profile_is_a_loud_error_naming_the_file() {
        let config = Config::from_toml("[profiles.fast]\nruntime = \"claude\"\n");
        let repo = repo_with_config(Some("[pools.bugs]\nprofile = \"fast\"\n"));
        config
            .repo_config(repo.path())
            .expect("a profile the user config defines is fine");

        let repo = repo_with_config(Some("[pools.bugs]\nprofile = \"turbo\"\n"));
        let error = config
            .repo_config(repo.path())
            .expect_err("an unknown profile is an error");
        let message = format!("{error:?}");
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("turbo"), "{message}");
        assert!(message.contains("bugs"), "{message}");
    }

    #[test]
    fn two_pools_with_the_same_routing_label_are_refused_naming_both() {
        let config = Config::from_toml("");
        let repo = repo_with_config(Some(
            "[pools.bugs]\nlabel = \"urgent\"\n\n[pools.features]\nlabel = \"urgent\"\n",
        ));
        let error = config
            .repo_config(repo.path())
            .expect_err("a duplicate routing label is an error");
        let message = format!("{error:?}");
        assert!(message.contains("bugs"), "{message}");
        assert!(message.contains("features"), "{message}");
        assert!(message.contains("urgent"), "{message}");
    }

    #[test]
    fn an_unknown_key_inside_a_pool_is_rejected() {
        let config = Config::from_toml("");
        let repo = repo_with_config(Some("[pools.bugs]\nsixe = 2\n"));
        let error = config
            .repo_config(repo.path())
            .expect_err("a typo is an error");
        let message = format!("{error:?}");
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("sixe"), "{message}");
    }

    #[test]
    fn a_pool_may_select_but_never_define_a_capability() {
        let config = Config::from_toml("");
        // A declaration may reference a user-global profile and source; defining
        // an agent command, a model, a runtime, credentials or a new source kind
        // of its own is a loud error naming the file (ADR 0014).
        for defining in [
            "[pools.bugs]\ncommand = \"rm -rf /\"\n",
            "[pools.bugs]\nmodel = \"claude-sonnet-4\"\n",
            "[pools.bugs]\nruntime = \"claude\"\n",
            "[pools.bugs]\nprovider = \"anthropic\"\n",
            "[pools.bugs]\ncredentials = \"sk-secret\"\n",
            "[pools.bugs]\nsource = \"jira\"\n",
            "[profiles.fast]\nruntime = \"claude\"\n",
        ] {
            let repo = repo_with_config(Some(defining));
            let error = config
                .repo_config(repo.path())
                .expect_err("defining rather than referencing must fail");
            let message = format!("{error:?}");
            assert!(message.contains("config.toml"), "{defining:?}: {message}");
        }
    }

    #[test]
    fn a_pool_size_that_cannot_be_honoured_is_an_error() {
        let user = Config::default();
        let pool = PoolConfig {
            size: Some(usize::MAX),
            ..PoolConfig::default()
        };
        let error = pool
            .validate("bugs", &user)
            .expect_err("an oversized pool is an error");
        assert!(error.to_string().contains("bugs"), "{error}");
    }

    #[test]
    fn a_pool_cap_that_cannot_be_honoured_is_an_error() {
        let config = Config::from_toml("");
        for cap in ["0", "-1.0", "nan"] {
            let repo = repo_with_config(Some(&format!("[pools.bugs]\ncap_usd = {cap}\n")));
            let error = config
                .repo_config(repo.path())
                .expect_err("an unusable cap is an error");
            assert!(format!("{error:?}").contains("bugs"), "{error:?}");
        }
    }

    #[test]
    fn a_pool_cap_defaults_its_window_and_mode_to_the_global_values() {
        let config = Config::from_toml(
            r#"
            [spend]
            cap_usd = 25.0
            window = "today"
            on_exceed = "warn"
            "#,
        );
        let repo = repo_with_config(Some("[pools.bugs]\ncap_usd = 5.0\n"));
        let repo_config = config
            .repo_config(repo.path())
            .expect("the repository config parses");
        let cap = repo_config.pools["bugs"]
            .spend_cap(&config.spend)
            .expect("valid")
            .expect("a cap");
        assert_eq!(cap.limit_micro_usd(), 5_000_000);
        assert_eq!(cap.window(), SpendWindow::Today);
        assert_eq!(cap.mode(), SpendCapMode::Warn);
    }

    #[test]
    fn tickets_default_to_the_linear_source() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert_eq!(cfg.tickets.source, TicketSource::Linear);
        assert_eq!(cfg.tickets.ready_status(), "Todo");
        assert_eq!(cfg.tickets.local.root, ".scratch");
        assert_eq!(cfg.tickets.local.ready_status, "ready-for-agent");
        assert_eq!(cfg.tickets.local.resolved_status, "resolved");
    }

    #[test]
    fn tickets_section_selects_a_source_and_its_settings() {
        let cfg: Config = toml::from_str(
            r#"
            [tickets]
            source = "local"

            [tickets.local]
            root = ".tickets"
            ready_status = "todo"
            resolved_status = "done"
            "#,
        )
        .expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.tickets.source, TicketSource::Local);
        assert_eq!(cfg.tickets.local.root, ".tickets");
        assert_eq!(cfg.tickets.ready_status(), "todo");
    }

    #[test]
    fn a_linear_source_can_override_its_expected_status() {
        let cfg: Config = toml::from_str(
            r#"
            [tickets]
            source = "linear"

            [tickets.linear]
            status = "Ready"
            "#,
        )
        .expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.tickets.source, TicketSource::Linear);
        assert_eq!(cfg.tickets.ready_status(), "Ready");
    }

    #[test]
    fn an_unknown_ticket_source_is_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [tickets]
            source = "jira"
            "#,
        )
        .expect_err("unknown source is an error");
        assert!(err.to_string().contains("jira"), "{err}");
    }

    #[test]
    fn a_tickets_section_without_a_source_is_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [tickets.local]
            root = ".tickets"
            "#,
        )
        .expect_err("missing source is an error");
        assert!(err.to_string().contains("source"), "{err}");
    }

    #[test]
    fn unknown_ticket_keys_are_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [tickets]
            source = "linear"

            [tickets.linear]
            sttus = "Todo"
            "#,
        )
        .expect_err("unknown key is an error");
        assert!(err.to_string().contains("sttus"), "{err}");
    }

    #[test]
    fn blank_ticket_settings_are_rejected() {
        let cfg: Config = toml::from_str(
            r#"
            [tickets]
            source = "linear"

            [tickets.linear]
            status = "   "
            "#,
        )
        .expect("toml parses");
        let error = cfg.validate().expect_err("blank status is an error");
        assert!(
            error.to_string().contains("tickets.linear.status"),
            "{error}"
        );
    }

    #[test]
    fn review_defaults_to_no_reviewer() {
        let cfg = Config::default();
        cfg.validate().expect("the default config is valid");
        assert_eq!(cfg.review.reviewer, ReviewerKind::None);
        assert_eq!(cfg.review.max_passes, 2);
    }

    #[test]
    fn a_review_section_selects_a_reviewer_and_its_pass_cap() {
        let cfg: Config = toml::from_str(
            r#"
            [review]
            reviewer = "coderabbit"
            max_passes = 4
            "#,
        )
        .expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.review.reviewer, ReviewerKind::Coderabbit);
        assert_eq!(cfg.review.max_passes, 4);
    }

    #[test]
    fn the_opencode_action_reviewer_is_selectable() {
        let cfg: Config = toml::from_str(
            r#"
            [review]
            reviewer = "opencode-action"
            "#,
        )
        .expect("toml parses");
        assert_eq!(cfg.review.reviewer, ReviewerKind::OpenCodeAction);
    }

    #[test]
    fn an_unknown_reviewer_is_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [review]
            reviewer = "sonarqube"
            "#,
        )
        .expect_err("unknown reviewer is an error");
        assert!(err.to_string().contains("sonarqube"), "{err}");
    }

    #[test]
    fn unknown_review_keys_are_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [review]
            reveiwer = "coderabbit"
            "#,
        )
        .expect_err("unknown key is an error");
        assert!(err.to_string().contains("reveiwer"), "{err}");
    }

    #[test]
    fn a_zero_pass_cap_is_rejected() {
        let cfg: Config = toml::from_str(
            r#"
            [review]
            reviewer = "coderabbit"
            max_passes = 0
            "#,
        )
        .expect("toml parses");
        let error = cfg.validate().expect_err("zero passes is an error");
        assert!(error.to_string().contains("review.max_passes"), "{error}");
    }

    #[test]
    fn config_path_is_absent_without_a_base_directory() {
        // `env::remove_var` is `unsafe` in edition 2024 and forbidden crate-wide,
        // so clear the variables in a child process rather than mutating this one.
        let output = std::process::Command::new(
            std::env::current_exe().expect("test executable should have a path"),
        )
        .args(["config_path_helper_without_a_base_directory", "--ignored"])
        .env_clear()
        .output()
        .expect("config path helper should run");
        assert!(
            output.status.success(),
            "config path helper failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "helper: run by config_path_is_absent_without_a_base_directory with a cleared environment"]
    fn config_path_helper_without_a_base_directory() {
        assert_eq!(config_path(), None);
    }

    #[test]
    fn layouts_and_their_display_mapping_parse_and_validate() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal.layout_by_display]
            laptop = "narrow"
            external = "wide"
            default = "wide"

            [terminal.layouts.wide]
            [[terminal.layouts.wide.panes]]
            role = "first"
            [[terminal.layouts.wide.panes]]
            role = "agent"
            location = "vsplit"
            bias = 81
            [[terminal.layouts.wide.panes]]
            role = "shell"
            location = "hsplit"

            [terminal.layouts.narrow]
            [[terminal.layouts.narrow.panes]]
            role = "first"
            [[terminal.layouts.narrow.panes]]
            role = "agent"
            location = "vsplit"
            bias = 70
            [[terminal.layouts.narrow.panes]]
            role = "shell"
            location = "hsplit"
            "#,
        )
        .expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.terminal.layouts["wide"].panes[1].bias, 81);
        assert_eq!(cfg.terminal.layouts["narrow"].panes[1].bias, 70);
        assert_eq!(
            cfg.terminal.layout_by_display.laptop.as_deref(),
            Some("narrow")
        );
        assert_eq!(
            cfg.terminal.layout_by_display.fallback.as_deref(),
            Some("wide")
        );
    }

    #[test]
    fn a_display_mapping_to_an_unknown_layout_is_an_error() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal.layout_by_display]
            laptop = "narrow"
            "#,
        )
        .expect("toml parses");
        let error = cfg.validate().expect_err("dangling mapping");
        assert!(
            error.to_string().contains("unknown layout `narrow`"),
            "{error}"
        );
    }

    #[test]
    fn an_invalid_layout_is_an_error_naming_the_profile() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal.layouts.broken]
            [[terminal.layouts.broken.panes]]
            role = "first"
            [[terminal.layouts.broken.panes]]
            role = "shell"
            "#,
        )
        .expect("toml parses");
        let error = cfg.validate().expect_err("missing location");
        assert!(
            error.to_string().contains("terminal.layouts.broken"),
            "{error}"
        );
        assert!(error.to_string().contains("location"), "{error}");
    }

    #[test]
    fn unknown_layout_keys_are_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [terminal.layouts.wide]
            [[terminal.layouts.wide.panes]]
            role = "first"
            [[terminal.layouts.wide.panes]]
            role = "agent"
            location = "vsplit"
            bais = 70
            "#,
        )
        .expect_err("typo is an error");
        assert!(err.to_string().contains("bais"), "{err}");
    }

    #[test]
    fn unknown_display_condition_keys_are_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [terminal.layout_by_display]
            monitor = "wide"
            "#,
        )
        .expect_err("unknown condition is an error");
        assert!(err.to_string().contains("monitor"), "{err}");
    }

    #[test]
    fn spend_is_uncapped_by_default() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert!(cfg.spend.cap_usd.is_none());
        assert_eq!(cfg.spend.window, SpendWindow::Week);
        assert_eq!(cfg.spend.on_exceed, SpendCapMode::Block);
        assert!(cfg.spend.cap().expect("valid").is_none());
    }

    #[test]
    fn a_spend_section_configures_the_cap_window_and_mode() {
        let cfg: Config = toml::from_str(
            r#"
            [spend]
            cap_usd = 12.5
            window = "today"
            on_exceed = "warn"
            "#,
        )
        .expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.spend.window, SpendWindow::Today);
        assert_eq!(cfg.spend.on_exceed, SpendCapMode::Warn);
        let cap = cfg.spend.cap().expect("valid").expect("a cap");
        assert_eq!(cap.limit_micro_usd(), 12_500_000);
        assert_eq!(cap.window(), SpendWindow::Today);
        assert_eq!(cap.mode(), SpendCapMode::Warn);
    }

    #[test]
    fn a_spend_section_keeps_the_uncapped_defaults_for_absent_keys() {
        let cfg: Config = toml::from_str("[spend]\ncap_usd = 5.0\n").expect("toml parses");
        cfg.validate().expect("the config is valid");
        assert_eq!(cfg.spend.window, SpendWindow::Week);
        assert_eq!(cfg.spend.on_exceed, SpendCapMode::Block);
    }

    #[test]
    fn a_non_positive_or_unknown_spend_cap_is_rejected() {
        for text in [
            "[spend]\ncap_usd = 0\n",
            "[spend]\ncap_usd = -1.0\n",
            "[spend]\ncap_usd = nan\n",
        ] {
            let cfg: Config = toml::from_str(text).expect("toml parses");
            let error = cfg.validate().expect_err("an unusable cap is an error");
            assert!(error.to_string().contains("spend.cap_usd"), "{error}");
        }
        let err =
            toml::from_str::<Config>("[spend]\ncap = 5.0\n").expect_err("unknown key is an error");
        assert!(err.to_string().contains("cap"), "{err}");
    }

    #[test]
    fn an_unknown_spend_window_or_mode_is_rejected() {
        let err = toml::from_str::<Config>("[spend]\nwindow = \"fortnight\"\n")
            .expect_err("unknown window is an error");
        assert!(err.to_string().contains("fortnight"), "{err}");
        let err = toml::from_str::<Config>("[spend]\non_exceed = \"shout\"\n")
            .expect_err("unknown mode is an error");
        assert!(err.to_string().contains("shout"), "{err}");
    }

    #[test]
    fn dispatch_is_uncapped_by_default() {
        let cfg = Config::default();
        cfg.validate().expect("the default config is valid");
        assert!(cfg.dispatch.max_concurrent.is_none());
        assert!(cfg.dispatch.cap().expect("valid").is_none());
    }

    #[test]
    fn a_dispatch_section_configures_the_concurrency_cap() {
        let cfg = Config::from_toml("[dispatch]\nmax_concurrent = 4\n");
        cfg.validate().expect("the config is valid");
        let cap = cfg.dispatch.cap().expect("valid").expect("a cap");
        assert_eq!(cap.limit(), 4);
    }

    #[test]
    fn a_concurrency_cap_below_one_is_rejected() {
        let cfg = Config::from_toml("[dispatch]\nmax_concurrent = 0\n");
        let error = cfg.validate().expect_err("a cap of zero is an error");
        assert!(
            error.to_string().contains("dispatch.max_concurrent"),
            "{error}"
        );
    }

    #[test]
    fn unknown_dispatch_keys_are_rejected() {
        let err = toml::from_str::<Config>("[dispatch]\nmax_concurrent_runs = 2\n")
            .expect_err("unknown key is an error");
        assert!(err.to_string().contains("max_concurrent_runs"), "{err}");
    }

    #[test]
    fn the_watchdog_defaults_to_thirty_minutes() {
        let cfg = Config::default();
        cfg.validate().expect("the default config is valid");
        assert_eq!(cfg.watchdog.stuck_after_secs, 30 * 60);
        let window = cfg.watchdog.stuck_window();
        assert!(window.is_enabled());
        assert_eq!(window.as_duration().as_secs(), 30 * 60);
    }

    #[test]
    fn a_watchdog_section_configures_and_disables_the_window() {
        let cfg = Config::from_toml("[watchdog]\nstuck_after_secs = 90\n");
        assert_eq!(cfg.watchdog.stuck_window().as_duration().as_secs(), 90);

        // Zero is the off switch, so the signal can be turned off without a
        // second setting.
        let cfg = Config::from_toml("[watchdog]\nstuck_after_secs = 0\n");
        cfg.validate().expect("a disabled watchdog is valid");
        assert!(!cfg.watchdog.stuck_window().is_enabled());
    }

    #[test]
    fn unknown_watchdog_keys_are_rejected() {
        let err = toml::from_str::<Config>("[watchdog]\nstuck_after = 90\n")
            .expect_err("unknown key is an error");
        assert!(err.to_string().contains("stuck_after"), "{err}");
    }

    #[test]
    fn legacy_workspace_section_is_ignored() {
        let cfg: Config = toml::from_str(
            r#"
            [workspace]
            on_create = ["mise", "trust"]
            "#,
        )
        .expect("the removed workspace section should be ignored");

        assert_eq!(cfg.agent.command, "claude");
    }

    #[test]
    fn forge_section_overrides_only_the_keys_given() {
        let cfg: Config = toml::from_str(
            r#"
            [forge]
            draft = false
            "#,
        )
        .expect("toml parses");
        // Unspecified key keeps its default; the given one is overridden.
        assert!(cfg.forge.pull_requests);
        assert!(!cfg.forge.draft);
    }

    #[test]
    fn forge_can_disable_pull_requests() {
        let cfg: Config = toml::from_str("[forge]\npull_requests = false\n").expect("toml parses");
        assert!(!cfg.forge.pull_requests);
        assert!(cfg.forge.draft);
    }

    #[test]
    fn full_terminal_section_parses() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal]
            listen_on = "unix:/tmp/kitty-visor"
            launch_command = ["kitty", "--detach", "-o", "listen_on=unix:/tmp/kitty-visor"]
            first_pane_command = "dev-server"
            "#,
        )
        .expect("toml parses");
        assert_eq!(
            cfg.terminal.listen_on.as_deref(),
            Some("unix:/tmp/kitty-visor")
        );
        assert_eq!(
            cfg.terminal.launch_command,
            ["kitty", "--detach", "-o", "listen_on=unix:/tmp/kitty-visor"]
        );
        assert_eq!(
            cfg.terminal.first_pane_command.as_deref(),
            Some("dev-server")
        );
    }

    #[test]
    fn the_removed_agent_selector_is_rejected() {
        let err = toml::from_str::<Config>("[terminal]\nagent = \"codex\"\n")
            .expect_err("removed agent selector is an error");
        assert!(err.to_string().contains("agent"), "{err}");
    }

    #[test]
    fn a_configured_agent_command_runs_through_the_login_shell() {
        let cfg: Config = toml::from_str(
            r#"
            [agent]
            command = "cx"
            "#,
        )
        .expect("toml parses");
        let cmd = cfg.agent_command();
        assert_eq!(&cmd[1..], ["-l", "-i", "-c", "cx"]);
        assert!(!cmd[0].is_empty(), "a shell program is always chosen");
    }

    #[test]
    fn the_default_agent_command_gets_the_login_shell_wrap() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        let cmd = cfg.agent_command();
        assert_eq!(&cmd[1..], ["-l", "-i", "-c", "claude"]);
        assert!(!cmd[0].is_empty(), "a shell program is always chosen");
    }

    #[test]
    fn a_configured_first_pane_command_runs_through_the_login_shell_with_a_fallback() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal]
            first_pane_command = "dev-server"
            "#,
        )
        .expect("toml parses");
        let cmd = cfg.first_pane_command();
        assert_eq!(&cmd[1..], ["-l", "-i", "-c", "dev-server; exec \"$SHELL\""]);
        assert!(!cmd[0].is_empty(), "a shell program is always chosen");
    }

    #[test]
    fn an_absent_or_blank_first_pane_command_leaves_the_shell_bare() {
        for text in ["", "[terminal]\nfirst_pane_command = \"   \"\n"] {
            let cfg: Config = toml::from_str(text).expect("toml parses");
            assert!(cfg.first_pane_command().is_empty(), "{text:?}");
        }
    }

    #[test]
    fn the_removed_tool_specific_sections_are_rejected() {
        for section in ["claude", "codex"] {
            let text = format!("[{section}]\ncommand = [\"{section}\"]\n");
            let err = toml::from_str::<Config>(&text).expect_err("removed section is an error");
            assert!(err.to_string().contains(section), "{err}");
        }
    }

    #[test]
    fn partial_terminal_section_leaves_the_rest_default() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal]
            listen_on = "unix:/tmp/kitty-visor"
            "#,
        )
        .expect("toml parses");
        assert_eq!(
            cfg.terminal.listen_on.as_deref(),
            Some("unix:/tmp/kitty-visor")
        );
        assert!(cfg.terminal.launch_command.is_empty());
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [terminal]
            listne_on = "typo"
            "#,
        )
        .expect_err("unknown key is an error");
        assert!(err.to_string().contains("listne_on"), "{err}");
    }

    #[test]
    fn a_missing_file_is_defaults_not_an_error() {
        let cfg = load_from(std::path::Path::new(
            "/nonexistent/jjfx/does-not-exist.toml",
        ))
        .expect("missing file yields defaults");
        assert!(cfg.terminal.listen_on.is_none());
    }
}
