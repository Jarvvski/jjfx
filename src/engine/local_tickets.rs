//! Local markdown Tickets: a [`TicketQuery`] over the repository's `.scratch/` tree.
//!
//! The source reads jjfx's own ticket convention - `<root>/<feature>/issues/
//! <NN>-<slug>.md` with `Key: value` metadata lines (see
//! `docs/agents/issue-tracker.md`) - so a repository that tracks work locally
//! can dispatch without Linear, an Agent Runtime, or the Pi helper.
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

    /// Finds one Ticket by its path-derived id.
    pub fn ticket(&self, id: &str) -> Result<Option<LocalTicket>, LocalTicketError> {
        Ok(self.scan()?.into_iter().find(|ticket| ticket.id == id))
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
                }));
            }
        }
        Ok(json!({ "tickets": entries }).to_string())
    }

    /// Builds one Ticket's title and body for prompt inlining.
    fn details_payload(&self, id: &TicketId) -> Result<String, TicketQueryError> {
        let tickets = self.scan().map_err(TicketQueryError::from)?;
        let Some(ticket) = tickets.iter().find(|ticket| ticket.id == id.as_str()) else {
            return Err(TicketQueryError::permanent(format!(
                "local Ticket {id} was not found under {}",
                self.root.display()
            )));
        };
        Ok(json!({
            "title": ticket.title,
            "description": ticket.body,
        })
        .to_string())
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
            let blocked_by = ticket
                .blockers
                .as_ref()
                .map(|blockers| {
                    blockers
                        .iter()
                        .map(|number| {
                            tickets
                                .iter()
                                .find(|candidate| {
                                    candidate.feature == ticket.feature
                                        && candidate.number == *number
                                })
                                .map_or_else(|| number.to_string(), |blocker| blocker.id.clone())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
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

        let details = discovery.ticket_details(&id).expect("Ticket details");

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
}
