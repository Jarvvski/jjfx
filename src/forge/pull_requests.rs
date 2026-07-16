use super::{Change, GitHubAdapter, NewPullRequest, PullRequest, PullRequestContext};

pub(super) enum Outcome {
    Did,
    Noop(String),
    Failed(String),
}

struct Entry {
    bookmark: String,
    title: String,
    body: String,
    pull_request: Option<PullRequest>,
}

impl Entry {
    fn merged(&self) -> bool {
        self.pull_request
            .as_ref()
            .is_some_and(|pull_request| pull_request.merged)
    }
}

pub(super) fn submit(
    context: &PullRequestContext,
    changes: Vec<Change>,
    draft: bool,
    github: &dyn GitHubAdapter,
) -> Outcome {
    if changes.is_empty() {
        return Outcome::Noop("no bookmark to open a PR".to_string());
    }

    let mut entries = Vec::with_capacity(changes.len());
    for change in changes {
        let pull_request = match github.find(&context.slug, &change.bookmark) {
            Ok(pull_request) => pull_request,
            Err(error) => return Outcome::Failed(error),
        };
        let (title, body) = split_title_body(&change.description);
        entries.push(Entry {
            bookmark: change.bookmark,
            title,
            body,
            pull_request,
        });
    }

    let mut did_work = false;
    let mut resolved = Vec::with_capacity(entries.len());
    for mut entry in entries {
        if entry.pull_request.is_none() {
            let base = base_for(&resolved, resolved.len(), &context.default_branch);
            let title = if entry.title.is_empty() {
                entry.bookmark.clone()
            } else {
                entry.title.clone()
            };
            let request = NewPullRequest {
                slug: context.slug.clone(),
                head: entry.bookmark.clone(),
                base,
                title,
                body: entry.body.clone(),
                draft,
            };
            match github.create(request) {
                Ok(pull_request) => entry.pull_request = Some(pull_request),
                Err(error) => return Outcome::Failed(error),
            }
            did_work = true;
        }
        resolved.push(entry);
    }
    let entries = resolved;

    for (index, entry) in entries.iter().enumerate() {
        let Some(pull_request) = &entry.pull_request else {
            continue;
        };
        if pull_request.merged {
            continue;
        }
        let number = pull_request.number;
        let body = merge_body(&pull_request.body, &stack_section(&entries, index));
        let base = base_for(&entries, index, &context.default_branch);
        if let Err(error) = github.edit(&context.slug, number, &body, &base) {
            return Outcome::Failed(error);
        }
        did_work = true;
    }

    if did_work {
        Outcome::Did
    } else {
        Outcome::Noop("PRs already current".to_string())
    }
}

fn split_title_body(description: &str) -> (String, String) {
    let description = description.trim_end();
    let mut lines = description.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    (title, body)
}

fn base_for(entries: &[Entry], index: usize, default_branch: &str) -> String {
    entries[..index]
        .iter()
        .rev()
        .find(|entry| !entry.merged())
        .map(|entry| entry.bookmark.clone())
        .unwrap_or_else(|| default_branch.to_string())
}

fn stack_section(entries: &[Entry], current: usize) -> String {
    let mut lines = vec!["## Stack".to_string(), String::new()];
    for (index, entry) in entries.iter().enumerate() {
        let Some(pull_request) = &entry.pull_request else {
            continue;
        };
        let marker = if index == current { "👉🏻 " } else { "" };
        let reference = if pull_request.merged {
            format!("~~#{}~~", pull_request.number)
        } else {
            format!("#{}", pull_request.number)
        };
        lines.push(format!("- {marker}{reference}"));
    }
    lines.join("\n")
}

fn merge_body(existing: &str, section: &str) -> String {
    let clean = strip_stack_section(existing);
    let clean = clean.trim();
    if clean.is_empty() {
        section.to_string()
    } else {
        format!("{clean}\n\n{section}")
    }
}

fn strip_stack_section(body: &str) -> String {
    let Some(start) = find_heading(body, "## Stack") else {
        return body.to_string();
    };
    let rest = start + "## Stack".len();
    let end = body[rest..]
        .find("\n## ")
        .map(|offset| rest + offset)
        .unwrap_or(body.len());
    let mut output = String::with_capacity(body.len());
    output.push_str(&body[..start]);
    output.push_str(&body[end..]);
    output
}

fn find_heading(body: &str, heading: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(relative) = body[from..].find(heading) {
        let index = from + relative;
        if index == 0 || body.as_bytes()[index - 1] == b'\n' {
            return Some(index);
        }
        from = index + heading.len();
    }
    None
}
