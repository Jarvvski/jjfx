use super::{Change, GitHubAdapter, NewPullRequest, PullRequestContext};

pub(super) enum Outcome {
    Did,
    Noop(String),
    Failed(String),
}

/// Open one Pull Request per change, against the repository's default branch.
///
/// This is the one blessed forge path: a change either already has a Pull
/// Request, or it gets one. There is no base-chained stack assembly - every Pull
/// Request is based on the default branch, so one bookmark's base is never
/// computed from the bookmarks around it, and nothing is written into the body
/// beyond the change's own description.
pub(super) fn submit(
    context: &PullRequestContext,
    changes: Vec<Change>,
    draft: bool,
    github: &dyn GitHubAdapter,
) -> Outcome {
    if changes.is_empty() {
        return Outcome::Noop("no bookmark to open a PR".to_string());
    }

    let mut did_work = false;
    for change in changes {
        match github.exists(&context.slug, &change.bookmark) {
            // A change that already has a Pull Request - open or merged - is
            // left alone: there is nothing to rewrite.
            Ok(true) => {}
            Ok(false) => {
                let (title, body) = split_title_body(&change.description);
                let title = if title.is_empty() {
                    change.bookmark.clone()
                } else {
                    title
                };
                let request = NewPullRequest {
                    slug: context.slug.clone(),
                    head: change.bookmark,
                    base: context.default_branch.clone(),
                    title,
                    body,
                    draft,
                };
                if let Err(error) = github.create(request) {
                    return Outcome::Failed(error);
                }
                did_work = true;
            }
            Err(error) => return Outcome::Failed(error),
        }
    }

    if did_work {
        Outcome::Did
    } else {
        Outcome::Noop("PRs already current".to_string())
    }
}

/// Split a commit description into a Pull Request title (its first line) and a
/// body (the rest).
fn split_title_body(description: &str) -> (String, String) {
    let description = description.trim_end();
    let mut lines = description.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    (title, body)
}
