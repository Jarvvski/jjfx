//! Pool-level cost and usage, aggregated from the durable per-Run records.
//!
//! Every finalized Run appends a [`RunRecord`] to jjfx's append-only store
//! (ticket 16). This module reads that store once and folds it into the shapes a
//! frontend needs: a pool total, the windows the Worker Pool view shows (today
//! and the last seven days), and a per-Worker split. It is the data behind the
//! pool cost/usage view and behind the optional Dispatch spend cap.
//!
//! The aggregation is a pure fold over decoded records ([`PoolCostSnapshot::from_records`]),
//! so it is exercised without a repository; [`Repository::pool_cost`] is the thin
//! read that feeds it.

use std::collections::BTreeMap;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::engine::{PoolName, Repository, RunRecord, StateError, WorkerId};

/// Cost and usage totalled over a set of recorded Runs.
///
/// A Run whose provider reported no usable cost still counts as a Run, but adds
/// nothing to the cost total and is counted in [`Self::unpriced_runs`], so a
/// reader can tell "spent nothing" from "spend unknown".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolCost {
    runs: u64,
    unpriced_runs: u64,
    cost_micro_usd: u64,
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
}

impl PoolCost {
    /// The number of Runs totalled.
    pub const fn runs(self) -> u64 {
        self.runs
    }

    /// The number of those Runs whose provider reported no usable cost.
    pub const fn unpriced_runs(self) -> u64 {
        self.unpriced_runs
    }

    /// The total cost in millionths of one US dollar.
    pub const fn cost_micro_usd(self) -> u64 {
        self.cost_micro_usd
    }

    /// The total cost in US dollars.
    pub fn cost_usd(self) -> f64 {
        self.cost_micro_usd as f64 / 1_000_000.0
    }

    /// The total cost formatted as `$d.dddd`, the form the cost views show.
    pub fn cost_display(self) -> String {
        usd(self.cost_micro_usd)
    }

    /// The input and output token totals formatted compactly, e.g. `in 1.2k out
    /// 340`.
    pub fn tokens_display(self) -> String {
        format!(
            "in {} out {}",
            compact_tokens(self.input_tokens),
            compact_tokens(self.output_tokens)
        )
    }

    /// The total provider-reported input tokens.
    pub const fn input_tokens(self) -> u64 {
        self.input_tokens
    }

    /// The total input tokens read from a provider cache.
    pub const fn cached_input_tokens(self) -> u64 {
        self.cached_input_tokens
    }

    /// The total input tokens written to a provider cache.
    pub const fn cache_write_input_tokens(self) -> u64 {
        self.cache_write_input_tokens
    }

    /// The total provider-reported output tokens.
    pub const fn output_tokens(self) -> u64 {
        self.output_tokens
    }

    /// The total output tokens used for provider reasoning.
    pub const fn reasoning_output_tokens(self) -> u64 {
        self.reasoning_output_tokens
    }

    /// Folds one recorded Run into the totals.
    fn add(&mut self, record: &RunRecord) {
        self.runs += 1;
        match record.cost_micro_usd {
            Some(cost) => self.cost_micro_usd = self.cost_micro_usd.saturating_add(cost),
            None => self.unpriced_runs += 1,
        }
        if let Some(usage) = record.usage {
            self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
            self.cached_input_tokens = self
                .cached_input_tokens
                .saturating_add(usage.cached_input_tokens);
            self.cache_write_input_tokens = self
                .cache_write_input_tokens
                .saturating_add(usage.cache_write_input_tokens);
            self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
            self.reasoning_output_tokens = self
                .reasoning_output_tokens
                .saturating_add(usage.reasoning_output_tokens);
        }
    }
}

/// The Worker Pool's recorded cost and usage, split the way the Worker Pool view
/// shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolCostSnapshot {
    total: PoolCost,
    today: PoolCost,
    week: PoolCost,
    workers: BTreeMap<WorkerId, PoolCost>,
}

impl PoolCostSnapshot {
    /// Folds decoded Run records into the pool's cost windows.
    ///
    /// `now` anchors the windows and is a parameter so the fold is deterministic
    /// in tests. A record whose `completed_at` is absent or unreadable still
    /// counts in [`Self::total`] and its Worker's split; it just falls outside
    /// the dated windows rather than being dropped.
    pub fn from_records(records: &[RunRecord], now: Timestamp) -> Self {
        let zone = TimeZone::system();
        let today = now.to_zoned(zone.clone()).date();
        let week_start = now - SignedDuration::from_hours(7 * 24);
        let mut snapshot = Self::default();
        for record in records {
            snapshot.total.add(record);
            snapshot
                .workers
                .entry(record.worker.clone())
                .or_default()
                .add(record);
            let completed = record
                .completed_at
                .as_ref()
                .and_then(|stamp| stamp.as_str().parse::<Timestamp>().ok());
            let Some(completed) = completed else {
                continue;
            };
            if completed >= week_start {
                snapshot.week.add(record);
            }
            if completed.to_zoned(zone.clone()).date() == today {
                snapshot.today.add(record);
            }
        }
        snapshot
    }

    /// Every recorded Run.
    pub const fn total(&self) -> PoolCost {
        self.total
    }

    /// Runs completed today, in the operator's local time.
    pub const fn today(&self) -> PoolCost {
        self.today
    }

    /// Runs completed in the last seven days, a rolling window rather than a
    /// calendar week, so the total resets continuously instead of jumping at a
    /// week boundary.
    pub const fn week(&self) -> PoolCost {
        self.week
    }

    /// Per-Worker totals, keyed by Worker.
    pub const fn workers(&self) -> &BTreeMap<WorkerId, PoolCost> {
        &self.workers
    }

    /// The recorded total for one Worker, or nothing when it recorded no Run.
    pub fn worker(&self, worker: &str) -> Option<PoolCost> {
        self.workers
            .iter()
            .find(|(id, _)| id.as_str() == worker)
            .map(|(_, cost)| *cost)
    }

    /// The recorded cost in one spend window - the total a cap over that window
    /// reads, and the number a frontend reports per pool.
    pub fn window(&self, window: SpendWindow) -> PoolCost {
        window.cost(self)
    }
}

impl Repository {
    /// Aggregates this handle's Pool recorded Run costs and usage.
    ///
    /// A handle from [`Repository::open`] reads `default`; one narrowed with
    /// [`Repository::with_pool`] reads the named pool. Reads the durable
    /// per-Run record store; an absent store is an empty aggregate, and a store
    /// that cannot be read is an error.
    pub fn pool_cost(&self) -> Result<PoolCostSnapshot, StateError> {
        let records = self.state_store().runs().records()?;
        Ok(PoolCostSnapshot::from_records(&records, Timestamp::now()))
    }

    /// Aggregates the recorded Run costs of every pool in `pools` into one
    /// Repository-wide snapshot.
    ///
    /// Each pool keeps its own Run Records, so the Repository total is their
    /// union: the per-pool subtotals a frontend reports add up to it. The pool
    /// names are the caller's, because the state layer does not read
    /// `RepoConfig`; a store that cannot be read is an error rather than zero
    /// spend.
    pub fn repository_cost(&self, pools: &[PoolName]) -> Result<PoolCostSnapshot, StateError> {
        let mut records = Vec::new();
        for pool in pools {
            records.extend(self.state_store_for_pool(pool.clone()).runs().records()?);
        }
        Ok(PoolCostSnapshot::from_records(&records, Timestamp::now()))
    }
}

/// The window a spend cap totals over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpendWindow {
    /// Runs completed today, in the operator's local time.
    Today,
    /// Runs completed in the last seven days.
    Week,
    /// Every recorded Run.
    Total,
}

impl SpendWindow {
    /// The spelling the config file uses, so a written declaration reads back
    /// as the same window.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Week => "week",
            Self::Total => "total",
        }
    }

    /// The window a `--window` value names, or `None` when it names none.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "today" => Some(Self::Today),
            "week" => Some(Self::Week),
            "total" => Some(Self::Total),
            _ => None,
        }
    }

    /// The window's total in a snapshot.
    fn cost(self, snapshot: &PoolCostSnapshot) -> PoolCost {
        match self {
            SpendWindow::Today => snapshot.today(),
            SpendWindow::Week => snapshot.week(),
            SpendWindow::Total => snapshot.total(),
        }
    }

    /// The window named in a cap message.
    fn label(self) -> &'static str {
        match self {
            SpendWindow::Today => "today",
            SpendWindow::Week => "over the last 7 days",
            SpendWindow::Total => "in total",
        }
    }
}

/// What Dispatch does once the aggregate reaches the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpendCapMode {
    /// Refuse Dispatch until the aggregate drops below the cap or the cap is
    /// raised.
    Block,
    /// Allow Dispatch and report the overage.
    Warn,
}

impl SpendCapMode {
    /// The spelling the config file uses, so a written declaration reads back
    /// as the same mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }

    /// The mode an `--on-exceed` value names, or `None` when it names none.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "block" => Some(Self::Block),
            "warn" => Some(Self::Warn),
            _ => None,
        }
    }
}

/// Whose recorded spend a limit totals, and whose name a refusal carries.
///
/// A limit is named by the dial that turns it: the Repository ceiling is named
/// as a whole, because a ceiling that has run out is raised by the
/// Repository-wide setting and affects every pool, while a pool cap is named by
/// its pool. Getting this wrong sends the owner to the wrong knob, which is the
/// failure the multi-pool spend design exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendScope {
    /// The Repository-wide ceiling, totalled over every pool's records.
    Repository,
    /// One pool's own cap, totalled over that pool's records.
    Pool(PoolName),
}

impl SpendScope {
    /// How a message names the limit itself.
    fn describe(&self, limit_micro_usd: u64) -> String {
        match self {
            Self::Repository => format!("the Repository spend cap of {}", usd(limit_micro_usd)),
            Self::Pool(name) => format!("the {} spend cap on pool `{name}`", usd(limit_micro_usd)),
        }
    }

    /// The setting that raises or removes the limit.
    fn cap_key(&self) -> String {
        match self {
            Self::Repository => "spend.cap_usd".to_owned(),
            Self::Pool(name) => format!("pools.{name}.cap_usd"),
        }
    }

    /// The setting that changes what Dispatch does at the limit.
    fn mode_key(&self) -> String {
        match self {
            Self::Repository => "spend.on_exceed".to_owned(),
            Self::Pool(name) => format!("pools.{name}.on_exceed"),
        }
    }

    /// The tie-break rank when two limits are equally restrictive: the Repository
    /// wins, because the ceiling bounds every pool rather than just one.
    fn rank(&self) -> u8 {
        match self {
            Self::Repository => 0,
            Self::Pool(_) => 1,
        }
    }
}

/// An optional aggregate spend cap on Dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpendCap {
    limit_micro_usd: u64,
    window: SpendWindow,
    mode: SpendCapMode,
}

impl SpendCap {
    /// Creates a cap of `limit_micro_usd` over `window`, enforced by `mode`.
    pub const fn new(limit_micro_usd: u64, window: SpendWindow, mode: SpendCapMode) -> Self {
        Self {
            limit_micro_usd,
            window,
            mode,
        }
    }

    /// The cap in millionths of one US dollar.
    pub const fn limit_micro_usd(self) -> u64 {
        self.limit_micro_usd
    }

    /// The cap formatted as `$d.dddd`, the form the spend views show.
    pub fn limit_display(self) -> String {
        usd(self.limit_micro_usd)
    }

    /// The window the cap totals over.
    pub const fn window(self) -> SpendWindow {
        self.window
    }

    /// What Dispatch does at the cap.
    pub const fn mode(self) -> SpendCapMode {
        self.mode
    }

    /// The gate decision for the current aggregate, attributed to `scope`.
    ///
    /// The cap fires at or above its limit - a Run at exactly the cap would push
    /// the total past it - and the message names the scope, the current total and
    /// the setting that overrides it.
    pub fn evaluate(self, scope: &SpendScope, snapshot: &PoolCostSnapshot) -> SpendDecision {
        let cost = self.window.cost(snapshot).cost_micro_usd();
        if cost < self.limit_micro_usd {
            return SpendDecision::Allowed;
        }
        let summary = format!(
            "recorded Run spend {} {} exceeds {}",
            usd(cost),
            self.window.label(),
            scope.describe(self.limit_micro_usd)
        );
        match self.mode {
            SpendCapMode::Block => SpendDecision::Refused(format!(
                "{summary}; raise or remove {}, or set {} = \"warn\", to override",
                scope.cap_key(),
                scope.mode_key()
            )),
            SpendCapMode::Warn => SpendDecision::Warned(format!(
                "{summary}; dispatching anyway ({} = \"warn\")",
                scope.mode_key()
            )),
        }
    }
}

/// One spend limit that applies to a Dispatch, paired with the spend it totals.
///
/// A pool cap totals the selected pool's records; the Repository ceiling totals
/// every pool's. [`evaluate_spend`] combines them so both bind. The snapshot is
/// owned rather than borrowed so a caller can build the limits as it reads each
/// store, without a snapshot having to outlive the call.
pub struct SpendLimit {
    scope: SpendScope,
    cap: SpendCap,
    spend: PoolCostSnapshot,
}

impl SpendLimit {
    /// Pairs a cap with the spend it totals and the scope a refusal names.
    pub const fn new(scope: SpendScope, cap: SpendCap, spend: PoolCostSnapshot) -> Self {
        Self { scope, cap, spend }
    }

    /// The decision this one limit reaches against the spend it totals.
    fn decision(&self) -> SpendDecision {
        self.cap.evaluate(&self.scope, &self.spend)
    }
}

/// The combined gate decision across every applicable spend limit.
///
/// Both limits bind, so a Dispatch proceeds only when **every** limit has room.
/// When one or more limits have fired, the **most restrictive** is the one
/// named: a blocking limit outranks a warning one, and among limits at the same
/// severity the smallest limit wins, because the effective limit is their
/// minimum. That names the pool when its own cap is the tighter constraint and
/// the Repository when the ceiling is, so a refusal always points at the dial
/// that unblocks the Dispatch.
///
/// A tie - two equally restrictive limits - names the Repository, because the
/// ceiling bounds every pool rather than just this one.
pub fn evaluate_spend(limits: impl IntoIterator<Item = SpendLimit>) -> SpendDecision {
    let severity = |decision: &SpendDecision| match decision {
        SpendDecision::Refused(_) => 0u8,
        SpendDecision::Warned(_) => 1,
        SpendDecision::Allowed => 2,
    };
    limits
        .into_iter()
        .filter_map(|limit| {
            let decision = limit.decision();
            (!matches!(decision, SpendDecision::Allowed)).then_some((
                limit.cap.limit_micro_usd(),
                limit.scope,
                decision,
            ))
        })
        .min_by_key(|(limit, scope, decision)| (severity(decision), *limit, scope.rank()))
        .map_or(SpendDecision::Allowed, |(_, _, decision)| decision)
}

/// The gate decision for one Dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendDecision {
    /// Under the cap, or no cap configured.
    Allowed,
    /// Over the cap in warn mode: Dispatch proceeds, with this message.
    Warned(String),
    /// Over the cap in block mode: Dispatch is refused, with this message.
    Refused(String),
}

impl SpendDecision {
    /// The message when the cap fired, for a warning or a refusal.
    pub fn message(&self) -> Option<&str> {
        match self {
            SpendDecision::Allowed => None,
            SpendDecision::Warned(message) | SpendDecision::Refused(message) => Some(message),
        }
    }
}

/// Formats millionths of one US dollar as `$d.dddd`.
fn usd(micro_usd: u64) -> String {
    format!("${:.4}", micro_usd as f64 / 1_000_000.0)
}

/// Formats a token count compactly: `340`, `1.2k`, `3.4m`.
fn compact_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => format!("{:.1}m", tokens as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{RunRecordConclusion, RunRecordUsage, WireTimestamp};

    fn record(worker: &str, completed_at: Option<&str>, cost_micro_usd: Option<u64>) -> RunRecord {
        RunRecord {
            run_id: format!("{worker}@{completed_at:?}"),
            worker: WorkerId::parse(worker).expect("worker id"),
            ticket: None,
            workspace: None,
            branch: None,
            runtime: None,
            provider: None,
            model: None,
            cost_micro_usd,
            usage: Some(RunRecordUsage {
                input_tokens: 10,
                cached_input_tokens: 1,
                cache_write_input_tokens: 2,
                output_tokens: 3,
                reasoning_output_tokens: 4,
            }),
            duration_ms: None,
            turns: None,
            conclusion: RunRecordConclusion::Succeeded,
            error: None,
            started_at: None,
            completed_at: completed_at.map(WireTimestamp::new),
        }
    }

    fn now() -> Timestamp {
        "2026-10-15T12:00:00Z".parse().expect("timestamp")
    }

    #[test]
    fn an_empty_store_aggregates_to_nothing() {
        let snapshot = PoolCostSnapshot::from_records(&[], now());
        assert_eq!(snapshot.total(), PoolCost::default());
        assert!(snapshot.workers().is_empty());
    }

    #[test]
    fn totals_split_into_today_week_and_per_worker() {
        let records = [
            // Today, worker-01.
            record("worker-01", Some("2026-10-15T09:00:00Z"), Some(1_000_000)),
            // Three days ago, worker-02: in the week, not today.
            record("worker-02", Some("2026-10-12T09:00:00Z"), Some(2_000_000)),
            // Ten days ago, worker-01: in the total only.
            record("worker-01", Some("2026-10-05T09:00:00Z"), Some(4_000_000)),
        ];

        let snapshot = PoolCostSnapshot::from_records(&records, now());

        assert_eq!(snapshot.total().runs(), 3);
        assert_eq!(snapshot.total().cost_micro_usd(), 7_000_000);
        assert_eq!(snapshot.today().runs(), 1);
        assert_eq!(snapshot.today().cost_micro_usd(), 1_000_000);
        assert_eq!(snapshot.week().runs(), 2);
        assert_eq!(snapshot.week().cost_micro_usd(), 3_000_000);
        assert_eq!(
            snapshot
                .worker("worker-01")
                .expect("worker-01")
                .cost_micro_usd(),
            5_000_000
        );
        assert_eq!(
            snapshot
                .worker("worker-02")
                .expect("worker-02")
                .cost_micro_usd(),
            2_000_000
        );
        assert_eq!(snapshot.worker("worker-03"), None);
        assert_eq!(snapshot.total().input_tokens(), 30);
    }

    #[test]
    fn a_run_without_a_cost_still_counts_but_adds_nothing() {
        let records = [
            record("worker-01", Some("2026-10-15T09:00:00Z"), None),
            record("worker-01", Some("2026-10-15T10:00:00Z"), Some(1_000_000)),
        ];

        let snapshot = PoolCostSnapshot::from_records(&records, now());

        assert_eq!(snapshot.total().runs(), 2);
        assert_eq!(snapshot.total().unpriced_runs(), 1);
        assert_eq!(snapshot.total().cost_micro_usd(), 1_000_000);
        assert_eq!(snapshot.today().runs(), 2);
    }

    #[test]
    fn a_record_without_a_completion_time_stays_in_the_total_only() {
        let records = [record("worker-01", None, Some(1_000_000))];

        let snapshot = PoolCostSnapshot::from_records(&records, now());

        assert_eq!(snapshot.total().runs(), 1);
        assert_eq!(snapshot.today().runs(), 0);
        assert_eq!(snapshot.week().runs(), 0);
        assert_eq!(snapshot.worker("worker-01").expect("worker").runs(), 1);
    }

    #[test]
    fn a_cap_under_its_limit_allows_dispatch() {
        let snapshot = PoolCostSnapshot::from_records(
            &[record(
                "worker-01",
                Some("2026-10-15T09:00:00Z"),
                Some(1_000_000),
            )],
            now(),
        );
        let cap = SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block);
        assert_eq!(
            cap.evaluate(&SpendScope::Repository, &snapshot),
            SpendDecision::Allowed
        );
    }

    #[test]
    fn a_blocking_cap_names_the_cap_and_the_override() {
        let snapshot = PoolCostSnapshot::from_records(
            &[record(
                "worker-01",
                Some("2026-10-15T09:00:00Z"),
                Some(6_000_000),
            )],
            now(),
        );
        let cap = SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block);
        let SpendDecision::Refused(message) = cap.evaluate(&SpendScope::Repository, &snapshot)
        else {
            panic!("expected a refusal");
        };
        assert!(message.contains("$6.0000"), "{message}");
        assert!(message.contains("$5.0000"), "{message}");
        assert!(message.contains("Repository"), "{message}");
        assert!(message.contains("spend.cap_usd"), "{message}");
        assert!(message.contains("spend.on_exceed"), "{message}");
    }

    #[test]
    fn a_pool_cap_names_its_pool_and_the_pools_own_settings() {
        let snapshot = PoolCostSnapshot::from_records(
            &[record(
                "worker-01",
                Some("2026-10-15T09:00:00Z"),
                Some(6_000_000),
            )],
            now(),
        );
        let cap = SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block);
        let scope = SpendScope::Pool(PoolName::parse("bugs").expect("pool name"));
        let SpendDecision::Refused(message) = cap.evaluate(&scope, &snapshot) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("pool `bugs`"), "{message}");
        assert!(message.contains("pools.bugs.cap_usd"), "{message}");
        assert!(message.contains("pools.bugs.on_exceed"), "{message}");
        // The Repository's own settings are not the dial for a pool cap.
        assert!(!message.contains("spend.cap_usd"), "{message}");
    }

    #[test]
    fn a_warning_cap_reports_the_overage_and_proceeds() {
        let snapshot = PoolCostSnapshot::from_records(
            &[record(
                "worker-01",
                Some("2026-10-15T09:00:00Z"),
                Some(6_000_000),
            )],
            now(),
        );
        let cap = SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Warn);
        let SpendDecision::Warned(message) = cap.evaluate(&SpendScope::Repository, &snapshot)
        else {
            panic!("expected a warning");
        };
        assert!(message.contains("dispatching anyway"), "{message}");
    }

    #[test]
    fn a_cap_ignores_older_runs_outside_its_window() {
        // Only a ten-day-old Run: outside today and the week, inside the total.
        let snapshot = PoolCostSnapshot::from_records(
            &[record(
                "worker-01",
                Some("2026-10-05T09:00:00Z"),
                Some(6_000_000),
            )],
            now(),
        );
        let week = SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block);
        assert_eq!(
            week.evaluate(&SpendScope::Repository, &snapshot),
            SpendDecision::Allowed
        );
        let total = SpendCap::new(5_000_000, SpendWindow::Total, SpendCapMode::Block);
        assert!(matches!(
            total.evaluate(&SpendScope::Repository, &snapshot),
            SpendDecision::Refused(_)
        ));
    }

    #[test]
    fn the_repository_reads_the_record_store_into_its_pool_snapshot() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        repository
            .state_store()
            .runs()
            .append(&record("worker-01", None, Some(1_000_000)))
            .expect("append");

        assert_eq!(repository.pool_cost().expect("pool cost").total().runs(), 1);
        let snapshot = repository.read_worker_pool_snapshot();
        assert_eq!(
            snapshot
                .cost()
                .expect("snapshot cost")
                .total()
                .cost_micro_usd(),
            1_000_000
        );
    }

    #[test]
    fn a_snapshot_reads_its_total_through_a_spend_window() {
        let records = [record(
            "worker-01",
            Some("2026-10-15T09:00:00Z"),
            Some(1_000_000),
        )];
        let snapshot = PoolCostSnapshot::from_records(&records, now());

        assert_eq!(
            snapshot.window(SpendWindow::Today).cost_micro_usd(),
            1_000_000
        );
        assert_eq!(
            snapshot.window(SpendWindow::Week).cost_micro_usd(),
            1_000_000
        );
        assert_eq!(
            snapshot.window(SpendWindow::Total).cost_micro_usd(),
            1_000_000
        );
    }

    #[test]
    fn the_repository_total_unions_every_pools_records() {
        let temp = tempfile::tempdir().expect("temp repository");
        std::fs::create_dir(temp.path().join(".jj")).expect("repository marker");
        let repository = Repository::open(temp.path()).expect("repository");
        let default = PoolName::default();
        let bugs = PoolName::parse("bugs").expect("pool name");
        repository
            .state_store()
            .runs()
            .append(&record("worker-01", None, Some(1_000_000)))
            .expect("append default");
        repository
            .state_store_for_pool(bugs.clone())
            .runs()
            .append(&record("worker-02", None, Some(2_000_000)))
            .expect("append bugs");

        let total = repository
            .repository_cost(&[default.clone(), bugs.clone()])
            .expect("repository cost");
        assert_eq!(total.total().runs(), 2);
        assert_eq!(total.total().cost_micro_usd(), 3_000_000);

        // The subtotals add up to the total, one store per pool.
        assert_eq!(
            repository
                .pool_cost()
                .expect("default cost")
                .total()
                .cost_micro_usd(),
            1_000_000
        );
        assert_eq!(
            repository
                .with_pool(bugs)
                .pool_cost()
                .expect("bugs cost")
                .total()
                .cost_micro_usd(),
            2_000_000
        );
    }

    /// A one-Run snapshot costing `cost_micro_usd` inside every window.
    fn spent(cost_micro_usd: u64) -> PoolCostSnapshot {
        PoolCostSnapshot::from_records(
            &[record(
                "worker-01",
                Some("2026-10-15T09:00:00Z"),
                Some(cost_micro_usd),
            )],
            now(),
        )
    }

    fn bugs() -> SpendScope {
        SpendScope::Pool(PoolName::parse("bugs").expect("pool name"))
    }

    #[test]
    fn every_limit_under_its_cap_allows_dispatch() {
        let repository = spent(1_000_000);
        let pool = spent(1_000_000);
        let decision = evaluate_spend([
            SpendLimit::new(
                SpendScope::Repository,
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block),
                repository,
            ),
            SpendLimit::new(
                bugs(),
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block),
                pool,
            ),
        ]);
        assert_eq!(decision, SpendDecision::Allowed);
    }

    #[test]
    fn no_limits_at_all_allow_dispatch() {
        assert_eq!(evaluate_spend([]), SpendDecision::Allowed);
    }

    #[test]
    fn the_tighter_pool_cap_is_named_when_both_limits_fire() {
        // The pool's $5 cap is tighter than the Repository's $10 ceiling, so the
        // pool is the dial - even though the ceiling has also run out.
        let repository = spent(11_000_000);
        let pool = spent(6_000_000);
        let SpendDecision::Refused(message) = evaluate_spend([
            SpendLimit::new(
                SpendScope::Repository,
                SpendCap::new(10_000_000, SpendWindow::Week, SpendCapMode::Block),
                repository,
            ),
            SpendLimit::new(
                bugs(),
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block),
                pool,
            ),
        ]) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("pool `bugs`"), "{message}");
        assert!(!message.contains("Repository"), "{message}");
    }

    #[test]
    fn the_repository_ceiling_is_named_when_the_pool_still_has_room() {
        // The pool is well under its cap; only the ceiling has run out, so the
        // refusal names the Repository rather than sending the owner to a pool.
        let repository = spent(11_000_000);
        let pool = spent(1_000_000);
        let SpendDecision::Refused(message) = evaluate_spend([
            SpendLimit::new(
                SpendScope::Repository,
                SpendCap::new(10_000_000, SpendWindow::Week, SpendCapMode::Block),
                repository,
            ),
            SpendLimit::new(
                bugs(),
                SpendCap::new(50_000_000, SpendWindow::Week, SpendCapMode::Block),
                pool,
            ),
        ]) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("Repository"), "{message}");
        assert!(message.contains("spend.cap_usd"), "{message}");
        assert!(!message.contains("pool `bugs`"), "{message}");
    }

    #[test]
    fn a_blocking_limit_outranks_a_warning_one() {
        // The pool is in warn mode, the ceiling blocks: the Dispatch is refused,
        // because every limit has to have room for it to proceed.
        let repository = spent(11_000_000);
        let pool = spent(6_000_000);
        let SpendDecision::Refused(message) = evaluate_spend([
            SpendLimit::new(
                SpendScope::Repository,
                SpendCap::new(10_000_000, SpendWindow::Week, SpendCapMode::Block),
                repository,
            ),
            SpendLimit::new(
                bugs(),
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Warn),
                pool,
            ),
        ]) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("Repository"), "{message}");
    }

    #[test]
    fn a_lone_warning_limit_proceeds_with_its_overage() {
        let repository = spent(1_000_000);
        let pool = spent(6_000_000);
        let SpendDecision::Warned(message) = evaluate_spend([
            SpendLimit::new(
                SpendScope::Repository,
                SpendCap::new(50_000_000, SpendWindow::Week, SpendCapMode::Block),
                repository,
            ),
            SpendLimit::new(
                bugs(),
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Warn),
                pool,
            ),
        ]) else {
            panic!("expected a warning");
        };
        assert!(message.contains("pool `bugs`"), "{message}");
        assert!(message.contains("dispatching anyway"), "{message}");
    }

    #[test]
    fn an_equally_tight_tie_names_the_repository() {
        let repository = spent(6_000_000);
        let pool = spent(6_000_000);
        let SpendDecision::Refused(message) = evaluate_spend([
            SpendLimit::new(
                SpendScope::Repository,
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block),
                repository,
            ),
            SpendLimit::new(
                bugs(),
                SpendCap::new(5_000_000, SpendWindow::Week, SpendCapMode::Block),
                pool,
            ),
        ]) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("Repository"), "{message}");
    }
}
