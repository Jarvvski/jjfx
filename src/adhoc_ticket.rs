//! A Ticket handed in directly: free text, a file, or stdin.
//!
//! This is an *input* to the Direct Dispatch path, not a third Ticket source.
//! It never queries a tracker, so `dispatch --text` works in a repository with
//! no `[tickets]` configuration and no `.scratch/` tree - the case that
//! otherwise fails on a Linear key the repository does not have.
//!
//! The result is the same [`Ticket`] plus [`TicketDetails`] a configured source
//! would supply, so the Worker prompt is identical whichever input was used.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::engine::{Ticket, TicketDetails, TicketId, TicketStatus, TicketTitle};

/// How many words of free text contribute to a default Ticket id.
const ID_WORDS: usize = 6;

/// Marks a default id as ad-hoc, so it is obvious in Pool state and the prompt.
const ID_PREFIX: &str = "ad-hoc--";

/// Where an ad-hoc Ticket's text comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdHocSource {
    /// `--text "<free text>"`.
    Text(String),
    /// `--from-file <path>`.
    File(PathBuf),
    /// `-`, or `--from-file -`: the Ticket is read from stdin.
    Stdin,
}

/// An ad-hoc Ticket request: where its text comes from, and an optional id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdHocInput {
    /// The text, file, or stdin the Ticket is read from.
    pub(crate) source: AdHocSource,
    /// `--id`, overriding the derived default when present.
    pub(crate) id: Option<String>,
}

/// A Ticket handed in directly, ready for the Direct Dispatch path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdHocTicket {
    id: TicketId,
    title: TicketTitle,
    body: String,
}

impl AdHocTicket {
    /// Reads the input and builds its Ticket.
    ///
    /// A file that cannot be read, empty text, or an unusable `--id` is an
    /// error, so a bad input fails here rather than mid-Dispatch.
    pub(crate) fn read(input: &AdHocInput) -> Result<Self> {
        let (content, default_slug) = match &input.source {
            AdHocSource::Text(text) => (text.clone(), None),
            AdHocSource::File(path) => (
                fs::read_to_string(path)
                    .with_context(|| format!("cannot read Ticket file {}", path.display()))?,
                Some(slug_of_stem(path)),
            ),
            AdHocSource::Stdin => (read_stdin()?, None),
        };
        Self::parse(&content, default_slug.as_deref(), input.id.as_deref())
    }

    /// Builds a Ticket from already-read text.
    ///
    /// The first `# ` heading is the title and the rest is the body, matching
    /// the local Ticket convention. With no heading the first non-empty line
    /// doubles as the title and the whole text is the body, so a one-line
    /// Ticket still carries its text.
    fn parse(content: &str, default_slug: Option<&str>, id: Option<&str>) -> Result<Self> {
        let (title, body) = split_title_and_body(content);
        if title.is_empty() {
            anyhow::bail!("the Ticket text is empty");
        }
        let id = match id {
            Some(id) => id.to_owned(),
            None => format!(
                "{ID_PREFIX}{}",
                default_slug.map_or_else(|| slug(&first_words(&title)), str::to_owned)
            ),
        };
        let id = TicketId::parse(id)
            .with_context(|| "--id must be a single, non-empty filename component")?;
        Ok(Self {
            id,
            title: TicketTitle::parse(title)?,
            body,
        })
    }

    /// The Ticket the Dispatch carries, for Pool state and the prompt.
    pub(crate) fn ticket(&self) -> Ticket {
        Ticket::new(
            self.id.clone(),
            self.title.clone(),
            TicketStatus::parse("Todo").expect("`Todo` is a non-blank Ticket status"),
        )
    }

    /// The title and body inlined into the Worker prompt.
    pub(crate) fn details(&self) -> TicketDetails {
        TicketDetails::new(self.id.clone(), self.title.clone(), self.body.clone())
    }
}

/// Reads the whole of stdin as the Ticket text.
fn read_stdin() -> Result<String> {
    let mut content = String::new();
    std::io::stdin()
        .read_to_string(&mut content)
        .context("cannot read the Ticket from stdin")?;
    Ok(content)
}

/// Splits markdown into a title and a body: the first `# ` heading is the
/// title and everything after it is the body.
fn split_title_and_body(content: &str) -> (String, String) {
    let leading = content.trim_start();
    if let Some(rest) = leading.strip_prefix("# ") {
        return match rest.split_once('\n') {
            Some((title, body)) => (title.trim().to_owned(), body.trim().to_owned()),
            None => (rest.trim().to_owned(), String::new()),
        };
    }
    let title = leading
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned();
    (title, content.trim().to_owned())
}

/// The first [`ID_WORDS`] whitespace-separated words of `value`.
fn first_words(value: &str) -> String {
    value
        .split_whitespace()
        .take(ID_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A file's stem as a slug, for a default id derived from the file name.
fn slug_of_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(slug)
        .unwrap_or_default()
}

/// A lowercase, hyphenated slug: runs of non-alphanumeric characters collapse
/// to one hyphen, and leading or trailing hyphens are dropped. Empty when the
/// value carries no alphanumeric character at all.
fn slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut pending_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
    }
    if slug.is_empty() {
        "ticket".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> AdHocInput {
        AdHocInput {
            source: AdHocSource::Text(value.to_owned()),
            id: None,
        }
    }

    #[test]
    fn free_text_becomes_a_titled_ticket_with_the_whole_text_as_its_body() {
        let ticket = AdHocTicket::read(&text("Fix the login bug\n\nIt only happens on Safari.\n"))
            .expect("ad-hoc Ticket");

        assert_eq!(ticket.ticket().id().as_str(), "ad-hoc--fix-the-login-bug");
        assert_eq!(ticket.ticket().title().as_str(), "Fix the login bug");
        assert_eq!(
            ticket.details().description(),
            "Fix the login bug\n\nIt only happens on Safari."
        );
    }

    #[test]
    fn a_markdown_heading_is_the_title_and_the_rest_is_the_body() {
        let ticket = AdHocTicket::read(&text("# Ship the widget\n\n## Problem\n\nDo the work.\n"))
            .expect("ad-hoc Ticket");

        assert_eq!(ticket.ticket().id().as_str(), "ad-hoc--ship-the-widget");
        assert_eq!(ticket.ticket().title().as_str(), "Ship the widget");
        assert_eq!(ticket.details().description(), "## Problem\n\nDo the work.");
    }

    #[test]
    fn an_explicit_id_overrides_the_derived_one() {
        let input = AdHocInput {
            source: AdHocSource::Text("# Whatever\n\nbody".to_owned()),
            id: Some("TOOL-9".to_owned()),
        };
        let ticket = AdHocTicket::read(&input).expect("ad-hoc Ticket");
        assert_eq!(ticket.ticket().id().as_str(), "TOOL-9");
        assert_eq!(ticket.details().id().as_str(), "TOOL-9");
    }

    #[test]
    fn a_file_takes_its_id_from_the_file_name() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("Fix the Login.md");
        std::fs::write(&path, "# Login\n\nBody.\n").expect("Ticket file");
        let input = AdHocInput {
            source: AdHocSource::File(path),
            id: None,
        };

        let ticket = AdHocTicket::read(&input).expect("ad-hoc Ticket");

        assert_eq!(ticket.ticket().id().as_str(), "ad-hoc--fix-the-login");
        assert_eq!(ticket.ticket().title().as_str(), "Login");
        assert_eq!(ticket.details().description(), "Body.");
    }

    #[test]
    fn an_unreadable_file_is_an_error_naming_it() {
        let input = AdHocInput {
            source: AdHocSource::File(PathBuf::from("/nonexistent/ticket.md")),
            id: None,
        };

        let error = AdHocTicket::read(&input).expect_err("a missing file must fail");
        assert!(
            error.to_string().contains("/nonexistent/ticket.md"),
            "{error}"
        );
    }

    #[test]
    fn empty_text_is_rejected() {
        let error = AdHocTicket::read(&text("   \n\n")).expect_err("empty text is not a Ticket");
        assert!(error.to_string().contains("empty"), "{error}");
    }

    #[test]
    fn an_id_that_is_not_a_filename_component_is_rejected() {
        let input = AdHocInput {
            source: AdHocSource::Text("body".to_owned()),
            id: Some("nested/id".to_owned()),
        };
        assert!(AdHocTicket::read(&input).is_err());
    }

    #[test]
    fn a_long_title_is_slugged_to_six_words() {
        let ticket = AdHocTicket::read(&text("one two three four five six seven eight nine"))
            .expect("ad-hoc Ticket");
        assert_eq!(
            ticket.ticket().id().as_str(),
            "ad-hoc--one-two-three-four-five-six"
        );
    }

    #[test]
    fn text_with_no_alphanumeric_title_still_gets_a_usable_id() {
        let ticket = AdHocTicket::read(&text("!!!\n\nbody")).expect("ad-hoc Ticket");
        assert_eq!(ticket.ticket().id().as_str(), "ad-hoc--ticket");
    }
}
