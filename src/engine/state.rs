//! Compatible Workspace Dispatch state repositories.
//!
//! This module is the only place that knows state paths, sidecar lock paths,
//! and JSON codecs. Callers load typed values and commit them with an opaque
//! exact-byte revision; no caller code runs while a lock is held. Atomic
//! replacement and locking are delegated to [`super::atomic`].

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::engine::atomic::{self, AtomicWriteError, LockError};
use crate::engine::locks::{self, ScopedLock};
use crate::engine::{AgentRuntimeProfile, DispatchExclusions, Repository};

/// The directory holding every Worker Pool's own state, one directory per pool.
const POOLS_DIRECTORY: &str = ".jj/pools";

/// The root of jjfx's persisted state, repository-scoped documents included.
const STATE_ROOT: &str = ".jj";

/// The name of the pool a Repository holds when it declares none. A Repository
/// always has one lane, so `default` is always present even though nothing
/// declares it.
pub const DEFAULT_POOL_NAME: &str = "default";

/// The pool document, inside the pool's own directory.
const POOL_DOCUMENT: &str = "pool.json";
const POOL_LOCK_NAME: &str = ".dispatch.lock";
const DESTROY_MARKER_NAME: &str = ".destroying";
const CLEANUP_SUFFIX: &str = ".cleanup";
const SESSION_NAME: &str = "sessions.json";
const SESSION_LOCK_NAME: &str = ".sessions.lock";
const WAITING_NAME: &str = "waiting.json";
const WAITING_LOCK_NAME: &str = ".waiting.lock";
const RUN_RECORDS_NAME: &str = "runs.jsonl";
const RUN_RECORDS_LOCK_NAME: &str = ".runs.lock";

// Repository-scoped state stays at the root of `.jj/`: occupancy is one
// reconcile over the whole terminal and spans pools, and undo is `jj op
// restore`, which is repository-wide.
const IN_USE_PATH: &str = ".jj/in-use.json";
const IN_USE_LOCK: &str = ".jj/.in-use.lock";
const UNDO_PATH: &str = ".jj/undo.json";
const UNDO_LOCK: &str = ".jj/.undo.lock";

// The layout a Repository had while it held exactly one pool. The automatic,
// one-time conversion reads it, verifies the result, and only then removes it.
const LEGACY_POOL_PATH: &str = ".jj/pool.json";
const LEGACY_POOL_DIRECTORY: &str = ".jj/pool";
const LEGACY_IN_USE_NAME: &str = "in-use.json";
const LEGACY_IN_USE_LOCK_NAME: &str = ".in-use.lock";
const LEGACY_UNDO_NAME: &str = "undo.json";
const LEGACY_UNDO_LOCK_NAME: &str = ".undo.lock";
const MIGRATION_LOCK: &str = ".jj/.migration.lock";

/// The Workspace key a repository-wide jjfx mutation is recorded under.
///
/// jj's undo primitives act on the whole repository, and jjfx drives many
/// Workspaces against it, so a mutation that is not confined to one Workspace
/// (a tidy, or a lift of every Workspace) must never be recorded as if it were
/// one Workspace's. It is recorded under this reserved key instead.
pub const REPOSITORY_WIDE_MUTATION: &str = "*";

type Extensions = BTreeMap<String, Value>;

fn wire_agent_is_absent(agent: &Option<WireAgent>) -> bool {
    agent.as_ref().is_none_or(|agent| agent.as_str().is_empty())
}

fn serialize_branch_name<S>(branch: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match branch.as_deref() {
        Some(branch) if !branch.is_empty() => serializer.serialize_some(branch),
        Some(_) | None => serializer.serialize_none(),
    }
}

fn configured_pool_profile(pool: &PoolState) -> Result<AgentRuntimeProfile, String> {
    AgentRuntimeProfile::from_configured(
        pool.agent.as_ref(),
        pool.provider.as_deref(),
        pool.model.as_deref(),
    )
}

fn worker_profile(state: &WorkerState) -> Result<Option<AgentRuntimeProfile>, String> {
    AgentRuntimeProfile::from_run_state(
        state.agent.as_ref(),
        state.provider.as_deref(),
        state.model.as_deref(),
    )
}

fn apply_worker_profile(state: &mut WorkerState, profile: &AgentRuntimeProfile) {
    state.agent = Some(WireAgent::new(profile.runtime().as_str()));
    state.provider = profile.provider_value();
    state.model = profile.model_value();
}

macro_rules! wire_string {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a wire value without imposing lifecycle policy.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            /// Returns the persisted spelling.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

wire_string!(
    WireStatus,
    "An open, forward-compatible persisted status value."
);
wire_string!(
    WireAgent,
    "An open, forward-compatible Agent Runtime value."
);
wire_string!(WireTimestamp, "A timestamp preserved exactly as persisted.");

/// A validated Worker identifier that is safe as one filename component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkerId(String);

impl WorkerId {
    /// Validates a persisted Worker identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_component(&value, "Worker")?;
        Ok(Self(value))
    }

    /// Returns the persisted identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WorkerId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A validated Worker Pool name that is safe as one filename component.
///
/// A pool is a directory (`.jj/pools/<name>/`), so its name is validated exactly
/// as a Worker id is: a single, normal path component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PoolName(String);

impl PoolName {
    /// Validates a persisted Worker Pool name.
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_component(&value, "Worker Pool")?;
        Ok(Self(value))
    }

    /// Returns the persisted name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PoolName {
    /// The pool a Repository holds when it declares none.
    fn default() -> Self {
        Self(DEFAULT_POOL_NAME.to_owned())
    }
}

impl fmt::Display for PoolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PoolName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A validated Ticket identifier that is safe in a Dispatch Group filename.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TicketId(String);

impl TicketId {
    /// Validates a persisted Ticket identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_component(&value, "Ticket")?;
        Ok(Self(value))
    }

    /// Returns the persisted identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TicketId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TicketId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn validate_component(value: &str, kind: &'static str) -> Result<(), IdentifierError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(IdentifierError {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// An invalid compatibility identifier.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind} identifier {value:?} is not a safe filename component")]
pub struct IdentifierError {
    kind: &'static str,
    value: String,
}

/// The exact persisted Pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolState {
    /// Number of configured Workers.
    pub size: i64,
    /// GitHub repository identifier used by dispatch.
    pub gh_repo: String,
    /// Worker identifiers in stable pool order.
    pub workers: Vec<WorkerId>,
    /// Pool creation timestamp.
    pub created_at: WireTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional foreground execution default.
    pub foreground: Option<bool>,
    #[serde(default, skip_serializing_if = "wire_agent_is_absent")]
    /// Optional default Agent Runtime.
    pub agent: Option<WireAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional model provider for the default Agent Runtime profile.
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional model for the default Agent Runtime profile.
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Optional display aliases keyed by Worker.
    pub names: BTreeMap<WorkerId, String>,
    #[serde(flatten)]
    extra: Extensions,
}

impl PoolState {
    /// Creates a pool state with no optional settings or extensions.
    pub fn new(
        size: i64,
        gh_repo: impl Into<String>,
        workers: Vec<WorkerId>,
        created_at: WireTimestamp,
    ) -> Self {
        Self {
            size,
            gh_repo: gh_repo.into(),
            workers,
            created_at,
            foreground: None,
            agent: None,
            provider: None,
            model: None,
            names: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// The exact persisted Worker state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerState {
    /// Persisted Worker Status.
    pub status: WireStatus,
    /// Agent Runtime selected for the current or latest Run.
    pub agent: Option<WireAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Model provider selected for the current or latest Run.
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Model selected for the current or latest Run.
    pub model: Option<String>,
    /// Assigned Ticket, if any.
    pub ticket: Option<String>,
    /// Agent Runtime process identifier, if known.
    pub pid: Option<i64>,
    /// Run start timestamp.
    pub started_at: Option<WireTimestamp>,
    /// Run completion timestamp.
    pub completed_at: Option<WireTimestamp>,
    /// Structured Agent Runtime log path.
    pub log_file: Option<String>,
    #[serde(serialize_with = "serialize_branch_name")]
    /// Current bookmark name, or the dispatch placeholder before resolution.
    pub branch_name: Option<String>,
    /// Process or Agent Runtime exit code.
    pub exit_code: Option<i64>,
    /// Latest failure message.
    pub error: Option<String>,
    #[serde(flatten)]
    extra: Extensions,
}

impl WorkerState {
    /// Creates a Worker state with all optional wire fields explicitly absent.
    pub fn new(status: WireStatus) -> Self {
        Self {
            status,
            agent: None,
            provider: None,
            model: None,
            ticket: None,
            pid: None,
            started_at: None,
            completed_at: None,
            log_file: None,
            branch_name: None,
            exit_code: None,
            error: None,
            extra: BTreeMap::new(),
        }
    }
}

/// The set of Worker Workspaces jjfx currently considers in use (open in a
/// terminal).
///
/// This is jjfx-owned state, deliberately kept out of the Pool and Worker
/// documents so their shape is preserved. A missing or unreadable file means
/// "nothing known to be in use".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InUseState {
    /// Worker identifiers whose Workspaces are currently occupied.
    #[serde(default)]
    pub workers: BTreeSet<WorkerId>,
}

/// The Agent Runtime session each Worker's current Run drives.
///
/// This is jjfx-owned state, deliberately kept out of the Pool and Worker
/// documents so their shape is preserved. jjfx records the session id an
/// opencode Run created here so a later, different process can identify that
/// Run's session - for example to interrupt it - without re-deriving it. A
/// missing or unreadable file means "no session recorded".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    /// Session identifier keyed by the Worker whose current Run drives it.
    #[serde(default)]
    pub sessions: BTreeMap<WorkerId, String>,
}

/// The Tickets the Dispatch capacity governor held back rather than fired.
///
/// This is jjfx-owned state, deliberately kept out of the Pool and Worker
/// documents so their shape is preserved. A missing or unreadable file means
/// "nothing was held back". The set is
/// rewritten by each capped Dispatch, so it always describes the last one: a
/// Ticket the owner dispatched by hand (or that a later, uncapped Dispatch
/// fired) is no longer waiting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingState {
    /// The Tickets held back from the last capped Dispatch, in input order.
    #[serde(default)]
    pub tickets: Vec<String>,
}

/// One durable record of a completed Run.
///
/// jjfx parses a Run's cost, usage, duration and turns at finalization but keeps
/// them nowhere else: `apply_run_result` persists only status, exit code and
/// error, and the Run log is opened with `.truncate(true)`, so each Run
/// overwrites the previous Run's cost data. This record is where that data is
/// kept instead - one JSON object per line in `.jj/pools/<name>/runs.jsonl`, appended
/// and never truncated, the same shape the Agent Runtime hooks already write.
///
/// This is jjfx-owned state, deliberately kept out of the Pool and Worker
/// documents so their shape is preserved, and it is readable without the TUI.
///
/// A value the provider reported unusably - a negative, non-finite or
/// out-of-range cost, as bug-hunt ticket 28 handles - is recorded as absent,
/// never as a reason for the Run to fail or for the record to be dropped. A Run
/// with no reported cost still appends a record, and a reader aggregates the
/// costs that are present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// The identity of the Run: its Worker and the timestamp its Reservation
    /// recorded.
    pub run_id: String,
    /// The Worker the Run executed in.
    pub worker: WorkerId,
    /// The Ticket the Run was dispatched for, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// The Worker Workspace path the Run executed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The bookmark naming the Run's change, if it was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The Agent Runtime that executed the Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The model provider selected for the Run, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model selected for the Run, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider-reported cost in millionths of one US dollar, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
    /// Provider-reported token usage, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RunRecordUsage>,
    /// Provider-reported duration in milliseconds, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Provider-reported number of turns, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u64>,
    /// The provider-reported conclusion.
    pub conclusion: RunRecordConclusion,
    /// The provider-neutral failure message, for a failed Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the Run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<WireTimestamp>,
    /// When the Run completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<WireTimestamp>,
}

/// Token usage normalized across Agent Runtime providers, as persisted in a
/// [`RunRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecordUsage {
    /// Provider-reported input tokens.
    pub input_tokens: u64,
    /// Input tokens read from a provider cache.
    pub cached_input_tokens: u64,
    /// Input tokens written to a provider cache.
    pub cache_write_input_tokens: u64,
    /// Provider-reported output tokens.
    pub output_tokens: u64,
    /// Output tokens used for provider reasoning.
    pub reasoning_output_tokens: u64,
}

/// The provider-reported conclusion of a Run, as persisted in a [`RunRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunRecordConclusion {
    /// The Agent Runtime reported successful completion.
    Succeeded,
    /// The Agent Runtime reported a failure.
    Failed,
}

/// The kind of jjfx-initiated mutation an undo record describes.
///
/// Open to the compatible set of mutations jjfx performs; a value jjfx does not
/// recognize is still carried, so a record written by a newer jjfx is not lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// A Forge run: fetch, weld, fix and push a Workspace's chain.
    Forge,
    /// A weld: rebase a Workspace's own mutable chain onto trunk.
    Weld,
    /// A tidy: abandon or rebase junk empty commits.
    Tidy,
    /// A Reset: abandon a Run and restore the Workspace.
    Reset,
}

impl MutationKind {
    /// Returns the persisted spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            MutationKind::Forge => "forge",
            MutationKind::Weld => "weld",
            MutationKind::Tidy => "tidy",
            MutationKind::Reset => "reset",
        }
    }
}

impl fmt::Display for MutationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One jjfx-initiated mutation of one Workspace, recorded so it can be undone.
///
/// jj snapshots every mutation in a repository-wide operation log. jjfx records
/// here the operation the repository was at *before* the mutation and the one it
/// was at when the mutation finished, so an undo can restore the checkpoint - but
/// only while the recorded end is still the repository's tip, because a restore
/// is repository-wide (see `engine::undo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationRecord {
    /// The Workspace mutated, or [`REPOSITORY_WIDE_MUTATION`] for a mutation that
    /// is not confined to one Workspace.
    pub workspace: String,
    /// What jjfx did.
    pub kind: MutationKind,
    /// The operation the repository was at immediately before the mutation.
    pub before: String,
    /// The operation the repository was at when the mutation finished.
    pub after: String,
    /// When the mutation finished.
    pub at: WireTimestamp,
}

/// The latest jjfx-initiated mutation per Workspace.
///
/// This is jjfx-owned state, deliberately kept out of the Pool and Worker
/// documents so their shape is preserved. A missing or unreadable file means
/// "nothing recorded to undo".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoState {
    /// The latest recorded mutation, keyed by the Workspace it mutated (or
    /// [`REPOSITORY_WIDE_MUTATION`]).
    #[serde(default)]
    pub mutations: BTreeMap<String, MutationRecord>,
}

/// One Sub-issue entry in a Dispatch Group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubIssueState {
    /// Human-facing Ticket title.
    pub title: String,
    /// Persisted Sub-issue Status.
    pub status: WireStatus,
    /// Direct dependency Ticket identifiers.
    pub blocked_by: Vec<TicketId>,
    /// Assigned Worker, if dispatched.
    pub worker: Option<WorkerId>,
    /// Resulting bookmark, if known.
    pub branch: Option<String>,
    /// Dispatch timestamp.
    pub dispatched_at: Option<WireTimestamp>,
    /// Completion timestamp.
    pub completed_at: Option<WireTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional reason a Sub-issue was skipped.
    pub skip_reason: Option<String>,
    /// Number of completed retry attempts.
    pub retries: i64,
    #[serde(flatten)]
    extra: Extensions,
}

impl SubIssueState {
    /// Creates an unassigned Sub-issue state.
    pub fn new(title: impl Into<String>, status: WireStatus, blocked_by: Vec<TicketId>) -> Self {
        Self {
            title: title.into(),
            status,
            blocked_by,
            worker: None,
            branch: None,
            dispatched_at: None,
            completed_at: None,
            skip_reason: None,
            retries: 0,
            extra: BTreeMap::new(),
        }
    }
}

/// Provider options persisted with a Dispatch Group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchGroupOptions {
    #[serde(default, skip_serializing_if = "wire_agent_is_absent")]
    /// Optional Agent Runtime override.
    pub agent: Option<WireAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional model provider selected for this Dispatch Group.
    pub provider: Option<String>,
    /// Optional model spelling, represented by an empty string when unset.
    pub model: String,
    #[serde(flatten)]
    extra: Extensions,
}

impl DispatchGroupOptions {
    /// Creates options without an Agent Runtime override.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            agent: None,
            provider: None,
            model: model.into(),
            extra: BTreeMap::new(),
        }
    }
}

/// The exact persisted Dispatch Group state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchGroupState {
    /// Parent Ticket that owns this Dispatch Group.
    pub parent: TicketId,
    /// Dispatch Group creation timestamp.
    pub created_at: WireTimestamp,
    /// GitHub repository identifier used by dispatch.
    pub gh_repo: String,
    /// Sub-issue state keyed by Ticket identifier.
    pub sub_issues: BTreeMap<TicketId, SubIssueState>,
    /// Agent Runtime and model options.
    pub opts: DispatchGroupOptions,
    #[serde(flatten)]
    extra: Extensions,
}

impl DispatchGroupState {
    /// Creates an empty Dispatch Group.
    pub fn new(
        parent: TicketId,
        created_at: WireTimestamp,
        gh_repo: impl Into<String>,
        opts: DispatchGroupOptions,
    ) -> Self {
        Self {
            parent,
            created_at,
            gh_repo: gh_repo.into(),
            sub_issues: BTreeMap::new(),
            opts,
            extra: BTreeMap::new(),
        }
    }
}

/// A loaded state document, including absence as data rather than an error.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded<T> {
    /// The state file does not exist.
    Missing,
    /// A validated state value and its exact-byte revision.
    Present(Versioned<T>),
}

/// A typed value paired with its opaque exact-byte revision.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    /// Loaded typed state.
    pub value: T,
    revision: StateRevision<T>,
}

impl<T> Versioned<T> {
    /// Returns the revision required for a compare-and-swap commit.
    pub fn revision(&self) -> &StateRevision<T> {
        &self.revision
    }
    /// Separates the typed value from its revision.
    pub fn into_parts(self) -> (T, StateRevision<T>) {
        (self.value, self.revision)
    }
}

/// An opaque exact-byte state revision.
pub struct StateRevision<T> {
    bytes: Vec<u8>,
    marker: PhantomData<fn() -> T>,
}
impl<T> Clone for StateRevision<T> {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            marker: PhantomData,
        }
    }
}
impl<T> PartialEq for StateRevision<T> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl<T> Eq for StateRevision<T> {}
impl<T> fmt::Debug for StateRevision<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StateRevision(..)")
    }
}

/// The state a commit expects to find after acquiring its lock.
#[derive(Debug, Clone, PartialEq)]
pub enum Expected<T> {
    /// Commit only if the target is absent.
    Missing,
    /// Commit only if the exact target bytes still match this revision.
    Match(StateRevision<T>),
}
/// A typed state change.
#[derive(Debug, Clone, PartialEq)]
pub enum StateChange<T> {
    /// Atomically replace the target with this typed state.
    Replace(T),
    /// Remove the target while retaining its stable sidecar lock.
    Remove,
}
/// The result of a commit attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitOutcome<T> {
    /// The requested change was committed.
    Applied(Loaded<T>),
    /// The expected state did not match the state reloaded under lock.
    Conflict(Loaded<T>),
}

/// A contextual state repository failure.
#[derive(Debug, Error)]
#[error("cannot {operation} {subject}: {detail}")]
pub struct StateError {
    operation: &'static str,
    subject: String,
    detail: String,
}
impl StateError {
    fn new(operation: &'static str, subject: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            operation,
            subject: subject.into(),
            detail: error.to_string(),
        }
    }
}

/// Access to all persisted Workspace Dispatch state for one Worker Pool.
///
/// A store is bound to one pool, because nearly every state path is now
/// pool-scoped: the pool document, its Workers, Run Records, the waiting set,
/// sessions and its locks all live under `.jj/pools/<name>/`. The
/// repository-scoped documents - occupancy and undo - ignore the pool and stay
/// at the root of `.jj/`.
#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
    pool: PoolName,
}

/// Result of the repository-owned atomic Worker Reservation transition.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReservationOutcome {
    Reserved {
        worker: WorkerId,
        profile: AgentRuntimeProfile,
        revision: StateRevision<WorkerState>,
        rollback: Box<WorkerState>,
    },
    NoIdle {
        available: usize,
    },
    WorkerNotInPool {
        worker: WorkerId,
    },
    WorkerNotIdle {
        worker: WorkerId,
    },
    WorkerNotEligible {
        worker: WorkerId,
    },
    InvalidAgentRuntime {
        message: String,
    },
}

pub(crate) struct ReservationInput {
    pub(crate) ticket: String,
    pub(crate) requested_worker: Option<WorkerId>,
    pub(crate) started_at: WireTimestamp,
    pub(crate) branch_name: String,
    pub(crate) profile: Option<AgentRuntimeProfile>,
}

pub(crate) struct ReservedWorker {
    pub(crate) worker: WorkerId,
    pub(crate) ticket: String,
    pub(crate) profile: AgentRuntimeProfile,
    pub(crate) revision: StateRevision<WorkerState>,
    pub(crate) rollback: WorkerState,
}

pub(crate) enum ReservationsOutcome {
    Reserved {
        workers: Vec<ReservedWorker>,
        unavailable: Vec<usize>,
    },
    NoIdle {
        available: usize,
    },
    WorkerNotInPool {
        worker: WorkerId,
    },
    WorkerNotIdle {
        worker: WorkerId,
    },
    WorkerNotEligible {
        worker: WorkerId,
    },
    InvalidAgentRuntime {
        message: String,
    },
}

pub(crate) enum GrowReservationsOutcome {
    Reserved(Vec<ReservedWorker>),
    Conflict,
    NoIdle { available: usize },
    InvalidAgentRuntime { message: String },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FollowUpOutcome {
    Started {
        profile: AgentRuntimeProfile,
        prior_log: Option<String>,
        revision: StateRevision<WorkerState>,
        rollback: Box<WorkerState>,
    },
    WorkerNotInPool,
    WorkerBusy,
    WorkerStateMissing,
    InvalidAgentRuntime {
        message: String,
    },
}

#[derive(Debug)]
pub(crate) enum PoolMembershipOutcome {
    Changed {
        capacity: usize,
        removed: Vec<WorkerId>,
    },
    NoChange {
        capacity: usize,
    },
    NeedsGrowth,
    Busy {
        workers: Vec<WorkerId>,
    },
    WorkerNotInPool {
        worker: WorkerId,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DetachedCleanupStatus {
    Ready,
    Busy {
        pid: Option<u32>,
        /// The runtime persisted for the Worker's current Run, so a destroy can
        /// tell an opencode Run (stopped through its session) from one that owns
        /// its own process group.
        runtime: Option<crate::engine::AgentRuntime>,
    },
    NotDetached,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PoolAliasOutcome {
    Updated,
    WorkerNotInPool,
    Destroying,
}

/// Pool state repository.
#[derive(Debug, Clone)]
pub struct PoolStateRepository {
    directory: PathBuf,
}
/// Worker state repository.
#[derive(Debug, Clone)]
pub struct WorkerStateRepository {
    directory: PathBuf,
    worker: WorkerId,
}
/// Dispatch Group state repository.
#[derive(Debug, Clone)]
pub struct DispatchGroupStateRepository {
    directory: PathBuf,
    parent: TicketId,
}
/// The jjfx-owned Occupancy (in-use) state repository.
#[derive(Debug, Clone)]
pub struct InUseStateRepository {
    root: PathBuf,
}
/// The jjfx-owned Agent Runtime Session sidecar repository.
#[derive(Debug, Clone)]
pub struct SessionStateRepository {
    directory: PathBuf,
}
/// The jjfx-owned record of Tickets the Dispatch capacity governor held back.
#[derive(Debug, Clone)]
pub struct WaitingStateRepository {
    directory: PathBuf,
}
/// The jjfx-owned, append-only per-Run cost record store.
#[derive(Debug, Clone)]
pub struct RunRecordRepository {
    directory: PathBuf,
}
/// The jjfx-owned undo record store: the operation the repository was at before
/// each jjfx-initiated mutation.
#[derive(Debug, Clone)]
pub struct MutationRecordRepository {
    root: PathBuf,
}

impl Repository {
    /// Opens the state facade for the pool a Repository holds by default.
    pub fn state_store(&self) -> StateStore {
        self.state_store_for_pool(PoolName::default())
    }

    /// Opens the state facade for one named Worker Pool.
    pub fn state_store_for_pool(&self, pool: PoolName) -> StateStore {
        StateStore {
            root: self.root().to_owned(),
            pool,
        }
    }
}

/// The filename the detached orchestration runner uses for `parent`.
fn orchestration_file_name(parent: &TicketId, extension: &str) -> String {
    format!(
        "orchestrate-{}.{extension}",
        parent.as_str().to_ascii_lowercase()
    )
}

impl StateStore {
    /// The pool's own state directory, `.jj/pools/<name>`.
    fn directory(&self) -> PathBuf {
        self.root.join(POOLS_DIRECTORY).join(self.pool.as_str())
    }

    /// The structured Run log path for `worker`, inside the pool directory.
    pub fn worker_log(&self, worker: &WorkerId) -> PathBuf {
        self.directory().join(format!("{worker}.log"))
    }

    /// The detached orchestration runner's log path for `parent`.
    pub fn orchestration_log(&self, parent: &TicketId) -> PathBuf {
        self.directory()
            .join(orchestration_file_name(parent, "log"))
    }

    /// The detached orchestration runner's lock path for `parent`.
    pub fn orchestration_lock(&self, parent: &TicketId) -> PathBuf {
        self.directory()
            .join(orchestration_file_name(parent, "lock"))
    }

    /// The Worker operation lock path for `worker`.
    pub fn worker_operation_lock(&self, worker: &WorkerId) -> PathBuf {
        self.directory().join(format!("{worker}.workspace.lock"))
    }

    fn pool_document(&self) -> PathBuf {
        self.directory().join(POOL_DOCUMENT)
    }

    fn pool_lock(&self) -> ScopedLock {
        ScopedLock::pool(self.directory().join(POOL_LOCK_NAME))
    }

    fn destroy_marker(&self) -> PathBuf {
        self.directory().join(DESTROY_MARKER_NAME)
    }

    fn worker_path(&self, worker: &WorkerId) -> PathBuf {
        self.directory().join(format!("{worker}.json"))
    }

    fn worker_lock(&self, worker: &WorkerId) -> ScopedLock {
        ScopedLock::pool(sidecar(&self.worker_path(worker)))
    }

    /// Opens the Worker Pool state repository.
    pub fn pool(&self) -> PoolStateRepository {
        PoolStateRepository {
            directory: self.directory(),
        }
    }
    /// Opens one Worker state repository.
    pub fn worker(&self, worker: WorkerId) -> WorkerStateRepository {
        WorkerStateRepository {
            directory: self.directory(),
            worker,
        }
    }
    /// Opens one Dispatch Group state repository.
    pub fn dispatch_group(&self, parent: TicketId) -> DispatchGroupStateRepository {
        DispatchGroupStateRepository {
            directory: self.directory(),
            parent,
        }
    }

    /// Opens the jjfx-owned Occupancy (in-use) state repository.
    pub fn in_use(&self) -> InUseStateRepository {
        InUseStateRepository {
            root: self.root.clone(),
        }
    }

    /// Opens the jjfx-owned Agent Runtime Session sidecar repository.
    pub fn sessions(&self) -> SessionStateRepository {
        SessionStateRepository {
            directory: self.directory(),
        }
    }

    /// Opens the jjfx-owned Waiting (governor-held) Ticket repository.
    pub fn waiting(&self) -> WaitingStateRepository {
        WaitingStateRepository {
            directory: self.directory(),
        }
    }

    /// Opens the jjfx-owned, append-only per-Run cost record store.
    pub fn runs(&self) -> RunRecordRepository {
        RunRecordRepository {
            directory: self.directory(),
        }
    }

    /// Opens the jjfx-owned undo record store.
    pub fn undo(&self) -> MutationRecordRepository {
        MutationRecordRepository {
            root: self.root.clone(),
        }
    }

    /// Sets or clears cosmetic Worker metadata under the compatible Pool lock.
    pub(crate) fn set_pool_profile(&self, profile: &AgentRuntimeProfile) -> Result<(), StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            if self.destroy_marker().exists() {
                return Err(StateError::new(
                    "set profile",
                    subject,
                    "Pool destruction is in progress",
                ));
            }
            let mut pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("set profile", subject, "state is missing"));
                }
            };
            pool.agent = Some(WireAgent::new(profile.runtime().as_str()));
            pool.provider = profile.provider_value();
            pool.model = profile.model_value();
            write_atomic(&self.pool_document(), &pool, subject)
        })
    }

    /// Sets or clears cosmetic Worker metadata under the compatible Pool lock.
    pub(crate) fn set_worker_alias(
        &self,
        worker: &WorkerId,
        alias: Option<String>,
    ) -> Result<PoolAliasOutcome, StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            let mut pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("set alias", subject, "state is missing"));
                }
            };
            if self.destroy_marker().exists() {
                return Ok(PoolAliasOutcome::Destroying);
            }
            if !pool.workers.iter().any(|member| member == worker) {
                return Ok(PoolAliasOutcome::WorkerNotInPool);
            }
            match alias {
                Some(alias) => {
                    pool.names.insert(worker.clone(), alias);
                }
                None => {
                    pool.names.remove(worker);
                }
            }
            write_atomic(&self.pool_document(), &pool, subject)?;
            Ok(PoolAliasOutcome::Updated)
        })
    }

    /// Atomically detaches every current Worker while retaining durable cleanup state.
    pub(crate) fn detach_pool_for_destroy(&self) -> Result<Vec<WorkerId>, StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            write_destroy_marker(&self.directory(), subject)?;
            let pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => return Ok(Vec::new()),
            };
            if pool.workers.is_empty() {
                return Ok(Vec::new());
            }
            let workers = pool.workers.clone();
            let locks = workers
                .iter()
                .map(|worker| self.worker_lock(worker))
                .collect::<Vec<_>>();
            with_locks(&locks, subject, || {
                let mut pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                    Loaded::Present(versioned) => versioned.value,
                    Loaded::Missing => return Ok(Vec::new()),
                };
                let workers = pool.workers.clone();
                for worker in &workers {
                    write_cleanup_marker(&self.directory(), worker, subject)?;
                }
                pool.workers.clear();
                pool.names.clear();
                pool.size = 0;
                write_atomic(&self.pool_document(), &pool, subject)?;
                Ok(workers)
            })
        })
    }

    /// Removes Pool state and every non-lock Pool artifact under compatible locks.
    pub(crate) fn finish_pool_destroy(&self) -> Result<(), StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            if let Loaded::Present(pool) =
                load_state(&self.pool_document(), subject, &validate_pool)?
                && !pool.value.workers.is_empty()
            {
                return Err(StateError::new(
                    "finish destroy",
                    subject,
                    "Pool membership changed during destruction",
                ));
            }
            let directory = self.directory();
            let entries = fs::read_dir(&directory)
                .map_err(|error| StateError::new("inspect destroy", subject, error))?;
            let mut locks = Vec::new();
            for entry in entries {
                let entry =
                    entry.map_err(|error| StateError::new("inspect destroy", subject, error))?;
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    locks.push(ScopedLock::pool(sidecar(&path)));
                }
            }
            with_locks(&locks, subject, || {
                let entries = fs::read_dir(&directory)
                    .map_err(|error| StateError::new("inspect destroy", subject, error))?;
                for entry in entries {
                    let entry = entry
                        .map_err(|error| StateError::new("inspect destroy", subject, error))?;
                    let path = entry.path();
                    if path == self.destroy_marker()
                        || path
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().ends_with(".lock"))
                    {
                        continue;
                    }
                    let is_directory = entry
                        .file_type()
                        .map_err(|error| StateError::new("inspect destroy", subject, error))?
                        .is_dir();
                    let result = if is_directory {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };
                    if let Err(error) = result
                        && error.kind() != io::ErrorKind::NotFound
                    {
                        return Err(StateError::new(
                            "remove destroy artifact",
                            path.display().to_string(),
                            error,
                        ));
                    }
                }
                remove_file_if_present(&self.pool_document(), "remove", subject)?;
                remove_file_if_present(&self.destroy_marker(), "finish destroy", subject)
            })
        })
    }

    /// Commits externally provisioned growth only while Pool destruction is absent.
    pub(crate) fn commit_pool_growth(
        &self,
        expected: StateRevision<PoolState>,
        next: PoolState,
    ) -> Result<CommitOutcome<PoolState>, StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            let current = load_state(&self.pool_document(), subject, &validate_pool)?;
            if self.destroy_marker().exists() {
                return Ok(CommitOutcome::Conflict(current));
            }
            let matches = matches!(
                &current,
                Loaded::Present(versioned) if versioned.revision.bytes == expected.bytes
            );
            if !matches {
                return Ok(CommitOutcome::Conflict(current));
            }
            validate_pool(&next)
                .map_err(|error| StateError::new("validate growth for", subject, error))?;
            write_atomic(&self.pool_document(), &next, subject)?;
            load_state(&self.pool_document(), subject, &validate_pool).map(CommitOutcome::Applied)
        })
    }

    /// Persists a launched PID only while its exact Reservation still owns Pool capacity.
    pub(crate) fn persist_reserved_pid(
        &self,
        worker: &WorkerId,
        expected: StateRevision<WorkerState>,
        pid: u32,
    ) -> Result<CommitOutcome<WorkerState>, StateError> {
        let subject = format!("Worker {worker}");
        with_locks(&[self.pool_lock()], &subject, || {
            let worker_path = self.worker_path(worker);
            let worker_lock = ScopedLock::pool(sidecar(&worker_path));
            with_locks(&[worker_lock], &subject, || {
                let current = load_state(&worker_path, &subject, &validate_worker)?;
                let pool = load_state(&self.pool_document(), "Worker Pool", &validate_pool)?;
                let owns_capacity = matches!(
                    pool,
                    Loaded::Present(ref versioned)
                        if versioned.value.workers.iter().any(|member| member == worker)
                ) && !self.destroy_marker().exists()
                    && !cleanup_marker_path(&self.directory(), worker).exists();
                let revision_matches = matches!(
                    &current,
                    Loaded::Present(versioned) if versioned.revision.bytes == expected.bytes
                );
                if !owns_capacity || !revision_matches {
                    return Ok(CommitOutcome::Conflict(current));
                }
                let Loaded::Present(mut versioned) = current else {
                    return Ok(CommitOutcome::Conflict(Loaded::Missing));
                };
                versioned.value.pid = Some(i64::from(pid));
                write_atomic(&worker_path, &versioned.value, &subject)?;
                load_state(&worker_path, &subject, &validate_worker).map(CommitOutcome::Applied)
            })
        })
    }

    /// Atomically detaches the stable Pool tail when every selected Worker is non-busy.
    pub(crate) fn shrink_pool(
        &self,
        requested: usize,
    ) -> Result<PoolMembershipOutcome, StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            let pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("resize", subject, "state is missing"));
                }
            };
            let current = usize::try_from(pool.size)
                .map_err(|_| StateError::new("resize", subject, "size is invalid"))?;
            if requested > current {
                return Ok(PoolMembershipOutcome::NeedsGrowth);
            }
            if requested == current {
                return Ok(PoolMembershipOutcome::NoChange { capacity: current });
            }
            let selected = pool.workers[requested..].to_vec();
            let locks = selected
                .iter()
                .map(|worker| self.worker_lock(worker))
                .collect::<Vec<_>>();
            with_locks(&locks, subject, || {
                let mut pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                    Loaded::Present(versioned) => versioned.value,
                    Loaded::Missing => {
                        return Err(StateError::new("resize", subject, "state is missing"));
                    }
                };
                let mut busy = Vec::new();
                for worker in &selected {
                    let path = self.worker_path(worker);
                    match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                        Loaded::Present(versioned) if versioned.value.status.as_str() == "busy" => {
                            busy.push(worker.clone());
                        }
                        Loaded::Present(_) | Loaded::Missing => {}
                    }
                }
                if !busy.is_empty() {
                    return Ok(PoolMembershipOutcome::Busy { workers: busy });
                }
                for worker in &selected {
                    pool.names.remove(worker);
                }
                pool.workers.truncate(requested);
                pool.size = i64::try_from(requested)
                    .map_err(|_| StateError::new("resize", subject, "size is too large"))?;
                write_atomic(&self.pool_document(), &pool, subject)?;
                // Mark the detached Workers *after* the manifest. A partial write
                // must never leave a Pool member carrying a cleanup marker: that
                // member would fail `persist_reserved_pid` forever, so no Run
                // could start on it.
                for worker in &selected {
                    write_cleanup_marker(&self.directory(), worker, subject)?;
                }
                Ok(PoolMembershipOutcome::Changed {
                    capacity: requested,
                    removed: selected,
                })
            })
        })
    }

    /// Atomically detaches one named non-busy Worker from Pool membership.
    pub(crate) fn remove_pool_worker(
        &self,
        requested: &WorkerId,
    ) -> Result<PoolMembershipOutcome, StateError> {
        let subject = "Worker Pool";
        with_locks(&[self.pool_lock()], subject, || {
            let mut pool = match load_state(&self.pool_document(), subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("remove", subject, "state is missing"));
                }
            };
            let Some(index) = pool.workers.iter().position(|worker| worker == requested) else {
                return Ok(PoolMembershipOutcome::WorkerNotInPool {
                    worker: requested.clone(),
                });
            };
            let lock = self.worker_lock(requested);
            with_locks(&[lock], subject, || {
                let path = self.worker_path(requested);
                if let Loaded::Present(versioned) =
                    load_state(&path, &format!("Worker {requested}"), &validate_worker)?
                    && versioned.value.status.as_str() == "busy"
                {
                    return Ok(PoolMembershipOutcome::Busy {
                        workers: vec![requested.clone()],
                    });
                }
                pool.workers.remove(index);
                pool.names.remove(requested);
                pool.size = i64::try_from(pool.workers.len())
                    .map_err(|_| StateError::new("remove", subject, "size is too large"))?;
                write_atomic(&self.pool_document(), &pool, subject)?;
                // As in `shrink_pool`: mark the detached Worker only after the
                // manifest, so a failed write cannot strand a member.
                write_cleanup_marker(&self.directory(), requested, subject)?;
                Ok(PoolMembershipOutcome::Changed {
                    capacity: pool.workers.len(),
                    removed: vec![requested.clone()],
                })
            })
        })
    }

    /// Finds durable markers left by interrupted detached Worker cleanup.
    pub(crate) fn detached_cleanup_markers(&self) -> Result<Vec<WorkerId>, StateError> {
        let directory = self.directory();
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StateError::new("discover cleanup", "Worker Pool", error)),
        };
        let mut workers = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|error| StateError::new("discover cleanup", "Worker Pool", error))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(worker) = name.strip_suffix(CLEANUP_SUFFIX) else {
                continue;
            };
            // A marker whose stem is not a valid Worker id cannot name a Pool
            // member, so there is nothing to clean up. Skipping it keeps one junk
            // file from failing discovery - and with it every resize and destroy.
            let Ok(worker) = WorkerId::parse(worker) else {
                continue;
            };
            workers.push(worker);
        }
        workers.sort();
        Ok(workers)
    }

    /// Revalidates a cleanup marker under the Pool lock followed by its Worker lock.
    pub(crate) fn detached_cleanup_status(
        &self,
        worker: &WorkerId,
    ) -> Result<DetachedCleanupStatus, StateError> {
        let subject = format!("Worker {worker}");
        with_locks(&[self.pool_lock()], &subject, || {
            let worker_lock = self.worker_lock(worker);
            with_locks(&[worker_lock], &subject, || {
                let marker = cleanup_marker_path(&self.directory(), worker);
                if !marker.exists() {
                    return Ok(DetachedCleanupStatus::NotDetached);
                }
                let pool = load_state(&self.pool_document(), "Worker Pool", &validate_pool)?;
                if matches!(
                    pool,
                    Loaded::Present(ref versioned)
                        if versioned.value.workers.iter().any(|member| member == worker)
                ) {
                    remove_file_if_present(&marker, "remove stale cleanup marker", &subject)?;
                    return Ok(DetachedCleanupStatus::NotDetached);
                }
                let path = self.worker_path(worker);
                match load_state(&path, &subject, &validate_worker)? {
                    Loaded::Present(versioned) if versioned.value.status.as_str() == "busy" => {
                        Ok(DetachedCleanupStatus::Busy {
                            pid: versioned.value.pid.and_then(|pid| u32::try_from(pid).ok()),
                            runtime: versioned
                                .value
                                .agent
                                .as_ref()
                                .and_then(crate::engine::AgentRuntime::parse),
                        })
                    }
                    Loaded::Present(_) | Loaded::Missing => Ok(DetachedCleanupStatus::Ready),
                }
            })
        })
    }

    /// Removes a completed cleanup marker if the Worker remains detached.
    pub(crate) fn finish_detached_cleanup(&self, worker: &WorkerId) -> Result<bool, StateError> {
        let subject = format!("Worker {worker}");
        with_locks(&[self.pool_lock()], &subject, || {
            let worker_lock = self.worker_lock(worker);
            with_locks(&[worker_lock], &subject, || {
                let pool = load_state(&self.pool_document(), "Worker Pool", &validate_pool)?;
                if matches!(
                    pool,
                    Loaded::Present(ref versioned)
                        if versioned.value.workers.iter().any(|member| member == worker)
                ) {
                    return Ok(false);
                }
                remove_file_if_present(
                    &cleanup_marker_path(&self.directory(), worker),
                    "finish cleanup",
                    &subject,
                )?;
                Ok(true)
            })
        })
    }

    /// Atomically starts a Follow-up Run while preserving prior Worker context.
    pub(crate) fn begin_follow_up(
        &self,
        worker: &WorkerId,
        started_at: WireTimestamp,
        log_file: String,
        profile_override: Option<AgentRuntimeProfile>,
    ) -> Result<FollowUpOutcome, StateError> {
        let pool_subject = "Worker Pool";
        with_locks(&[self.pool_lock()], pool_subject, || {
            let pool = match load_state(&self.pool_document(), pool_subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => return Ok(FollowUpOutcome::WorkerNotInPool),
            };
            if !pool.workers.iter().any(|candidate| candidate == worker) {
                return Ok(FollowUpOutcome::WorkerNotInPool);
            }
            let worker_lock = self.worker_lock(worker);
            with_locks(&[worker_lock], &format!("Worker {worker}"), || {
                let path = self.worker_path(worker);
                let mut state =
                    match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                        Loaded::Present(versioned) => versioned.value,
                        Loaded::Missing => return Ok(FollowUpOutcome::WorkerStateMissing),
                    };
                if state.status.as_str() == "busy" {
                    return Ok(FollowUpOutcome::WorkerBusy);
                }
                let profile = match profile_override {
                    Some(profile) => profile,
                    None => match worker_profile(&state) {
                        Ok(Some(profile)) => profile,
                        Ok(None) => match configured_pool_profile(&pool) {
                            Ok(profile) => profile,
                            Err(message) => {
                                return Ok(FollowUpOutcome::InvalidAgentRuntime { message });
                            }
                        },
                        Err(message) => {
                            return Ok(FollowUpOutcome::InvalidAgentRuntime { message });
                        }
                    },
                };
                let prior_log = state.log_file.clone();
                let rollback = state.clone();
                state.status = WireStatus::new("busy");
                apply_worker_profile(&mut state, &profile);
                state.pid = None;
                state.started_at = Some(started_at);
                state.completed_at = None;
                state.log_file = Some(log_file);
                state.exit_code = None;
                state.error = None;
                write_atomic(&path, &state, &format!("Worker {worker}"))?;
                let revision =
                    match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                        Loaded::Present(versioned) => versioned.revision().clone(),
                        Loaded::Missing => {
                            return Err(StateError::new(
                                "follow up",
                                format!("Worker {worker}"),
                                "state disappeared after transition",
                            ));
                        }
                    };
                Ok(FollowUpOutcome::Started {
                    profile,
                    prior_log,
                    revision,
                    rollback: Box::new(rollback),
                })
            })
        })
    }

    /// Adopts provisioned Workers and reserves a complete batch in one locked transition.
    pub(crate) fn grow_and_reserve_workers(
        &self,
        expected: StateRevision<PoolState>,
        next: PoolState,
        inputs: Vec<ReservationInput>,
        excluded: &DispatchExclusions,
    ) -> Result<GrowReservationsOutcome, StateError> {
        let pool_subject = "Worker Pool";
        with_locks(&[self.pool_lock()], pool_subject, || {
            if self.destroy_marker().exists() {
                return Err(StateError::new(
                    "grow and reserve",
                    pool_subject,
                    "Pool destruction is in progress",
                ));
            }
            let loaded = match load_state(&self.pool_document(), pool_subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned,
                Loaded::Missing => return Ok(GrowReservationsOutcome::Conflict),
            };
            if loaded.revision() != &expected {
                return Ok(GrowReservationsOutcome::Conflict);
            }
            validate_pool(&next)
                .map_err(|detail| StateError::new("grow and reserve", pool_subject, detail))?;
            let worker_locks = next
                .workers
                .iter()
                .map(|worker| self.worker_lock(worker))
                .collect::<Vec<_>>();
            with_locks(&worker_locks, pool_subject, || {
                let loaded = match load_state(&self.pool_document(), pool_subject, &validate_pool)?
                {
                    Loaded::Present(versioned) => versioned,
                    Loaded::Missing => return Ok(GrowReservationsOutcome::Conflict),
                };
                if loaded.revision() != &expected {
                    return Ok(GrowReservationsOutcome::Conflict);
                }
                let reserved = match reserve_batch_locked(
                    &self.directory(),
                    &next,
                    inputs,
                    false,
                    excluded,
                )? {
                    ReservationsOutcome::Reserved {
                        workers,
                        unavailable,
                    } => {
                        debug_assert!(unavailable.is_empty());
                        workers
                    }
                    ReservationsOutcome::NoIdle { available } => {
                        return Ok(GrowReservationsOutcome::NoIdle { available });
                    }
                    ReservationsOutcome::WorkerNotInPool { worker }
                    | ReservationsOutcome::WorkerNotIdle { worker }
                    | ReservationsOutcome::WorkerNotEligible { worker } => {
                        return Err(StateError::new(
                            "grow and reserve",
                            pool_subject,
                            format!("unexpected targeted Worker {worker}"),
                        ));
                    }
                    ReservationsOutcome::InvalidAgentRuntime { message } => {
                        return Ok(GrowReservationsOutcome::InvalidAgentRuntime { message });
                    }
                };
                if let Err(primary) = write_atomic(&self.pool_document(), &next, pool_subject) {
                    let rollback_failures = reserved
                        .iter()
                        .filter_map(|reserved| {
                            let path = self.directory().join(format!("{}.json", reserved.worker));
                            write_atomic(
                                &path,
                                &reserved.rollback,
                                &format!("Worker {}", reserved.worker),
                            )
                            .err()
                            .map(|error| format!("{}: {error}", reserved.worker))
                        })
                        .collect::<Vec<_>>();
                    let detail = if rollback_failures.is_empty() {
                        primary.to_string()
                    } else {
                        format!(
                            "{primary}; reservation rollback failed: {}",
                            rollback_failures.join("; ")
                        )
                    };
                    return Err(StateError::new("grow and reserve", pool_subject, detail));
                }
                Ok(GrowReservationsOutcome::Reserved(reserved))
            })
        })
    }

    /// Atomically reserves enough idle Workers for the complete input batch.
    ///
    /// Selection and every write happen while holding the Pool lock followed
    /// by every member Worker lock in deterministic Pool order. A shortage
    /// writes nothing. A later write failure restores every earlier Worker
    /// before releasing the locks.
    pub(crate) fn reserve_workers(
        &self,
        inputs: Vec<ReservationInput>,
        excluded: &DispatchExclusions,
    ) -> Result<ReservationsOutcome, StateError> {
        self.reserve_workers_inner(inputs, false, excluded)
    }

    pub(crate) fn reserve_available_workers(
        &self,
        inputs: Vec<ReservationInput>,
        excluded: &DispatchExclusions,
    ) -> Result<ReservationsOutcome, StateError> {
        self.reserve_workers_inner(inputs, true, excluded)
    }

    fn reserve_workers_inner(
        &self,
        inputs: Vec<ReservationInput>,
        allow_partial: bool,
        excluded: &DispatchExclusions,
    ) -> Result<ReservationsOutcome, StateError> {
        let pool_subject = "Worker Pool";
        with_locks(&[self.pool_lock()], pool_subject, || {
            if self.destroy_marker().exists() {
                return Err(StateError::new(
                    "reserve",
                    pool_subject,
                    "Pool destruction is in progress",
                ));
            }
            let pool = match load_state(&self.pool_document(), pool_subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("reserve", pool_subject, "state is missing"));
                }
            };
            let worker_locks = pool
                .workers
                .iter()
                .map(|worker| self.worker_lock(worker))
                .collect::<Vec<_>>();
            with_locks(&worker_locks, pool_subject, || {
                let pool = match load_state(&self.pool_document(), pool_subject, &validate_pool)? {
                    Loaded::Present(versioned) => versioned.value,
                    Loaded::Missing => {
                        return Err(StateError::new("reserve", pool_subject, "state is missing"));
                    }
                };
                reserve_batch_locked(&self.directory(), &pool, inputs, allow_partial, excluded)
            })
        })
    }

    /// Atomically reserves an idle Worker while holding the compatible pool
    /// and Worker sidecar locks. This repository-owned transition reloads
    /// membership and Worker state under lock so lifecycle callers never
    /// make a claim from a stale snapshot.
    pub(crate) fn reserve_worker(
        &self,
        requested: Option<&WorkerId>,
        ticket: String,
        started_at: WireTimestamp,
        branch_name: String,
        profile: Option<AgentRuntimeProfile>,
        excluded: &DispatchExclusions,
    ) -> Result<ReservationOutcome, StateError> {
        let pool_subject = "Worker Pool";
        with_locks(&[self.pool_lock()], pool_subject, || {
            if self.destroy_marker().exists() {
                return Err(StateError::new(
                    "reserve",
                    pool_subject,
                    "Pool destruction is in progress",
                ));
            }
            let pool = match load_state(&self.pool_document(), pool_subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("reserve", pool_subject, "state is missing"));
                }
            };
            let worker_locks = pool
                .workers
                .iter()
                .map(|worker| self.worker_lock(worker))
                .collect::<Vec<_>>();
            with_locks(&worker_locks, pool_subject, || {
                let pool = match load_state(&self.pool_document(), pool_subject, &validate_pool)? {
                    Loaded::Present(versioned) => versioned.value,
                    Loaded::Missing => {
                        return Err(StateError::new("reserve", pool_subject, "state is missing"));
                    }
                };
                if let Some(requested) = requested
                    && !pool.workers.iter().any(|worker| worker == requested)
                {
                    return Ok(ReservationOutcome::WorkerNotInPool {
                        worker: requested.clone(),
                    });
                }
                if let Some(requested) = requested
                    && excluded.contains(requested)
                {
                    return Ok(ReservationOutcome::WorkerNotEligible {
                        worker: requested.clone(),
                    });
                }
                let profile = match profile {
                    Some(profile) => profile,
                    None => match configured_pool_profile(&pool) {
                        Ok(profile) => profile,
                        Err(message) => {
                            return Ok(ReservationOutcome::InvalidAgentRuntime { message });
                        }
                    },
                };

                let candidates = requested.into_iter().cloned().chain(
                    requested
                        .is_none()
                        .then(|| pool.workers.clone())
                        .into_iter()
                        .flatten(),
                );
                let mut available = 0;
                let mut selected = None;
                for worker in candidates {
                    if excluded.contains(&worker) {
                        // An occupied Worker is not eligible capacity.
                        continue;
                    }
                    let path = self.worker_path(&worker);
                    let state =
                        match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                            Loaded::Present(versioned) => versioned.value,
                            Loaded::Missing => {
                                return Err(StateError::new(
                                    "reserve",
                                    format!("Worker {worker}"),
                                    "state is missing",
                                ));
                            }
                        };
                    if state.status.as_str() == "idle" {
                        available += 1;
                        if selected.is_none() {
                            selected = Some((worker, state));
                        }
                    } else if requested.is_some() {
                        return Ok(ReservationOutcome::WorkerNotIdle { worker });
                    }
                }

                let Some((worker, mut state)) = selected else {
                    return Ok(ReservationOutcome::NoIdle { available });
                };
                let rollback = state.clone();
                state.status = WireStatus::new("busy");
                apply_worker_profile(&mut state, &profile);
                state.ticket = Some(ticket);
                state.started_at = Some(started_at);
                state.log_file = Some(
                    self.directory()
                        .join(format!("{worker}.log"))
                        .to_string_lossy()
                        .into_owned(),
                );
                state.branch_name = Some(branch_name);
                state.completed_at = None;
                state.pid = None;
                state.exit_code = None;
                state.error = None;
                let path = self.worker_path(&worker);
                write_atomic(&path, &state, &format!("Worker {worker}"))?;
                let revision =
                    match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                        Loaded::Present(versioned) => versioned.revision().clone(),
                        Loaded::Missing => {
                            return Err(StateError::new(
                                "reserve",
                                format!("Worker {worker}"),
                                "state disappeared after reservation",
                            ));
                        }
                    };
                Ok(ReservationOutcome::Reserved {
                    worker,
                    profile,
                    revision,
                    rollback: Box::new(rollback),
                })
            })
        })
    }
}

fn reserve_batch_locked(
    directory: &Path,
    pool: &PoolState,
    inputs: Vec<ReservationInput>,
    allow_partial: bool,
    excluded: &DispatchExclusions,
) -> Result<ReservationsOutcome, StateError> {
    // One Ticket must not occupy two Workers. Each input below reserves a
    // *distinct* Worker, so a batch naming the same Ticket twice would run it
    // twice. A duplicate is a caller bug, so reject the batch outright rather
    // than silently doubling (or dropping) it.
    let mut seen = BTreeSet::new();
    for input in &inputs {
        if let Ok(id) = TicketId::parse(input.ticket.clone())
            && !seen.insert(id.clone())
        {
            return Err(StateError::new(
                "reserve",
                "Worker Pool",
                format!("duplicate Ticket {id} in one Dispatch batch"),
            ));
        }
    }
    let default_profile = if inputs.iter().any(|input| input.profile.is_none()) {
        match configured_pool_profile(pool) {
            Ok(profile) => Some(profile),
            Err(message) => return Ok(ReservationsOutcome::InvalidAgentRuntime { message }),
        }
    } else {
        None
    };
    let mut workers = Vec::with_capacity(pool.workers.len());
    for worker in &pool.workers {
        let path = directory.join(format!("{worker}.json"));
        let state = match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
            Loaded::Present(versioned) => versioned.value,
            Loaded::Missing => {
                return Err(StateError::new(
                    "reserve",
                    format!("Worker {worker}"),
                    "state is missing",
                ));
            }
        };
        workers.push((worker.clone(), state));
    }
    let available = workers
        .iter()
        .filter(|(worker, state)| state.status.as_str() == "idle" && !excluded.contains(worker))
        .count();
    let mut selected = BTreeSet::new();
    let mut assignments = Vec::with_capacity(inputs.len());
    let mut unavailable = Vec::new();
    for (index, input) in inputs.into_iter().enumerate() {
        let selection = match input.requested_worker.as_ref() {
            Some(requested) => match workers.iter().find(|(worker, _)| worker == requested) {
                None => {
                    if !allow_partial {
                        return Ok(ReservationsOutcome::WorkerNotInPool {
                            worker: requested.clone(),
                        });
                    }
                    None
                }
                Some((worker, _)) if excluded.contains(worker) => {
                    if !allow_partial {
                        return Ok(ReservationsOutcome::WorkerNotEligible {
                            worker: worker.clone(),
                        });
                    }
                    None
                }
                Some((worker, state))
                    if state.status.as_str() == "idle" && !selected.contains(worker) =>
                {
                    Some((worker.clone(), state.clone()))
                }
                Some((worker, _)) => {
                    if !allow_partial {
                        return Ok(ReservationsOutcome::WorkerNotIdle {
                            worker: worker.clone(),
                        });
                    }
                    None
                }
            },
            None => workers
                .iter()
                .find(|(worker, state)| {
                    state.status.as_str() == "idle"
                        && !selected.contains(worker)
                        && !excluded.contains(worker)
                })
                .map(|(worker, state)| (worker.clone(), state.clone())),
        };
        let Some(selection) = selection else {
            if allow_partial {
                unavailable.push(index);
                continue;
            }
            return Ok(ReservationsOutcome::NoIdle { available });
        };
        selected.insert(selection.0.clone());
        assignments.push((selection.0, selection.1, input));
    }

    let mut written = Vec::<(PathBuf, WorkerId, WorkerState)>::new();
    let mut reserved = Vec::with_capacity(assignments.len());
    for (worker, mut state, input) in assignments {
        let rollback = state.clone();
        let ticket = input.ticket;
        let profile = input
            .profile
            .or_else(|| default_profile.clone())
            .expect("a default profile exists when a Reservation has no override");
        state.status = WireStatus::new("busy");
        apply_worker_profile(&mut state, &profile);
        state.ticket = Some(ticket.clone());
        state.started_at = Some(input.started_at);
        state.log_file = Some(
            directory
                .join(format!("{worker}.log"))
                .to_string_lossy()
                .into_owned(),
        );
        state.branch_name = Some(input.branch_name);
        state.completed_at = None;
        state.pid = None;
        state.exit_code = None;
        state.error = None;
        let path = directory.join(format!("{worker}.json"));
        if let Err(primary) = write_atomic(&path, &state, &format!("Worker {worker}")) {
            return Err(rollback_reservation_writes(&written, primary));
        }
        written.push((path.clone(), worker.clone(), rollback.clone()));
        let revision = match load_state(&path, &format!("Worker {worker}"), &validate_worker) {
            Ok(Loaded::Present(versioned)) => versioned.revision().clone(),
            Ok(Loaded::Missing) => {
                let primary = StateError::new(
                    "reserve",
                    format!("Worker {worker}"),
                    "state disappeared after reservation",
                );
                return Err(rollback_reservation_writes(&written, primary));
            }
            Err(primary) => return Err(rollback_reservation_writes(&written, primary)),
        };
        reserved.push(ReservedWorker {
            worker,
            ticket,
            profile,
            revision,
            rollback,
        });
    }
    Ok(ReservationsOutcome::Reserved {
        workers: reserved,
        unavailable,
    })
}

fn rollback_reservation_writes(
    written: &[(PathBuf, WorkerId, WorkerState)],
    primary: StateError,
) -> StateError {
    let rollback_failures = written
        .iter()
        .rev()
        .filter_map(|(path, worker, rollback)| {
            write_atomic(path, rollback, &format!("Worker {worker}"))
                .err()
                .map(|error| format!("{worker}: {error}"))
        })
        .collect::<Vec<_>>();
    let detail = if rollback_failures.is_empty() {
        primary.to_string()
    } else {
        format!(
            "{primary}; reservation rollback failed: {}",
            rollback_failures.join("; ")
        )
    };
    StateError::new("reserve", "Worker Pool", detail)
}

macro_rules! repository_methods {
    ($repository:ident, $state:ty, $path:expr, $locks:expr, $subject:expr, $validate:expr) => {
        impl $repository {
            /// Loads compatible state without taking a mutation lock.
            pub fn load(&self) -> Result<Loaded<$state>, StateError> {
                load_state(&$path(self), &$subject(self), &$validate)
            }
            /// Commits a typed compare-and-swap mutation under compatible locks.
            pub fn commit(
                &self,
                expected: Expected<$state>,
                change: StateChange<$state>,
            ) -> Result<CommitOutcome<$state>, StateError> {
                commit_state(
                    &$path(self),
                    &$locks(self),
                    &$subject(self),
                    expected,
                    change,
                    $validate,
                )
            }
        }
    };
}

repository_methods!(
    PoolStateRepository,
    PoolState,
    |s: &PoolStateRepository| s.directory.join(POOL_DOCUMENT),
    |s: &PoolStateRepository| vec![ScopedLock::pool(s.directory.join(POOL_LOCK_NAME))],
    |_s: &PoolStateRepository| "Worker Pool".to_owned(),
    validate_pool
);
repository_methods!(
    WorkerStateRepository,
    WorkerState,
    |s: &WorkerStateRepository| s.directory.join(format!("{}.json", s.worker)),
    |s: &WorkerStateRepository| {
        vec![ScopedLock::pool(
            s.directory.join(format!("{}.json.lock", s.worker)),
        )]
    },
    |s: &WorkerStateRepository| format!("Worker {}", s.worker),
    validate_worker
);
repository_methods!(
    InUseStateRepository,
    InUseState,
    |s: &InUseStateRepository| s.root.join(IN_USE_PATH),
    |s: &InUseStateRepository| vec![ScopedLock::repository(s.root.join(IN_USE_LOCK))],
    |_s: &InUseStateRepository| "Worker Occupancy".to_owned(),
    validate_in_use
);
repository_methods!(
    SessionStateRepository,
    SessionState,
    |s: &SessionStateRepository| s.directory.join(SESSION_NAME),
    |s: &SessionStateRepository| vec![ScopedLock::pool(s.directory.join(SESSION_LOCK_NAME))],
    |_s: &SessionStateRepository| "Worker Sessions".to_owned(),
    validate_sessions
);
repository_methods!(
    MutationRecordRepository,
    UndoState,
    |s: &MutationRecordRepository| s.root.join(UNDO_PATH),
    |s: &MutationRecordRepository| vec![ScopedLock::repository(s.root.join(UNDO_LOCK))],
    |_s: &MutationRecordRepository| "Undo Records".to_owned(),
    validate_undo
);
repository_methods!(
    WaitingStateRepository,
    WaitingState,
    |s: &WaitingStateRepository| s.directory.join(WAITING_NAME),
    |s: &WaitingStateRepository| vec![ScopedLock::pool(s.directory.join(WAITING_LOCK_NAME))],
    |_s: &WaitingStateRepository| "Waiting Tickets".to_owned(),
    validate_waiting
);

impl WaitingStateRepository {
    /// Returns the Tickets held back by the last capped Dispatch, in order.
    ///
    /// Absence and an unreadable file both mean "nothing held back": the waiting
    /// set is a presentation aid, never a reason to refuse a Dispatch.
    pub fn held(&self) -> Vec<String> {
        match self.load() {
            Ok(Loaded::Present(versioned)) => versioned.value.tickets,
            Ok(Loaded::Missing) | Err(_) => Vec::new(),
        }
    }

    /// Replaces the waiting set wholesale, for the Dispatch that just ran.
    ///
    /// A no-op when the set is unchanged, so re-running an identical capped
    /// Dispatch does not rewrite the record.
    pub fn replace(&self, tickets: Vec<String>) -> Result<(), StateError> {
        let subject = "Waiting Tickets";
        with_locks(
            &[ScopedLock::pool(self.directory.join(WAITING_LOCK_NAME))],
            subject,
            || {
                let current = match load_state(
                    &self.directory.join(WAITING_NAME),
                    subject,
                    &validate_waiting,
                )? {
                    Loaded::Present(versioned) => versioned.value.tickets,
                    Loaded::Missing => Vec::new(),
                };
                if current == tickets {
                    return Ok(());
                }
                write_atomic(
                    &self.directory.join(WAITING_NAME),
                    &WaitingState { tickets },
                    subject,
                )
            },
        )
    }
}

impl InUseStateRepository {
    /// Returns the Worker ids currently marked in use, treating absence as empty.
    pub fn marked(&self) -> Result<BTreeSet<WorkerId>, StateError> {
        match self.load()? {
            Loaded::Present(versioned) => Ok(versioned.value.workers),
            Loaded::Missing => Ok(BTreeSet::new()),
        }
    }

    /// Adds one Worker to the in-use set.
    pub fn mark(&self, worker: &WorkerId) -> Result<(), StateError> {
        self.update(|workers| {
            workers.insert(worker.clone());
        })
    }

    /// Removes one Worker from the in-use set.
    pub fn clear(&self, worker: &WorkerId) -> Result<(), StateError> {
        self.update(|workers| {
            workers.remove(worker);
        })
    }

    /// Replaces the in-use set wholesale, for reconciliation against the terminal.
    ///
    /// A no-op when the set is unchanged, so a periodic reconcile does not
    /// rewrite the marker on every refresh.
    pub fn replace(&self, workers: BTreeSet<WorkerId>) -> Result<(), StateError> {
        let subject = "Worker Occupancy";
        with_locks(
            &[ScopedLock::repository(self.root.join(IN_USE_LOCK))],
            subject,
            || {
                let current =
                    match load_state(&self.root.join(IN_USE_PATH), subject, &validate_in_use)? {
                        Loaded::Present(versioned) => versioned.value.workers,
                        Loaded::Missing => BTreeSet::new(),
                    };
                if current == workers {
                    return Ok(());
                }
                self.write(workers, subject)
            },
        )
    }

    fn update(&self, edit: impl FnOnce(&mut BTreeSet<WorkerId>)) -> Result<(), StateError> {
        let subject = "Worker Occupancy";
        with_locks(
            &[ScopedLock::repository(self.root.join(IN_USE_LOCK))],
            subject,
            || {
                let mut workers =
                    match load_state(&self.root.join(IN_USE_PATH), subject, &validate_in_use)? {
                        Loaded::Present(versioned) => versioned.value.workers,
                        Loaded::Missing => BTreeSet::new(),
                    };
                edit(&mut workers);
                self.write(workers, subject)
            },
        )
    }

    fn write(&self, workers: BTreeSet<WorkerId>, subject: &str) -> Result<(), StateError> {
        write_atomic(
            &self.root.join(IN_USE_PATH),
            &InUseState { workers },
            subject,
        )
    }
}

impl SessionStateRepository {
    /// Returns the session id recorded for `worker`, if any.
    pub fn session(&self, worker: &WorkerId) -> Result<Option<String>, StateError> {
        match self.load()? {
            Loaded::Present(versioned) => Ok(versioned.value.sessions.get(worker).cloned()),
            Loaded::Missing => Ok(None),
        }
    }

    /// Records the session id `worker`'s current Run drives, replacing any prior one.
    pub fn record(
        &self,
        worker: &WorkerId,
        session_id: impl Into<String>,
    ) -> Result<(), StateError> {
        let session_id = session_id.into();
        self.update(|sessions| {
            sessions.insert(worker.clone(), session_id.clone()) != Some(session_id)
        })
    }

    /// Forgets the session id recorded for `worker`, if any.
    ///
    /// A Worker whose current Run drives no session must record none: a stale id
    /// left by a previous Run would let a later reader mistake that Run's
    /// session for this one's - and finish this Worker on an outcome that was
    /// never its own.
    pub fn clear(&self, worker: &WorkerId) -> Result<(), StateError> {
        self.update(|sessions| sessions.remove(worker).is_some())
    }

    /// Applies `edit` under the sidecar lock, writing only when it changed
    /// something.
    fn update(
        &self,
        edit: impl FnOnce(&mut BTreeMap<WorkerId, String>) -> bool,
    ) -> Result<(), StateError> {
        let subject = "Worker Sessions";
        with_locks(
            &[ScopedLock::pool(self.directory.join(SESSION_LOCK_NAME))],
            subject,
            || {
                let mut sessions = match load_state(
                    &self.directory.join(SESSION_NAME),
                    subject,
                    &validate_sessions,
                )? {
                    Loaded::Present(versioned) => versioned.value.sessions,
                    Loaded::Missing => BTreeMap::new(),
                };
                if !edit(&mut sessions) {
                    return Ok(());
                }
                write_atomic(
                    &self.directory.join(SESSION_NAME),
                    &SessionState { sessions },
                    subject,
                )
            },
        )
    }
}

impl RunRecordRepository {
    /// Appends one Run record as a single JSON line, creating the store if it is
    /// missing.
    ///
    /// The line is written under the store's sidecar lock and flushed before the
    /// lock is released, so concurrent Workers append whole records and the
    /// store is never truncated or rewritten.
    pub fn append(&self, record: &RunRecord) -> Result<(), StateError> {
        let subject = "Run Records";
        with_locks(
            &[ScopedLock::pool(self.directory.join(RUN_RECORDS_LOCK_NAME))],
            subject,
            || {
                let mut line = serde_json::to_vec(record)
                    .map_err(|error| StateError::new("serialize", subject, error))?;
                line.push(b'\n');
                let path = self.directory.join(RUN_RECORDS_NAME);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        StateError::new("create store directory for", subject, error)
                    })?;
                }
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|error| StateError::new("open", subject, error))?;
                file.write_all(&line)
                    .map_err(|error| StateError::new("append", subject, error))?;
                file.sync_data()
                    .map_err(|error| StateError::new("sync", subject, error))
            },
        )
    }

    /// Reads every recorded Run in append order.
    ///
    /// A missing store means "no Runs recorded". A line that cannot be decoded -
    /// a torn tail from a process that died mid-append, or a foreign writer - is
    /// skipped so one bad line never hides the rest.
    pub fn records(&self) -> Result<Vec<RunRecord>, StateError> {
        let subject = "Run Records";
        let bytes = match fs::read(self.directory.join(RUN_RECORDS_NAME)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StateError::new("read", subject, error)),
        };
        Ok(bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect())
    }

    /// The Tickets whose most recent Run concluded successfully.
    ///
    /// This is the durable completion signal a local Ticket writeback resolves
    /// from: a Run appends its record when it finalizes, and the record names the
    /// Ticket, so a finished Run can still resolve its Ticket after `pool reset`
    /// cleared the live Worker's Ticket association. Only the most recent record
    /// per Ticket counts, so a Ticket re-dispatched after an earlier success is
    /// not resolved from the stale record; a Ticket whose latest Run failed is not
    /// resolved either. Returned in first-recorded order.
    pub fn completed_tickets(&self) -> Result<Vec<String>, StateError> {
        let mut latest: Vec<(String, RunRecordConclusion)> = Vec::new();
        for record in self.records()? {
            let Some(ticket) = record.ticket else {
                continue;
            };
            match latest
                .iter_mut()
                .find(|(candidate, _)| candidate == &ticket)
            {
                Some(entry) => entry.1 = record.conclusion,
                None => latest.push((ticket, record.conclusion)),
            }
        }
        Ok(latest
            .into_iter()
            .filter(|(_, conclusion)| *conclusion == RunRecordConclusion::Succeeded)
            .map(|(ticket, _)| ticket)
            .collect())
    }
}

impl MutationRecordRepository {
    /// Returns the latest mutation recorded for `workspace`, if any.
    pub fn mutation(&self, workspace: &str) -> Result<Option<MutationRecord>, StateError> {
        match self.load()? {
            Loaded::Present(versioned) => Ok(versioned.value.mutations.get(workspace).cloned()),
            Loaded::Missing => Ok(None),
        }
    }

    /// Records `record` as the latest mutation for its Workspace, replacing any
    /// prior one.
    ///
    /// Only the latest mutation is kept: an undo restores the operation the
    /// repository was at *before* it, which already subsumes every earlier
    /// jjfx-initiated mutation.
    pub fn record(&self, record: &MutationRecord) -> Result<(), StateError> {
        let workspace = record.workspace.clone();
        self.update(|mutations| mutations.insert(workspace, record.clone()).is_none())
    }

    /// Forgets the mutation recorded for `workspace`, if any.
    pub fn clear(&self, workspace: &str) -> Result<(), StateError> {
        self.update(|mutations| mutations.remove(workspace).is_some())
    }

    /// Applies `edit` under the sidecar lock, writing only when it changed
    /// something.
    fn update(
        &self,
        edit: impl FnOnce(&mut BTreeMap<String, MutationRecord>) -> bool,
    ) -> Result<(), StateError> {
        let subject = "Undo Records";
        with_locks(
            &[ScopedLock::repository(self.root.join(UNDO_LOCK))],
            subject,
            || {
                let mut mutations =
                    match load_state(&self.root.join(UNDO_PATH), subject, &validate_undo)? {
                        Loaded::Present(versioned) => versioned.value.mutations,
                        Loaded::Missing => BTreeMap::new(),
                    };
                if !edit(&mut mutations) {
                    return Ok(());
                }
                write_atomic(
                    &self.root.join(UNDO_PATH),
                    &UndoState { mutations },
                    subject,
                )
            },
        )
    }
}

impl WorkerStateRepository {
    /// Reports whether Pool membership removal left a durable cleanup marker.
    pub(crate) fn cleanup_marker_exists(&self) -> bool {
        cleanup_marker_path(&self.directory, &self.worker).exists()
    }

    /// Removes detached Worker state and its log together under the Worker lock.
    ///
    /// When `expected` is present, a newer Worker state is left untouched and
    /// reported as a conflict. With no expected revision the operation is
    /// idempotent and removes whichever detached state remains.
    pub(crate) fn remove_detached(
        &self,
        expected: Option<StateRevision<WorkerState>>,
    ) -> Result<bool, StateError> {
        let path = self.directory.join(format!("{}.json", self.worker));
        let subject = format!("Worker {}", self.worker);
        with_locks(&[ScopedLock::pool(sidecar(&path))], &subject, || {
            let current = load_state(&path, &subject, &validate_worker)?;
            if let Some(expected) = expected
                && !matches!(
                    &current,
                    Loaded::Present(versioned) if expected.bytes == versioned.revision.bytes
                )
            {
                return Ok(false);
            }
            let log = self.directory.join(format!("{}.log", self.worker));
            for target in [&log, &path] {
                match fs::remove_file(target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(StateError::new("remove", &subject, error)),
                }
            }
            Ok(true)
        })
    }
}

impl DispatchGroupStateRepository {
    fn path(&self) -> PathBuf {
        self.directory.join(format!(
            "dispatch-{}.json",
            self.parent.as_str().to_lowercase()
        ))
    }

    /// Loads the Dispatch Group without taking a mutation lock.
    pub fn load(&self) -> Result<Loaded<DispatchGroupState>, StateError> {
        let subject = format!("Dispatch Group {}", self.parent);
        load_state(&self.path(), &subject, &|group| {
            validate_dispatch_group_for(&self.parent, group)
        })
    }

    /// Commits a typed compare-and-swap mutation under Pool and group locks.
    pub fn commit(
        &self,
        expected: Expected<DispatchGroupState>,
        change: StateChange<DispatchGroupState>,
    ) -> Result<CommitOutcome<DispatchGroupState>, StateError> {
        let path = self.path();
        let subject = format!("Dispatch Group {}", self.parent);
        commit_state(
            &path,
            &[
                ScopedLock::pool(self.directory.join(POOL_LOCK_NAME)),
                ScopedLock::pool(sidecar(&path)),
            ],
            &subject,
            expected,
            change,
            |group| validate_dispatch_group_for(&self.parent, group),
        )
    }
}

fn cleanup_marker_path(directory: &Path, worker: &WorkerId) -> PathBuf {
    directory.join(format!("{worker}{CLEANUP_SUFFIX}"))
}

/// Converts a Repository's single-pool legacy layout to the per-pool layout.
///
/// The legacy layout held exactly one pool: the document `.jj/pool.json` beside
/// the directory `.jj/pool/`. This builds the new layout - the pool document and
/// everything pool-scoped under `.jj/pools/default/`, with the repository-scoped
/// documents (`in-use.json`, `undo.json` and their locks) moving to `.jj/` -
/// **verifies** the result, and only then removes the old layout.
///
/// The conversion copies before it removes, and refuses if it cannot verify the
/// result: a Repository whose new layout cannot be proven complete and readable
/// is left exactly as it was, and the failure is loud. A Worker Pool is never
/// destroyed by a conversion that could not finish. It is safe to run
/// concurrently - a repository-wide lock serialises it - and safe to re-run,
/// because a destination that already holds the source bytes is accepted.
pub(crate) fn convert_legacy_layout(root: &Path) -> Result<(), StateError> {
    if !legacy_layout_present(root) {
        return Ok(());
    }
    let subject = "Worker Pool state conversion";
    with_locks(
        &[ScopedLock::repository(root.join(MIGRATION_LOCK))],
        subject,
        || {
            // Another process may have converted between the check above and the
            // lock being taken.
            if !legacy_layout_present(root) {
                return Ok(());
            }
            let destination = root.join(POOLS_DIRECTORY).join(DEFAULT_POOL_NAME);
            copy_legacy_document(root, &destination, subject)?;
            copy_legacy_directory(root, &destination, subject)?;
            verify_conversion(root, &destination, subject)?;
            remove_legacy_layout(root, subject)
        },
    )
}

/// The names of pool directories under `.jj/pools/` that `declared` does not
/// claim.
///
/// A directory that appeared without a declaration is either a crash's
/// leftovers or a hand-edit, so jjfx reports it and leaves it: it is never
/// adopted as a pool, and it never blocks another pool.
fn orphan_pool_directories(
    root: &Path,
    declared: &BTreeSet<PoolName>,
) -> Result<Vec<PoolName>, StateError> {
    let subject = "Worker Pool directories";
    let entries = match fs::read_dir(root.join(POOLS_DIRECTORY)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StateError::new("inspect", subject, error)),
    };
    let mut orphans = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| StateError::new("inspect", subject, error))?;
        let is_directory = entry
            .file_type()
            .map_err(|error| StateError::new("inspect", subject, error))?
            .is_dir();
        if !is_directory {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // A directory whose name is not a valid pool name cannot name a pool, so
        // it is not an orphan to report either.
        let Ok(name) = PoolName::parse(name) else {
            continue;
        };
        if !declared.contains(&name) {
            orphans.push(name);
        }
    }
    orphans.sort();
    Ok(orphans)
}

impl Repository {
    /// Names of pool directories under `.jj/pools/` that no declaration claims.
    pub fn orphan_pool_directories(
        &self,
        declared: &BTreeSet<PoolName>,
    ) -> Result<Vec<PoolName>, StateError> {
        orphan_pool_directories(self.root(), declared)
    }
}

fn legacy_layout_present(root: &Path) -> bool {
    root.join(LEGACY_POOL_PATH).is_file() || root.join(LEGACY_POOL_DIRECTORY).is_dir()
}

fn copy_legacy_document(root: &Path, destination: &Path, subject: &str) -> Result<(), StateError> {
    let source = root.join(LEGACY_POOL_PATH);
    if !source.is_file() {
        return Ok(());
    }
    copy_verified(&source, &destination.join(POOL_DOCUMENT), subject)
}

fn copy_legacy_directory(root: &Path, destination: &Path, subject: &str) -> Result<(), StateError> {
    let legacy = root.join(LEGACY_POOL_DIRECTORY);
    let entries = match fs::read_dir(&legacy) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StateError::new("read", subject, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| StateError::new("read", subject, error))?;
        let name = entry.file_name();
        let target = legacy_destination(root, destination, &name);
        copy_verified(&entry.path(), &target, subject)?;
    }
    Ok(())
}

/// Where a legacy `.jj/pool/` entry belongs in the new layout.
///
/// Occupancy and undo are repository-scoped, so they move to the root of `.jj/`;
/// everything else is the pool's and moves under the pool directory. The
/// filename is unchanged in both cases.
fn legacy_destination(root: &Path, destination: &Path, name: &OsStr) -> PathBuf {
    if matches!(
        name.to_str(),
        Some(
            LEGACY_IN_USE_NAME | LEGACY_IN_USE_LOCK_NAME | LEGACY_UNDO_NAME | LEGACY_UNDO_LOCK_NAME
        )
    ) {
        root.join(STATE_ROOT).join(name)
    } else {
        destination.join(name)
    }
}

/// Copies `source` to `destination` and verifies the copy byte for byte.
///
/// A destination that already holds the same bytes is accepted, so a conversion
/// interrupted between the copy and the removal can be re-run. A destination
/// that exists with *different* bytes is refused: jjfx cannot know which copy is
/// authoritative, and overwriting either would destroy state.
fn copy_verified(source: &Path, destination: &Path, subject: &str) -> Result<(), StateError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| StateError::new("read", subject, error))?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .map_err(|error| StateError::new("create directory for", subject, error))?;
        let entries =
            fs::read_dir(source).map_err(|error| StateError::new("read", subject, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| StateError::new("read", subject, error))?;
            copy_verified(&entry.path(), &destination.join(entry.file_name()), subject)?;
        }
        return Ok(());
    }
    let bytes = fs::read(source).map_err(|error| StateError::new("read", subject, error))?;
    match fs::read(destination) {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {
            return Err(StateError::new(
                "convert",
                subject,
                format!(
                    "{} already exists with different content",
                    destination.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StateError::new("read", subject, error)),
    }
    write_bytes_atomic(destination, &bytes, subject)?;
    let written =
        fs::read(destination).map_err(|error| StateError::new("verify", subject, error))?;
    if written != bytes {
        return Err(StateError::new(
            "verify",
            subject,
            format!(
                "{} does not match {}",
                destination.display(),
                source.display()
            ),
        ));
    }
    Ok(())
}

/// Proves the new layout holds everything the legacy layout did, and that the
/// pool document it holds is readable, before anything is removed.
fn verify_conversion(root: &Path, destination: &Path, subject: &str) -> Result<(), StateError> {
    let document = root.join(LEGACY_POOL_PATH);
    if document.is_file() {
        verify_copy(&document, &destination.join(POOL_DOCUMENT), subject)?;
    }
    let legacy = root.join(LEGACY_POOL_DIRECTORY);
    if legacy.is_dir() {
        verify_legacy_directory(&legacy, root, destination, subject)?;
    }
    let pool_document = destination.join(POOL_DOCUMENT);
    if pool_document.is_file() {
        load_state(&pool_document, subject, &validate_pool)?;
    }
    Ok(())
}

fn verify_legacy_directory(
    legacy: &Path,
    root: &Path,
    destination: &Path,
    subject: &str,
) -> Result<(), StateError> {
    let entries =
        fs::read_dir(legacy).map_err(|error| StateError::new("verify", subject, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| StateError::new("verify", subject, error))?;
        let path = entry.path();
        let is_directory = entry
            .file_type()
            .map_err(|error| StateError::new("verify", subject, error))?
            .is_dir();
        if is_directory {
            verify_legacy_directory(
                &path,
                root,
                &legacy_destination(root, destination, &entry.file_name()),
                subject,
            )?;
        } else {
            verify_copy(
                &path,
                &legacy_destination(root, destination, &entry.file_name()),
                subject,
            )?;
        }
    }
    Ok(())
}

fn verify_copy(source: &Path, destination: &Path, subject: &str) -> Result<(), StateError> {
    let bytes = fs::read(source).map_err(|error| StateError::new("verify", subject, error))?;
    let copied =
        fs::read(destination).map_err(|error| StateError::new("verify", subject, error))?;
    if copied != bytes {
        return Err(StateError::new(
            "verify",
            subject,
            format!(
                "{} does not match {}",
                destination.display(),
                source.display()
            ),
        ));
    }
    Ok(())
}

/// Removes the legacy layout once the new one has been verified.
fn remove_legacy_layout(root: &Path, subject: &str) -> Result<(), StateError> {
    remove_file_if_present(
        &root.join(LEGACY_POOL_PATH),
        "remove legacy pool document",
        subject,
    )?;
    match fs::remove_dir_all(root.join(LEGACY_POOL_DIRECTORY)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StateError::new(
            "remove legacy pool directory",
            subject,
            error,
        )),
    }
}

fn write_destroy_marker(directory: &Path, subject: &str) -> Result<(), StateError> {
    let marker = directory.join(DESTROY_MARKER_NAME);
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(marker)
        .map_err(|error| StateError::new("mark destroy", subject, error))?;
    file.sync_all()
        .map_err(|error| StateError::new("sync destroy marker", subject, error))
}

fn write_cleanup_marker(
    directory: &Path,
    worker: &WorkerId,
    subject: &str,
) -> Result<(), StateError> {
    let marker = cleanup_marker_path(directory, worker);
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&marker)
        .map_err(|error| StateError::new("mark cleanup", subject, error))?;
    file.sync_all()
        .map_err(|error| StateError::new("sync cleanup marker", subject, error))
}

fn remove_file_if_present(
    path: &Path,
    operation: &'static str,
    subject: &str,
) -> Result<(), StateError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StateError::new(operation, subject, error)),
    }
}

fn sidecar(path: &Path) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".lock");
    PathBuf::from(sidecar)
}

fn load_state<T: for<'de> Deserialize<'de>>(
    path: &Path,
    subject: &str,
    validate: &impl Fn(&T) -> Result<(), String>,
) -> Result<Loaded<T>, StateError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
        Err(error) => return Err(StateError::new("read", subject, error)),
    };
    decode(bytes, subject, validate)
}

fn decode<T: for<'de> Deserialize<'de>>(
    bytes: Vec<u8>,
    subject: &str,
    validate: &impl Fn(&T) -> Result<(), String>,
) -> Result<Loaded<T>, StateError> {
    let value: T = serde_json::from_slice(&bytes)
        .map_err(|error| StateError::new("decode", subject, error))?;
    validate(&value).map_err(|error| StateError::new("validate", subject, error))?;
    Ok(Loaded::Present(Versioned {
        value,
        revision: StateRevision {
            bytes,
            marker: PhantomData,
        },
    }))
}

fn commit_state<T: Serialize + for<'de> Deserialize<'de>>(
    path: &Path,
    locks: &[ScopedLock],
    subject: &str,
    expected: Expected<T>,
    change: StateChange<T>,
    validate: impl Fn(&T) -> Result<(), String>,
) -> Result<CommitOutcome<T>, StateError> {
    with_locks(locks, subject, || {
        let current = load_state(path, subject, &validate)?;
        let matches = match (&expected, &current) {
            (Expected::Missing, Loaded::Missing) => true,
            (Expected::Match(expected), Loaded::Present(current)) => {
                expected.bytes == current.revision.bytes
            }
            _ => false,
        };
        if !matches {
            return Ok(CommitOutcome::Conflict(current));
        }
        match change {
            StateChange::Replace(value) => {
                validate(&value)
                    .map_err(|error| StateError::new("validate replacement for", subject, error))?;
                write_atomic(path, &value, subject)?;
            }
            StateChange::Remove => match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(StateError::new("remove", subject, error)),
            },
        }
        load_state(path, subject, &validate).map(CommitOutcome::Applied)
    })
}

/// Runs `operation` holding `locks` in the fixed repository-then-pool order.
///
/// The ordering itself lives in [`locks::lock_scoped`], which takes repository
/// locks before pool locks however the caller lists them.
fn with_locks<T>(
    locks: &[ScopedLock],
    subject: &str,
    operation: impl FnOnce() -> Result<T, StateError>,
) -> Result<T, StateError> {
    let _held = locks::lock_scoped(locks).map_err(|error| {
        let operation = match &error {
            LockError::CreateDirectory(_) => "create lock directory for",
            LockError::Open(_) => "open lock for",
            LockError::Lock(_) => "lock",
        };
        StateError::new(operation, subject, error.source())
    })?;
    operation()
}

fn write_atomic<T: Serialize>(path: &Path, value: &T, subject: &str) -> Result<(), StateError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| StateError::new("serialize", subject, error))?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes, subject)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8], subject: &str) -> Result<(), StateError> {
    atomic::write_atomic_bytes(path, bytes).map_err(|error| match &error {
        AtomicWriteError::MissingParent => {
            StateError::new("replace", subject, "state path has no parent")
        }
        AtomicWriteError::CreateDirectory(source) => {
            StateError::new("create state directory for", subject, source)
        }
        AtomicWriteError::CreateTemporary(source) => {
            StateError::new("create temporary file for", subject, source)
        }
        AtomicWriteError::SetPermissions(source) => {
            StateError::new("set temporary permissions for", subject, source)
        }
        AtomicWriteError::Write(source) => StateError::new("write", subject, source),
        AtomicWriteError::Flush(source) => StateError::new("flush", subject, source),
        AtomicWriteError::Sync(source) => StateError::new("sync", subject, source),
        AtomicWriteError::Rename(source) => StateError::new("replace", subject, source),
    })
}

fn validate_pool(pool: &PoolState) -> Result<(), String> {
    if pool.size < 0 {
        return Err("pool size must not be negative".to_owned());
    }
    if pool.size != i64::try_from(pool.workers.len()).map_err(|error| error.to_string())? {
        return Err("pool size does not match Worker count".to_owned());
    }
    let unique: BTreeSet<_> = pool.workers.iter().collect();
    if unique.len() != pool.workers.len() {
        return Err("pool contains duplicate Workers".to_owned());
    }
    Ok(())
}
fn validate_worker(worker: &WorkerState) -> Result<(), String> {
    if worker.status.as_str().is_empty() {
        return Err("Worker status must not be empty".to_owned());
    }
    Ok(())
}
fn validate_in_use(_state: &InUseState) -> Result<(), String> {
    // Worker ids are validated on deserialization; there is nothing else to check.
    Ok(())
}
fn validate_sessions(_state: &SessionState) -> Result<(), String> {
    // Worker ids are validated on deserialization and session ids are opaque
    // strings; there is nothing else to check.
    Ok(())
}
fn validate_undo(_state: &UndoState) -> Result<(), String> {
    // A mutation record's Workspace key is an opaque string (a Workspace name, or
    // the repository-wide marker) and the operation ids are opaque; there is
    // nothing else to check.
    Ok(())
}
fn validate_waiting(_state: &WaitingState) -> Result<(), String> {
    // Ticket ids are opaque strings to this record: it names Tickets back to the
    // owner, and never parses them.
    Ok(())
}
fn validate_dispatch_group_for(
    parent: &TicketId,
    group: &DispatchGroupState,
) -> Result<(), String> {
    if &group.parent != parent {
        return Err("Dispatch Group parent does not match repository identity".to_owned());
    }
    if group.sub_issues.values().any(|issue| issue.retries < 0) {
        return Err("Sub-issue retries must not be negative".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(worker: &str, run_id: &str, cost_micro_usd: Option<u64>) -> RunRecord {
        RunRecord {
            run_id: run_id.to_owned(),
            worker: WorkerId::parse(worker).expect("worker id"),
            ticket: Some("ENG-1".to_owned()),
            workspace: None,
            branch: None,
            runtime: Some("claude".to_owned()),
            provider: None,
            model: None,
            cost_micro_usd,
            usage: None,
            duration_ms: None,
            turns: None,
            conclusion: RunRecordConclusion::Succeeded,
            error: None,
            started_at: None,
            completed_at: None,
        }
    }

    fn store() -> (tempfile::TempDir, RunRecordRepository) {
        let temp = tempfile::tempdir().expect("temp repository");
        fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        let store = repository.state_store().runs();
        (temp, store)
    }

    #[test]
    fn a_missing_store_reads_as_no_records() {
        let (_temp, store) = store();
        assert!(store.records().expect("records").is_empty());
    }

    #[test]
    fn appending_run_records_is_append_only_and_ordered() {
        let (_temp, store) = store();
        store
            .append(&record("worker-01", "worker-01@1", Some(100)))
            .expect("first append");
        store
            .append(&record("worker-01", "worker-01@2", Some(200)))
            .expect("second append");

        let records = store.records().expect("records");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].run_id, "worker-01@1");
        assert_eq!(records[0].cost_micro_usd, Some(100));
        assert_eq!(records[1].run_id, "worker-01@2");
        assert_eq!(records[1].cost_micro_usd, Some(200));

        // The store is never truncated: the raw file still carries both lines.
        let bytes = fs::read(store.directory.join(RUN_RECORDS_NAME)).expect("store bytes");
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 2);
    }

    #[test]
    fn a_record_without_a_cost_round_trips() {
        let (_temp, store) = store();
        store
            .append(&record("worker-01", "worker-01@1", None))
            .expect("append");
        let records = store.records().expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cost_micro_usd, None);
    }

    fn record_for(ticket: Option<&str>, conclusion: RunRecordConclusion) -> RunRecord {
        let mut record = record("worker-01", "worker-01@1", Some(1));
        record.ticket = ticket.map(str::to_owned);
        record.conclusion = conclusion;
        record
    }

    #[test]
    fn completed_tickets_returns_the_latest_success_per_ticket() {
        let (_temp, store) = store();
        store
            .append(&record_for(
                Some("widgets--01-foundation"),
                RunRecordConclusion::Succeeded,
            ))
            .expect("append");
        store
            .append(&record_for(
                Some("widgets--02-dependent"),
                RunRecordConclusion::Failed,
            ))
            .expect("append");
        store
            .append(&record_for(None, RunRecordConclusion::Succeeded))
            .expect("append");

        assert_eq!(
            store.completed_tickets().expect("completed tickets"),
            ["widgets--01-foundation"]
        );
    }

    #[test]
    fn a_ticket_whose_latest_run_failed_is_not_completed() {
        let (_temp, store) = store();
        store
            .append(&record_for(
                Some("widgets--01-foundation"),
                RunRecordConclusion::Succeeded,
            ))
            .expect("append");
        // A re-dispatch that failed: the latest record for the Ticket wins, so a
        // stale success does not resolve it.
        store
            .append(&record_for(
                Some("widgets--01-foundation"),
                RunRecordConclusion::Failed,
            ))
            .expect("append");

        assert!(
            store
                .completed_tickets()
                .expect("completed tickets")
                .is_empty()
        );
    }

    #[test]
    fn an_undecodable_line_does_not_hide_the_rest() {
        let (_temp, store) = store();
        store
            .append(&record("worker-01", "worker-01@1", Some(100)))
            .expect("append");
        let path = store.directory.join(RUN_RECORDS_NAME);
        let mut bytes = fs::read(&path).expect("store bytes");
        bytes.extend_from_slice(b"{\"torn\":");
        bytes.push(b'\n');
        bytes.extend_from_slice(
            &serde_json::to_vec(&record("worker-01", "worker-01@2", Some(200))).expect("line"),
        );
        bytes.push(b'\n');
        fs::write(&path, bytes).expect("write store");

        let records = store.records().expect("records");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cost_micro_usd, Some(100));
        assert_eq!(records[1].cost_micro_usd, Some(200));
    }

    #[test]
    fn concurrent_appends_do_not_interleave_a_partial_line() {
        let (_temp, store) = store();
        let writers = 8;
        let per_writer = 25;
        std::thread::scope(|scope| {
            for writer in 0..writers {
                let store = store.clone();
                scope.spawn(move || {
                    for index in 0..per_writer {
                        store
                            .append(&record(
                                "worker-01",
                                &format!("worker-01@{writer}-{index}"),
                                Some(1),
                            ))
                            .expect("append");
                    }
                });
            }
        });

        let records = store.records().expect("records");
        assert_eq!(records.len(), writers * per_writer);
    }
}
