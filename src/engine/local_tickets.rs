//! Local markdown Tickets: a [`TicketQuery`] over the repository's `.scratch/` tree.
//!
//! The source reads jjfx's own ticket convention - `<root>/<feature>/issues/
//! <NN>-<slug>.md` with `Key: value` metadata lines (see
//! `docs/agents/issue-tracker.md`) - so a repository that tracks work locally
//! can dispatch without Linear or an Agent Runtime.
//!
//! Ticket identity is path-derived: `<feature>--<NN>-<slug>`. A local Ticket id
//! must be a single [`TicketId`] component (it names Dispatch Group state files),
//! so the feature is joined to the file stem with `--` rather than the path
//! separator the directory layout uses.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;
use thiserror::Error;

use crate::engine::{
    ReadyTicketFilter, TicketId, TicketQuery, TicketQueryError, TicketQueryRequest,
};

/// The `issues` directory holding one feature's Ticket files.
const ISSUES_DIRECTORY: &str = "issues";

/// The status a local Ticket carries while a Worker owns it.
pub const CLAIMED_STATUS: &str = "claimed";

/// A local markdown Ticket source rooted at one directory.
#[derive(Debug, Clone)]
pub struct LocalTickets {
    root: PathBuf,
    ready_status: String,
    resolved_status: String,
}

impl LocalTickets {
    /// Creates a source rooted at `<root>/<feature>/issues/<NN>-<slug>.md`.
    pub fn new(
        root: impl Into<PathBuf>,
        ready_status: impl Into<String>,
        resolved_status: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            ready_status: ready_status.into(),
            resolved_status: resolved_status.into(),
        }
    }

    /// Returns the directory the Tickets are read from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Scans every local Ticket file, sorted by feature then file name.
    ///
    /// A missing root is an empty set, not an error: a repository with no
    /// `.scratch/` tree simply has no local Tickets.
    pub fn scan(&self) -> Result<Vec<LocalTicket>, LocalTicketError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(self.read_error(&self.root, error)),
        };
        let mut features = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| self.read_error(&self.root, error))?
                .path();
            if path.is_dir() {
                features.push(path);
            }
        }
        features.sort();
        let mut tickets = Vec::new();
        for feature in features {
            let issues = feature.join(ISSUES_DIRECTORY);
            if !issues.is_dir() {
                continue;
            }
            let Some(feature) = feature.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let mut paths = Vec::new();
            for entry in fs::read_dir(&issues).map_err(|error| self.read_error(&issues, error))? {
                let path = entry
                    .map_err(|error| self.read_error(&issues, error))?
                    .path();
                if path.is_file() {
                    paths.push(path);
                }
            }
            paths.sort();
            for path in paths {
                if let Some(ticket) = self.scan_file(feature, &path)? {
                    tickets.push(ticket);
                }
            }
        }
        Ok(tickets)
    }

    /// Claims the Tickets in-flight Workers own and resolves the Tickets whose
    /// latest durable Run Record succeeded.
    ///
    /// The completion signal comes from the Run Record store (`completed`), not
    /// live Worker state, because `pool reset` clears a Worker's Ticket
    /// association: a Worker that finished and was reset would otherwise never
    /// resolve its Ticket. A Ticket a live Worker still owns is in flight, not
    /// finished, so a claim wins and a re-dispatched Ticket is never resolved from
    /// a stale record. Failures are collected rather than raised: the writeback is
    /// a side effect of a command that already succeeded, so the caller decides
    /// how loudly to report it. A Ticket that is not a local file is skipped - a
    /// Worker can own a Ticket named by hand, or one left over from another
    /// source.
    pub fn write_back(&self, in_flight: &[&str], completed: &[&str]) -> Vec<LocalTicketError> {
        let mut failures = Vec::new();
        for ticket in in_flight {
            match self.claim(ticket) {
                Ok(_) => {}
                Err(LocalTicketError::NotFound { .. }) => {}
                Err(error) => failures.push(error),
            }
        }
        for ticket in completed {
            if in_flight.contains(ticket) {
                continue;
            }
            match self.resolve(ticket) {
                Ok(_) => {}
                Err(LocalTicketError::NotFound { .. }) => {}
                Err(error) => failures.push(error),
            }
        }
        failures
    }

    /// Finds one Ticket by its path-derived id.
    pub fn ticket(&self, id: &str) -> Result<Option<LocalTicket>, LocalTicketError> {
        Ok(self.scan()?.into_iter().find(|ticket| ticket.id == id))
    }

    /// Marks a Ticket as `claimed` when a Worker reserves it.
    ///
    /// Only the `Status:` line is rewritten, so every other byte of the file is
    /// preserved. A Ticket that is already claimed, or already resolved, is left
    /// alone rather than downgraded.
    pub fn claim(&self, id: &str) -> Result<LocalTicketUpdate, LocalTicketError> {
        let Some(ticket) = self.ticket(id)? else {
            return Err(self.not_found(id));
        };
        if status_eq(&ticket.status, CLAIMED_STATUS)
            || status_eq(&ticket.status, &self.resolved_status)
        {
            return Ok(LocalTicketUpdate::Unchanged);
        }
        self.rewrite_status(&ticket, CLAIMED_STATUS)
    }

    /// Marks a Ticket as resolved and appends a dated `## Comments` line.
    ///
    /// Idempotent: a Ticket that is already resolved is left untouched, so a
    /// repeated reconcile never appends the comment twice.
    pub fn resolve(&self, id: &str) -> Result<LocalTicketUpdate, LocalTicketError> {
        let Some(ticket) = self.ticket(id)? else {
            return Err(self.not_found(id));
        };
        if status_eq(&ticket.status, &self.resolved_status) {
            return Ok(LocalTicketUpdate::Unchanged);
        }
        let content = self.read(&ticket)?;
        let content = set_status_line(&content, &self.resolved_status)
            .ok_or_else(|| self.missing_status(id))?;
        let comment = format!("- {}: Resolved by jjfx dispatch.", today());
        let content = append_comment(&content, &comment);
        self.write(&ticket, &content)?;
        Ok(LocalTicketUpdate::Applied)
    }

    /// Rewrites only a Ticket's `Status:` line.
    fn rewrite_status(
        &self,
        ticket: &LocalTicket,
        status: &str,
    ) -> Result<LocalTicketUpdate, LocalTicketError> {
        let content = self.read(ticket)?;
        let Some(content) = set_status_line(&content, status) else {
            return Err(self.missing_status(&ticket.id));
        };
        self.write(ticket, &content)?;
        Ok(LocalTicketUpdate::Applied)
    }

    fn read(&self, ticket: &LocalTicket) -> Result<String, LocalTicketError> {
        fs::read_to_string(&ticket.path).map_err(|source| LocalTicketError::Write {
            path: ticket.path.clone(),
            source,
        })
    }

    fn write(&self, ticket: &LocalTicket, content: &str) -> Result<(), LocalTicketError> {
        fs::write(&ticket.path, content).map_err(|source| LocalTicketError::Write {
            path: ticket.path.clone(),
            source,
        })
    }

    fn not_found(&self, id: &str) -> LocalTicketError {
        LocalTicketError::NotFound {
            id: id.to_owned(),
            root: self.root.clone(),
        }
    }

    fn missing_status(&self, id: &str) -> LocalTicketError {
        LocalTicketError::MissingStatus { id: id.to_owned() }
    }

    /// Parses one Ticket file, or `None` when its name is not `<NN>-<slug>.md`.
    fn scan_file(
        &self,
        feature: &str,
        path: &Path,
    ) -> Result<Option<LocalTicket>, LocalTicketError> {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(stem) = name.strip_suffix(".md") else {
            return Ok(None);
        };
        let Some((number, slug)) = stem.split_once('-') else {
            return Ok(None);
        };
        if number.is_empty() || slug.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            return Ok(None);
        }
        let Ok(number) = number.parse::<u32>() else {
            return Ok(None);
        };
        let content = fs::read_to_string(path).map_err(|error| self.read_error(path, error))?;
        Ok(Some(LocalTicket::parse(
            feature, number, slug, path, &content,
        )))
    }

    fn read_error(&self, path: &Path, source: io::Error) -> LocalTicketError {
        LocalTicketError::Read {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Builds the Ready Ticket payload for the shared discovery filter.
    fn ready_payload(&self, filter: &ReadyTicketFilter) -> Result<String, TicketQueryError> {
        let tickets = self.scan().map_err(TicketQueryError::from)?;
        let mut entries = Vec::new();
        let mut diagnostics = Vec::new();
        for ticket in &tickets {
            if let Some(malformation) = ticket.malformation() {
                // A malformed file must never be dropped silently: emit it so
                // `TicketDiscovery` reports a diagnostic rather than the ready
                // list collapsing to a plausible-looking zero.
                entries.push(ticket.malformed_entry(filter, malformation));
                continue;
            }
            if self.is_ready(ticket, &tickets) {
                entries.push(json!({
                    "id": ticket.id,
                    "title": ticket.title,
                    "status": filter.status().as_str(),
                    "labels": [filter.label()],
                    "blocked_by": self.blocker_ids(ticket, &tickets),
                }));
                continue;
            }
            // A well-formed Ticket that is not ready is the case the bare `[]`
            // used to hide: name it and say what stops it, so a caller can tell
            // "nothing to do" from "nothing can be done".
            if let Some(reason) = self.not_ready_reason(ticket, &tickets) {
                diagnostics.push(json!({ "subject": ticket.id, "reason": reason }));
            }
        }
        Ok(json!({ "tickets": entries, "diagnostics": diagnostics }).to_string())
    }

    /// Builds one Ticket's title and body for prompt inlining.
    ///
    /// A local feature Parent is a directory rather than a Ticket file, so it has
    /// no details of its own: the id resolves to the JSON `null` sentinel, which
    /// `TicketDiscovery` reads as "no such Ticket" and orchestration then
    /// proceeds to the Parent's children. An id that is neither a Ticket nor a
    /// feature directory stays a hard error, so a mistyped Ticket is not silently
    /// dispatched without its body.
    fn details_payload(&self, id: &TicketId) -> Result<String, TicketQueryError> {
        let tickets = self.scan().map_err(TicketQueryError::from)?;
        if let Some(ticket) = tickets.iter().find(|ticket| ticket.id == id.as_str()) {
            return Ok(json!({
                "title": ticket.title,
                "description": ticket.body,
            })
            .to_string());
        }
        if self.is_feature(id.as_str()) {
            return Ok("null".to_owned());
        }
        Err(TicketQueryError::permanent(format!(
            "local Ticket {id} was not found under {}",
            self.root.display()
        )))
    }

    /// Whether `name` is one of the feature directories the scanner enumerates -
    /// the local Parent, which owns `<root>/<name>/issues/*.md` rather than a
    /// Ticket file of its own.
    fn is_feature(&self, name: &str) -> bool {
        self.root.join(name).join(ISSUES_DIRECTORY).is_dir()
    }

    /// Builds the dependency graph of one feature, whose id is the Parent.
    ///
    /// A local Parent is the feature directory: its direct children are that
    /// feature's Tickets, and each child's `Blocked by:` numbers resolve to the
    /// sibling Tickets they name. A Parent that is not a feature directory has no
    /// children, so orchestration falls back to dispatching it directly.
    fn dependency_payload(&self, parent: &TicketId) -> Result<String, TicketQueryError> {
        let tickets = self.scan().map_err(TicketQueryError::from)?;
        let children = tickets
            .iter()
            .filter(|ticket| ticket.feature == parent.as_str());
        let mut sub_issues = Vec::new();
        for ticket in children {
            let blocked_by = self.blocker_ids(ticket, &tickets);
            sub_issues.push(json!({
                "id": ticket.id,
                "title": ticket.title,
                "status": self.dispatch_vocabulary_status(&ticket.status),
                "blocked_by": blocked_by,
                "cross_repo": false,
            }));
        }
        Ok(json!({ "sub_issues": sub_issues }).to_string())
    }

    /// One Ticket's `Blocked by:` numbers resolved to sibling Ticket ids.
    ///
    /// A number with no sibling in the same feature keeps its bare form, so the
    /// relationship is still reported rather than dropped - the graph then
    /// diagnoses it as an unknown Blocker.
    fn blocker_ids(&self, ticket: &LocalTicket, tickets: &[LocalTicket]) -> Vec<String> {
        ticket
            .blockers
            .as_ref()
            .map(|blockers| {
                blockers
                    .iter()
                    .map(|number| {
                        tickets
                            .iter()
                            .find(|candidate| {
                                candidate.feature == ticket.feature && candidate.number == *number
                            })
                            .map_or_else(|| number.to_string(), |blocker| blocker.id.clone())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Maps a local Ticket status onto the shared Dispatch Group vocabulary.
    ///
    /// The group builder dispatches `todo`-like work and treats terminal work as
    /// delivered, so the local ready status reads as `todo` and a resolved Ticket
    /// reads as `done`; a claimed Ticket is in flight and is skipped.
    fn dispatch_vocabulary_status(&self, status: &str) -> String {
        if status_eq(status, &self.ready_status) {
            "todo".to_owned()
        } else if status_eq(status, &self.resolved_status) {
            "done".to_owned()
        } else if status_eq(status, CLAIMED_STATUS) {
            "in progress".to_owned()
        } else {
            status.to_owned()
        }
    }

    /// Whether one Ticket is ready for Dispatch: its status is the ready status
    /// and every Blocker resolves to a resolved sibling in the same feature.
    fn is_ready(&self, ticket: &LocalTicket, tickets: &[LocalTicket]) -> bool {
        if !status_eq(&ticket.status, &self.ready_status) {
            return false;
        }
        let Some(blockers) = ticket.blockers.as_ref() else {
            return false;
        };
        blockers.iter().all(|number| {
            tickets
                .iter()
                .find(|candidate| {
                    candidate.feature == ticket.feature && candidate.number == *number
                })
                .is_some_and(|blocker| status_eq(&blocker.status, &self.resolved_status))
        })
    }

    /// Why a well-formed local Ticket is not in the Ready set, or `None` when it
    /// is genuinely finished.
    ///
    /// A resolved Ticket is done, so its absence from the Ready set needs no
    /// explanation and it is not reported. Every other Ticket is a candidate that
    /// cannot run right now, and the reason names what stops it: a `Status:` that
    /// is not the ready status, or the unresolved sibling(s) blocking it.
    fn not_ready_reason(&self, ticket: &LocalTicket, tickets: &[LocalTicket]) -> Option<String> {
        if status_eq(&ticket.status, &self.resolved_status) {
            return None;
        }
        if !status_eq(&ticket.status, &self.ready_status) {
            return Some(format!(
                "expected status {:?}, found {:?}",
                self.ready_status, ticket.status
            ));
        }
        let unresolved = ticket
            .blockers
            .as_ref()?
            .iter()
            .filter_map(|number| self.unresolved_blocker(ticket, *number, tickets))
            .collect::<Vec<_>>();
        if unresolved.is_empty() {
            return None;
        }
        Some(format!("blocked by {}", unresolved.join(", ")))
    }

    /// Describes one Blocker that is not yet resolved: its Ticket id and status,
    /// or its bare number when no sibling in the feature declares it.
    fn unresolved_blocker(
        &self,
        ticket: &LocalTicket,
        number: u32,
        tickets: &[LocalTicket],
    ) -> Option<String> {
        match tickets
            .iter()
            .find(|candidate| candidate.feature == ticket.feature && candidate.number == number)
        {
            Some(blocker) if status_eq(&blocker.status, &self.resolved_status) => None,
            Some(blocker) => Some(format!("{} ({})", blocker.id, blocker.status)),
            None => Some(format!("{number} (unknown)")),
        }
    }
}

/// The `TicketQuery` adapter over a local markdown tree.
#[derive(Debug, Clone)]
pub struct LocalMarkdownQuery {
    tickets: LocalTickets,
}

impl LocalMarkdownQuery {
    /// Creates the query over `<root>/<feature>/issues/<NN>-<slug>.md`.
    pub fn new(
        root: impl Into<PathBuf>,
        ready_status: impl Into<String>,
        resolved_status: impl Into<String>,
    ) -> Self {
        Self {
            tickets: LocalTickets::new(root, ready_status, resolved_status),
        }
    }
}

impl TicketQuery for LocalMarkdownQuery {
    fn query(&self, request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        match request {
            TicketQueryRequest::ReadyTickets { filter } => self.tickets.ready_payload(filter),
            TicketQueryRequest::TicketDetails { id } => self.tickets.details_payload(id),
            TicketQueryRequest::DependencyGraph { parent, .. } => {
                self.tickets.dependency_payload(parent.id())
            }
        }
    }
}

/// One parsed Ticket file, with every value the local convention carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTicket {
    id: String,
    feature: String,
    number: u32,
    slug: String,
    title: String,
    body: String,
    status: String,
    /// `None` when the `Blocked by:` value could not be parsed.
    blockers: Option<Vec<u32>>,
    ticket_type: Option<String>,
    path: PathBuf,
}

impl LocalTicket {
    fn parse(feature: &str, number: u32, slug: &str, path: &Path, content: &str) -> Self {
        let lines = content.lines().collect::<Vec<_>>();
        let mut index = 0;
        while index < lines.len() && lines[index].trim().is_empty() {
            index += 1;
        }
        let mut title = String::new();
        if let Some(rest) = lines.get(index).and_then(|line| line.strip_prefix("# ")) {
            title = rest.trim().to_owned();
            index += 1;
        }
        let mut status = String::new();
        let mut blockers = Some(Vec::new());
        let mut ticket_type = None;
        while let Some(line) = lines.get(index) {
            if line.trim().is_empty() {
                index += 1;
                continue;
            }
            let Some((key, value)) = metadata(line) else {
                break;
            };
            match key.to_ascii_lowercase().as_str() {
                "status" => status = value.to_owned(),
                "blocked by" => blockers = parse_blockers(value),
                "type" => ticket_type = Some(value.to_owned()),
                _ => {}
            }
            index += 1;
        }
        let body = lines[index..].join("\n").trim_end().to_owned();
        let number_text = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".md"))
            .and_then(|stem| stem.split_once('-'))
            .map_or_else(|| number.to_string(), |(text, _)| text.to_owned());
        Self {
            id: format!("{feature}--{number_text}-{slug}"),
            feature: feature.to_owned(),
            number,
            slug: slug.to_owned(),
            title,
            body,
            status,
            blockers,
            ticket_type,
            path: path.to_path_buf(),
        }
    }

    /// Returns the path-derived, single-component Ticket id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the feature directory the Ticket lives under.
    pub fn feature(&self) -> &str {
        &self.feature
    }

    /// Returns the numeric part of the Ticket's file name.
    pub const fn number(&self) -> u32 {
        self.number
    }

    /// Returns the Ticket's first `# ` heading, blank when it has none.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the file content below the metadata lines.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Returns the `Status:` value, blank when it has none.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Returns the numeric `Blocked by:` entries, or `None` when unparseable.
    pub fn blockers(&self) -> Option<&[u32]> {
        self.blockers.as_deref()
    }

    /// Returns the `Type:` value, when the Ticket carries one.
    pub fn ticket_type(&self) -> Option<&str> {
        self.ticket_type.as_deref()
    }

    /// Returns the Ticket file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The reason this file violates the Ticket convention, if any.
    pub fn malformation(&self) -> Option<LocalTicketMalformation> {
        if self.title.is_empty() {
            return Some(LocalTicketMalformation::MissingTitle);
        }
        if self.status.is_empty() {
            return Some(LocalTicketMalformation::MissingStatus);
        }
        if self.blockers.is_none() {
            return Some(LocalTicketMalformation::UnparseableBlockers);
        }
        None
    }

    /// A raw entry that `TicketDiscovery` rejects, so a malformed file surfaces
    /// as a diagnostic naming the file instead of being silently dropped.
    fn malformed_entry(
        &self,
        filter: &ReadyTicketFilter,
        malformation: LocalTicketMalformation,
    ) -> serde_json::Value {
        let ready_label = filter.label().to_owned();
        let ready_status = filter.status().as_str().to_owned();
        let (title, status, labels) = match malformation {
            LocalTicketMalformation::MissingTitle => {
                (String::new(), ready_status, vec![ready_label])
            }
            LocalTicketMalformation::MissingStatus => {
                (self.title.clone(), String::new(), vec![ready_label])
            }
            LocalTicketMalformation::UnparseableBlockers => {
                (self.title.clone(), ready_status, Vec::new())
            }
        };
        json!({
            "id": self.id,
            "title": title,
            "status": status,
            "labels": labels,
        })
    }
}

/// How a local Ticket file violates the markdown convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTicketMalformation {
    /// The file has no `# ` heading.
    MissingTitle,
    /// The file has no `Status:` line.
    MissingStatus,
    /// The `Blocked by:` value is neither `none` nor a list of numbers.
    UnparseableBlockers,
}

impl LocalTicketMalformation {
    /// Returns the human-facing reason this file is not a usable Ticket.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::MissingTitle => "missing a `# ` title",
            Self::MissingStatus => "missing a `Status:` line",
            Self::UnparseableBlockers => "`Blocked by:` is not `none` or a list of numbers",
        }
    }
}

/// Splits one `Key: value` metadata line, rejecting anything that is not.
fn metadata(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim_end();
    if key.is_empty() || !key.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-')
    {
        return None;
    }
    Some((key, value.trim()))
}

/// Parses a `Blocked by:` value: `none`, blank, or comma-separated numbers.
fn parse_blockers(value: &str) -> Option<Vec<u32>> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    value
        .split(',')
        .map(|part| part.trim().parse::<u32>().ok())
        .collect()
}

/// Whether two workflow statuses name the same state, ignoring case.
fn status_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Replaces the first `Status:` line, preserving every other byte of the file.
///
/// `None` when the file has no `Status:` line to replace.
fn set_status_line(content: &str, status: &str) -> Option<String> {
    let mut replaced = false;
    let mut updated = String::with_capacity(content.len() + status.len());
    for line in content.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let is_status = metadata(body).is_some_and(|(key, _)| key.eq_ignore_ascii_case("Status"));
        if is_status && !replaced {
            updated.push_str("Status: ");
            updated.push_str(status);
            if line.ends_with('\n') {
                updated.push('\n');
            }
            replaced = true;
        } else {
            updated.push_str(line);
        }
    }
    replaced.then_some(updated)
}

/// Appends a dated bullet under the file's `## Comments` heading, adding the
/// heading when the file has none.
fn append_comment(content: &str, comment: &str) -> String {
    let mut updated = content.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.lines().any(|line| line.trim() == "## Comments") {
        updated.push_str("\n## Comments\n");
    }
    updated.push('\n');
    updated.push_str(comment);
    updated.push('\n');
    updated
}

/// The local date, in the `YYYY-MM-DD` form the ticket convention uses.
fn today() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::system())
        .date()
        .to_string()
}

/// The outcome of a local Ticket writeback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTicketUpdate {
    /// The Ticket file was rewritten.
    Applied,
    /// The Ticket was already in the requested state.
    Unchanged,
}

/// A failure reading or writing the local markdown tree.
#[derive(Debug, Error)]
pub enum LocalTicketError {
    /// A Ticket file or directory could not be read.
    #[error("cannot read local Tickets at {path}: {source}")]
    Read {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// No Ticket with that id exists under the configured root.
    #[error("local Ticket {id} was not found under {root}")]
    NotFound {
        /// The requested Ticket id.
        id: String,
        /// The root that was searched.
        root: PathBuf,
    },
    /// The Ticket file carries no `Status:` line to rewrite.
    #[error("local Ticket {id} has no `Status:` line to update")]
    MissingStatus {
        /// The Ticket whose file is unusable.
        id: String,
    },
    /// A Ticket file could not be written.
    #[error("cannot write local Ticket {path}: {source}")]
    Write {
        /// The path that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
}

impl From<LocalTicketError> for TicketQueryError {
    fn from(error: LocalTicketError) -> Self {
        TicketQueryError::permanent(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::engine::{ParentTicket, RepositoryIdentity, TicketDiscovery, TicketStatus};
    use tempfile::TempDir;

    fn write_ticket(root: &Path, feature: &str, file: &str, content: &str) {
        let directory = root.join(feature).join(ISSUES_DIRECTORY);
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(directory.join(file), content).expect("Ticket file");
    }

    fn source(root: &Path) -> LocalMarkdownQuery {
        LocalMarkdownQuery::new(root, "ready-for-agent", "resolved")
    }

    fn ready_filter() -> ReadyTicketFilter {
        ReadyTicketFilter::new(
            "ready-for-agent",
            TicketStatus::parse("ready-for-agent").expect("ready status"),
        )
        .expect("Ready Ticket filter")
    }

    #[test]
    fn scan_parses_id_title_body_and_metadata() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "03-ship-the-thing.md",
            "# Ship the thing\n\nStatus: ready-for-agent\nType: task\nBlocked by: none\n\n## Problem\n\nDo the work.\n",
        );

        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved")
            .scan()
            .expect("scan");

        assert_eq!(tickets.len(), 1);
        let ticket = &tickets[0];
        assert_eq!(ticket.id(), "widgets--03-ship-the-thing");
        assert_eq!(ticket.feature(), "widgets");
        assert_eq!(ticket.number(), 3);
        assert_eq!(ticket.title(), "Ship the thing");
        assert_eq!(ticket.status(), "ready-for-agent");
        assert_eq!(ticket.ticket_type(), Some("task"));
        assert_eq!(ticket.blockers(), Some(&[][..]));
        assert_eq!(ticket.body(), "## Problem\n\nDo the work.");
        assert!(ticket.malformation().is_none());
    }

    #[test]
    fn non_ticket_files_are_ignored() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(temporary.path(), "widgets", "README.md", "# Not a ticket\n");
        write_ticket(temporary.path(), "widgets", "notes.txt", "free text\n");

        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved")
            .scan()
            .expect("scan");

        assert!(tickets.is_empty());
    }

    #[test]
    fn a_missing_root_is_an_empty_set() {
        let temporary = TempDir::new().expect("temporary directory");
        let tickets = LocalTickets::new(
            temporary.path().join("absent"),
            "ready-for-agent",
            "resolved",
        )
        .scan()
        .expect("scan");
        assert!(tickets.is_empty());
    }

    #[test]
    fn ready_discovery_returns_only_ready_tickets() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-ready.md",
            "# Ready\n\nStatus: ready-for-agent\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-done.md",
            "# Done\n\nStatus: resolved\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert_eq!(ready.tickets().len(), 1);
        assert_eq!(ready.tickets()[0].id().as_str(), "widgets--01-ready");
        assert_eq!(ready.tickets()[0].title().as_str(), "Ready");
        assert!(ready.diagnostics().is_empty(), "{:?}", ready.diagnostics());
    }

    #[test]
    fn details_return_the_title_and_body_for_inlining() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-ready.md",
            "# Ready\n\nStatus: ready-for-agent\n\n## Solution\n\nInline me.\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));
        let id = TicketId::parse("widgets--01-ready").expect("Ticket id");

        let details = discovery
            .ticket_details(&id)
            .expect("Ticket details")
            .expect("a Ticket file has details");

        assert_eq!(details.title().as_str(), "Ready");
        assert_eq!(details.description(), "## Solution\n\nInline me.");
    }

    #[test]
    fn a_malformed_file_surfaces_as_a_diagnostic() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-no-title.md",
            "Status: ready-for-agent\n\nBody without a heading.\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert!(ready.tickets().is_empty());
        assert_eq!(ready.diagnostics().len(), 1, "{:?}", ready.diagnostics());
        assert_eq!(ready.diagnostics()[0].subject(), "widgets--01-no-title");
    }

    #[test]
    fn a_ticket_blocked_by_an_unresolved_sibling_is_not_ready() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: ready-for-agent\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert_eq!(ready.tickets().len(), 1);
        assert_eq!(ready.tickets()[0].id().as_str(), "widgets--01-foundation");
    }

    #[test]
    fn a_blocked_ticket_reports_its_blocker_and_status() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: claimed\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert!(ready.tickets().is_empty(), "{:?}", ready.tickets());
        // Both Tickets explain themselves: 01 is in flight, and 02 is blocked by
        // the in-flight 01.
        assert_eq!(ready.diagnostics().len(), 2, "{:?}", ready.diagnostics());
        let dependent = ready
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.subject() == "widgets--02-dependent")
            .expect("the dependent is reported");
        assert_eq!(
            dependent.reason(),
            "blocked by widgets--01-foundation (claimed)"
        );
    }

    #[test]
    fn a_ticket_with_the_wrong_status_reports_it() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-in-flight.md",
            "# In flight\n\nStatus: claimed\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert!(ready.tickets().is_empty(), "{:?}", ready.tickets());
        assert_eq!(ready.diagnostics().len(), 1, "{:?}", ready.diagnostics());
        assert_eq!(ready.diagnostics()[0].subject(), "widgets--01-in-flight");
        assert_eq!(
            ready.diagnostics()[0].reason(),
            "expected status \"ready-for-agent\", found \"claimed\""
        );
    }

    #[test]
    fn a_ticket_blocked_by_an_unknown_sibling_names_the_number() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 99\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert_eq!(ready.diagnostics().len(), 1, "{:?}", ready.diagnostics());
        assert_eq!(ready.diagnostics()[0].reason(), "blocked by 99 (unknown)");
    }

    #[test]
    fn resolved_tickets_are_not_reported_as_skipped() {
        // A genuinely empty Ready set - everything is done - must not read as
        // "nothing can be done": a resolved Ticket needs no explanation.
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-done.md",
            "# Done\n\nStatus: resolved\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert!(ready.tickets().is_empty(), "{:?}", ready.tickets());
        assert!(ready.diagnostics().is_empty(), "{:?}", ready.diagnostics());
    }

    #[test]
    fn resolving_a_blocker_makes_its_dependent_ready() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: resolved\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        assert_eq!(ready.tickets().len(), 1);
        assert_eq!(ready.tickets()[0].id().as_str(), "widgets--02-dependent");
    }

    #[test]
    fn a_ready_dependent_reports_its_resolved_blocker_by_ticket_id() {
        // The `Blocked by:` number must arrive as the sibling's Ticket id, so
        // `--all` can look up the Blocker's branch and stack on it.
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: resolved\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));

        let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

        let dependent = ready.tickets()[0].id();
        assert_eq!(dependent.as_str(), "widgets--02-dependent");
        assert_eq!(
            ready.blockers(dependent),
            &[TicketId::parse("widgets--01-foundation").expect("Blocker id")]
        );
    }

    #[test]
    fn dependency_graph_maps_blockers_to_sibling_ids() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: resolved\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
        );
        write_ticket(
            temporary.path(),
            "other",
            "01-elsewhere.md",
            "# Elsewhere\n\nStatus: ready-for-agent\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));
        let parent = ParentTicket::new(TicketId::parse("widgets").expect("Parent id"));
        let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

        let graph = discovery
            .dependency_graph(&parent, &repository)
            .expect("dependency graph");

        assert_eq!(graph.sub_issues().len(), 2, "{:?}", graph.diagnostics());
        let dependent = &graph.sub_issues()[&TicketId::parse("widgets--02-dependent").expect("id")];
        assert_eq!(dependent.ticket().status().as_str(), "todo");
        assert_eq!(dependent.blockers().len(), 1);
        assert_eq!(
            dependent.blockers()[0].id().as_str(),
            "widgets--01-foundation"
        );
        assert!(!dependent.is_cross_repository());
        let foundation =
            &graph.sub_issues()[&TicketId::parse("widgets--01-foundation").expect("id")];
        assert_eq!(foundation.ticket().status().as_str(), "done");
    }

    #[test]
    fn a_parent_that_is_not_a_feature_has_no_children() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-foundation.md",
            "# Foundation\n\nStatus: ready-for-agent\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));
        let parent = ParentTicket::new(TicketId::parse("widgets--01-foundation").expect("id"));
        let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

        let graph = discovery
            .dependency_graph(&parent, &repository)
            .expect("dependency graph");

        assert!(graph.sub_issues().is_empty());
    }

    #[test]
    fn an_unknown_blocker_is_reported_in_the_dependency_graph() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 99\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-independent.md",
            "# Independent\n\nStatus: ready-for-agent\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));
        let parent = ParentTicket::new(TicketId::parse("widgets").expect("Parent id"));
        let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

        let graph = discovery
            .dependency_graph(&parent, &repository)
            .expect("dependency graph");

        assert_eq!(graph.sub_issues().len(), 1);
        assert!(
            graph.diagnostics()[0].reason().contains("99"),
            "{:?}",
            graph.diagnostics()
        );
    }

    #[test]
    fn a_feature_whose_only_ticket_is_invalid_is_not_an_empty_graph() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-dependent.md",
            "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 99\n",
        );
        let discovery = TicketDiscovery::new(source(temporary.path()));
        let parent = ParentTicket::new(TicketId::parse("widgets").expect("Parent id"));
        let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

        let error = discovery
            .dependency_graph(&parent, &repository)
            .expect_err("an all-invalid graph is unusable, not empty");

        assert!(error.to_string().contains("unusable"), "{error}");
    }

    fn ticket_path(root: &Path, feature: &str, file: &str) -> std::path::PathBuf {
        root.join(feature).join(ISSUES_DIRECTORY).join(file)
    }

    #[test]
    fn claim_rewrites_only_the_status_line() {
        let temporary = TempDir::new().expect("temporary directory");
        let original = "# Ready\n\nStatus: ready-for-agent\nType: task\nBlocked by: none\n\n## Problem\n\nDo it.\n";
        write_ticket(temporary.path(), "widgets", "01-ready.md", original);
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        let update = tickets.claim("widgets--01-ready").expect("claim");

        assert_eq!(update, LocalTicketUpdate::Applied);
        let rewritten = fs::read_to_string(ticket_path(temporary.path(), "widgets", "01-ready.md"))
            .expect("rewritten file");
        assert_eq!(
            rewritten,
            "# Ready\n\nStatus: claimed\nType: task\nBlocked by: none\n\n## Problem\n\nDo it.\n"
        );
    }

    #[test]
    fn claim_is_a_no_op_once_claimed() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-ready.md",
            "# Ready\n\nStatus: claimed\n",
        );
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        assert_eq!(
            tickets.claim("widgets--01-ready").expect("claim"),
            LocalTicketUpdate::Unchanged
        );
    }

    #[test]
    fn resolve_sets_the_status_and_appends_a_dated_comment() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-ready.md",
            "# Ready\n\nStatus: claimed\n\n## Problem\n\nDo it.\n\n## Comments\n\n- 2026-01-01: Filed.\n",
        );
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        let update = tickets.resolve("widgets--01-ready").expect("resolve");

        assert_eq!(update, LocalTicketUpdate::Applied);
        let rewritten = fs::read_to_string(ticket_path(temporary.path(), "widgets", "01-ready.md"))
            .expect("rewritten file");
        assert!(
            rewritten.starts_with("# Ready\n\nStatus: resolved\n"),
            "{rewritten}"
        );
        assert!(rewritten.contains("- 2026-01-01: Filed.\n"), "{rewritten}");
        assert_eq!(rewritten.matches("## Comments").count(), 1, "{rewritten}");
        assert_eq!(
            rewritten.matches("Resolved by jjfx dispatch.").count(),
            1,
            "{rewritten}"
        );
    }

    #[test]
    fn resolve_adds_a_comments_heading_when_the_file_has_none() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-ready.md",
            "# Ready\n\nStatus: claimed\n",
        );
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        tickets.resolve("widgets--01-ready").expect("resolve");

        let rewritten = fs::read_to_string(ticket_path(temporary.path(), "widgets", "01-ready.md"))
            .expect("rewritten file");
        assert!(rewritten.contains("## Comments\n\n- "), "{rewritten}");
    }

    #[test]
    fn resolve_is_idempotent() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-ready.md",
            "# Ready\n\nStatus: claimed\n",
        );
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        assert_eq!(
            tickets.resolve("widgets--01-ready").expect("resolve"),
            LocalTicketUpdate::Applied
        );
        assert_eq!(
            tickets.resolve("widgets--01-ready").expect("resolve"),
            LocalTicketUpdate::Unchanged
        );

        let rewritten = fs::read_to_string(ticket_path(temporary.path(), "widgets", "01-ready.md"))
            .expect("rewritten file");
        assert_eq!(
            rewritten.matches("Resolved by jjfx dispatch.").count(),
            1,
            "{rewritten}"
        );
    }

    #[test]
    fn writeback_of_an_unknown_ticket_reports_not_found() {
        let temporary = TempDir::new().expect("temporary directory");
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        let error = tickets
            .claim("widgets--01-ready")
            .expect_err("unknown Ticket");

        assert!(
            matches!(error, LocalTicketError::NotFound { .. }),
            "{error}"
        );
    }

    #[test]
    fn write_back_claims_in_flight_tickets_and_resolves_completed_ones() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-running.md",
            "# Running\n\nStatus: ready-for-agent\n",
        );
        write_ticket(
            temporary.path(),
            "widgets",
            "02-finished.md",
            "# Finished\n\nStatus: claimed\n",
        );
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        let failures = tickets.write_back(
            &["widgets--01-running", "widgets--03-not-local"],
            &["widgets--02-finished", "widgets--04-not-local"],
        );

        assert!(failures.is_empty(), "{failures:?}");
        let running = fs::read_to_string(ticket_path(temporary.path(), "widgets", "01-running.md"))
            .expect("running file");
        assert!(running.contains("Status: claimed"), "{running}");
        let finished =
            fs::read_to_string(ticket_path(temporary.path(), "widgets", "02-finished.md"))
                .expect("finished file");
        assert!(finished.contains("Status: resolved"), "{finished}");
        assert!(
            finished.contains("Resolved by jjfx dispatch."),
            "{finished}"
        );
    }

    #[test]
    fn an_in_flight_ticket_is_never_resolved_from_a_stale_completion() {
        let temporary = TempDir::new().expect("temporary directory");
        write_ticket(
            temporary.path(),
            "widgets",
            "01-redispatched.md",
            "# Redispatched\n\nStatus: ready-for-agent\n",
        );
        let tickets = LocalTickets::new(temporary.path(), "ready-for-agent", "resolved");

        // The same Ticket is both in flight (a live Worker owns it) and named by a
        // stale succeeded Run Record; the claim wins, so it is not resolved.
        let failures =
            tickets.write_back(&["widgets--01-redispatched"], &["widgets--01-redispatched"]);

        assert!(failures.is_empty(), "{failures:?}");
        let redispatched = fs::read_to_string(ticket_path(
            temporary.path(),
            "widgets",
            "01-redispatched.md",
        ))
        .expect("redispatched file");
        assert!(redispatched.contains("Status: claimed"), "{redispatched}");
    }
}
