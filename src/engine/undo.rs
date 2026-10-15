//! Undo a jjfx-initiated mutation through jj's operation log.
//!
//! jj snapshots every mutation in a repository-wide operation log, and jjfx used
//! none of it: when a Forge, weld, tidy or Reset did something the owner did not
//! want, the only way back was to reason about `jj op log` by hand. This module
//! is the in-tool surface for that.
//!
//! jj's undo primitives are repository-wide, and jjfx drives many Workspaces
//! against the one log, so the scoping is the whole design:
//!
//! - **Primitive: `jj op restore <checkpoint>`**, where `<checkpoint>` is the
//!   operation jjfx records immediately before a mutation. `jj op revert` was
//!   tried first and rejected: it reverses one operation's effect but leaves the
//!   Workspace's working copy on the rewritten chain, and a jjfx mutation is a
//!   sequence of operations (a Forge is fetch, weld, fix, push), so reverting one
//!   of them is not an undo. `jj undo` is rejected too: it is `jj op restore` to
//!   the previous operation, with the same repository-wide reach and no way to
//!   say what it would discard.
//! - **Refuse unless the mutation is still the repository's tip.** With the
//!   checkpoint, jjfx records the operation that was the tip when the mutation
//!   finished. A restore runs only while the current tip is still that recorded
//!   end - nothing at all has happened since, in any Workspace - so the
//!   operations between the checkpoint and the tip are exactly jjfx's own
//!   mutation. When any operation has happened since, the undo is refused and the
//!   refusal reports how many later operations a restore would discard; `force`
//!   is the explicit, warned override.
//!
//! jj does not record who created an operation, so an operation created by
//! another process *inside* a mutation's own window is indistinguishable from
//! jjfx's. Recording per Workspace and refusing on anything later narrows that
//! window to a single mutation; closing it entirely needs jj to tag operations.

use std::path::Path;

use jiff::Timestamp;
use thiserror::Error;

use crate::engine::Repository;
use crate::engine::state::{MutationKind, MutationRecord, StateError, WireTimestamp};

/// The operation-log listing is capped so a huge log cannot make an undo check
/// unbounded. A recorded operation older than the cap reads as "cannot tell",
/// which refuses the undo rather than assuming nothing happened since.
const OPERATION_LOG_LIMIT: usize = 500;

/// A recorded pre-mutation checkpoint, completed once the mutation finishes.
///
/// Holding it proves the checkpoint was read before the mutation ran; the caller
/// passes it back to [`Repository::finish_mutation`] when the mutation is done,
/// successfully or not (the operations that did happen are still undoable).
#[derive(Debug, Clone)]
pub struct MutationCheckpoint {
    workspace: String,
    kind: MutationKind,
    before: String,
}

impl MutationCheckpoint {
    /// The Workspace this mutation targets (or [`REPOSITORY_WIDE_MUTATION`]).
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// What jjfx is about to do.
    pub fn kind(&self) -> MutationKind {
        self.kind
    }
}

/// What an undo did, or refused to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    /// No jjfx-initiated mutation is recorded for the Workspace.
    Nothing,
    /// The repository was restored to the recorded pre-mutation operation.
    Restored {
        /// What jjfx had done.
        kind: MutationKind,
        /// How many of the mutation's own operations the restore discarded.
        discarded: usize,
        /// How many later operations the restore also discarded (zero unless the
        /// undo was forced).
        later: usize,
    },
    /// The mutation is recorded, but the undo was refused.
    Refused {
        /// What jjfx had done.
        kind: MutationKind,
        /// How many operations have happened since the mutation, or `None` when
        /// the recorded end is no longer in the log and jjfx cannot tell.
        later: Option<usize>,
    },
}

/// Errors from recording or undoing a jjfx-initiated mutation.
#[derive(Debug, Error)]
pub enum UndoError {
    /// jj's operation log could not be read.
    #[error("cannot read jj's operation log: {0}")]
    Operation(String),
    /// The undo record store could not be read or written.
    #[error(transparent)]
    State(#[from] StateError),
    /// jj refused the restore.
    #[error("cannot restore the operation: {0}")]
    Restore(String),
}

/// A live mutation checkpoint that records itself when it goes out of scope.
///
/// A mutation has many exits - a failed weld, a refused push, an early
/// `continue` - and every one of them must complete the record, so a guard is
/// the only shape that cannot be forgotten. Recording is best-effort: it must
/// never turn a completed mutation into a reported failure, and an undo that
/// finds no record says so rather than pretending one exists.
pub struct MutationGuard<'a> {
    repository: &'a Repository,
    checkpoint: Option<MutationCheckpoint>,
}

impl<'a> MutationGuard<'a> {
    /// Takes the checkpoint for a mutation of `workspace` that is about to run.
    pub fn begin(repository: &'a Repository, workspace: &str, kind: MutationKind) -> Self {
        let checkpoint = repository.begin_mutation(workspace, kind).ok();
        Self {
            repository,
            checkpoint,
        }
    }
}

impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        if let Some(checkpoint) = self.checkpoint.take() {
            let _ = self.repository.finish_mutation(checkpoint);
        }
    }
}

impl Repository {
    /// Records the checkpoint for a jjfx-initiated mutation of `workspace`.
    ///
    /// Call this immediately before the mutation's first jj command. `workspace`
    /// is a Workspace name, or [`REPOSITORY_WIDE_MUTATION`] for a mutation that
    /// is not confined to one Workspace.
    pub fn begin_mutation(
        &self,
        workspace: &str,
        kind: MutationKind,
    ) -> Result<MutationCheckpoint, UndoError> {
        Ok(MutationCheckpoint {
            workspace: workspace.to_owned(),
            kind,
            before: operation_id(self.root())?,
        })
    }

    /// Completes `checkpoint`: records the mutation with the operation the
    /// repository is at now.
    ///
    /// Call this once the mutation is over, whether it succeeded or failed - the
    /// operations that did happen are exactly what an undo would discard. A
    /// mutation that changed nothing is not recorded, so it cannot displace the
    /// record of the last mutation that did.
    pub fn finish_mutation(&self, checkpoint: MutationCheckpoint) -> Result<(), UndoError> {
        let after = operation_id(self.root())?;
        if after == checkpoint.before {
            return Ok(());
        }
        self.state_store().undo().record(&MutationRecord {
            workspace: checkpoint.workspace,
            kind: checkpoint.kind,
            before: checkpoint.before,
            after,
            at: WireTimestamp::new(Timestamp::now().to_string()),
        })?;
        Ok(())
    }

    /// Undoes the latest jjfx-initiated mutation recorded for `workspace`.
    ///
    /// Refuses (with [`UndoOutcome::Refused`]) when anything has happened since
    /// the mutation, unless `force` is set - the restore is repository-wide and
    /// would otherwise discard another Workspace's work. On success the record is
    /// cleared, so an undo is not offered twice.
    pub fn undo_mutation(&self, workspace: &str, force: bool) -> Result<UndoOutcome, UndoError> {
        let Some(record) = self.state_store().undo().mutation(workspace)? else {
            return Ok(UndoOutcome::Nothing);
        };
        let log = operation_log(self.root())?;
        let position = |operation: &str| log.iter().position(|id| id == operation);
        // The mutation's end must still be in the log: without it jjfx cannot say
        // what a restore would discard, so it must not guess.
        let Some(after) = position(&record.after) else {
            return Ok(UndoOutcome::Refused {
                kind: record.kind,
                later: None,
            });
        };
        // Newest first, so a non-zero index is exactly the count of operations
        // that have happened since the mutation.
        let later = after;
        if later > 0 && !force {
            return Ok(UndoOutcome::Refused {
                kind: record.kind,
                later: Some(later),
            });
        }
        let before = position(&record.before);
        crate::jj::restore_operation(self.root(), &record.before)
            .map_err(|error| UndoError::Restore(error.to_string()))?;
        self.state_store().undo().clear(workspace)?;
        // The mutation's own operations sit between its checkpoint and its end.
        let discarded = before.map_or(0, |before| before.saturating_sub(after));
        Ok(UndoOutcome::Restored {
            kind: record.kind,
            discarded,
            later,
        })
    }
}

/// The repository's newest operation id.
fn operation_id(root: &Path) -> Result<String, UndoError> {
    crate::jj::operation_id(root).map_err(|error| UndoError::Operation(error.to_string()))
}

/// The repository's operation log, newest first, capped at
/// [`OPERATION_LOG_LIMIT`].
fn operation_log(root: &Path) -> Result<Vec<String>, UndoError> {
    crate::jj::operation_log(root, OPERATION_LOG_LIMIT)
        .map_err(|error| UndoError::Operation(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_local_repo;
    use std::path::PathBuf;

    /// Run one mutating jj command in the test repository, asserting it succeeds.
    fn jj(root: &Path, args: &[&str]) {
        let output = crate::jj::command()
            .current_dir(root)
            .args(args)
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The first line of the description of `@-`, the head of the Workspace's
    /// own chain.
    fn description(root: &Path) -> String {
        crate::jj::read_at_repo(
            root,
            &[
                "log",
                "-r",
                "@-",
                "--no-graph",
                "-T",
                "description.first_line()",
            ],
        )
        .expect("description")
        .trim()
        .to_owned()
    }

    /// A repository with one commit, so a mutation has something to rewrite.
    fn seeded(tag: &str) -> (PathBuf, Repository) {
        let root = test_local_repo(tag);
        std::fs::write(root.join("seed.txt"), "seed\n").expect("write seed file");
        jj(&root, &["commit", "-m", "seed"]);
        let repository = Repository::open(&root).expect("repository");
        (root, repository)
    }

    /// Records a mutation that rewrites the head commit's description.
    fn record_rewrite(root: &Path, repository: &Repository, workspace: &str) {
        let checkpoint = repository
            .begin_mutation(workspace, MutationKind::Weld)
            .expect("checkpoint");
        jj(root, &["describe", "-r", "@-", "-m", "rewritten"]);
        repository
            .finish_mutation(checkpoint)
            .expect("finish mutation");
    }

    #[test]
    fn undo_restores_the_recorded_mutation() {
        let (root, repository) = seeded("undo-restore");
        record_rewrite(&root, &repository, "ws-a");
        assert_eq!(description(&root), "rewritten");

        let outcome = repository.undo_mutation("ws-a", false).expect("undo");

        assert!(
            matches!(outcome, UndoOutcome::Restored { kind: MutationKind::Weld, discarded, later: 0 } if discarded >= 1),
            "expected a restore, got {outcome:?}"
        );
        assert_eq!(description(&root), "seed");
    }

    #[test]
    fn a_completed_undo_is_not_offered_twice() {
        let (root, repository) = seeded("undo-once");
        record_rewrite(&root, &repository, "ws-a");
        repository.undo_mutation("ws-a", false).expect("undo");

        assert_eq!(
            repository
                .undo_mutation("ws-a", false)
                .expect("second undo"),
            UndoOutcome::Nothing
        );
    }

    #[test]
    fn undo_refuses_when_anything_happened_since_the_mutation() {
        let (root, repository) = seeded("undo-refuse");
        record_rewrite(&root, &repository, "ws-a");
        // Another process - an agent in another Workspace - does something after
        // the mutation. A repository-wide restore would discard it.
        jj(&root, &["describe", "-r", "@-", "-m", "someone else"]);

        let outcome = repository.undo_mutation("ws-a", false).expect("undo");

        assert!(
            matches!(outcome, UndoOutcome::Refused { kind: MutationKind::Weld, later: Some(later) } if later >= 1),
            "expected a refusal, got {outcome:?}"
        );
        // The record survives a refusal, so the undo is still offered.
        assert!(
            repository
                .state_store()
                .undo()
                .mutation("ws-a")
                .expect("record")
                .is_some()
        );
        assert_eq!(description(&root), "someone else");
    }

    #[test]
    fn a_forced_undo_discards_the_later_operations_and_reports_them() {
        let (root, repository) = seeded("undo-force");
        record_rewrite(&root, &repository, "ws-a");
        jj(&root, &["describe", "-r", "@-", "-m", "someone else"]);

        let outcome = repository.undo_mutation("ws-a", true).expect("undo");

        assert!(
            matches!(outcome, UndoOutcome::Restored { later, .. } if later >= 1),
            "expected a forced restore, got {outcome:?}"
        );
        assert_eq!(description(&root), "seed");
    }

    #[test]
    fn undo_reports_nothing_when_no_mutation_is_recorded() {
        let (_root, repository) = seeded("undo-nothing");
        assert_eq!(
            repository.undo_mutation("ws-a", false).expect("undo"),
            UndoOutcome::Nothing
        );
    }

    #[test]
    fn a_mutation_that_changed_nothing_is_not_recorded() {
        let (_root, repository) = seeded("undo-noop");
        let checkpoint = repository
            .begin_mutation("ws-a", MutationKind::Weld)
            .expect("checkpoint");
        repository
            .finish_mutation(checkpoint)
            .expect("finish mutation");

        assert_eq!(
            repository.undo_mutation("ws-a", false).expect("undo"),
            UndoOutcome::Nothing
        );
    }

    #[test]
    fn the_guard_records_the_mutation_it_brackets_on_an_early_exit() {
        let (root, repository) = seeded("undo-guard");
        {
            let _guard = MutationGuard::begin(&repository, "ws-a", MutationKind::Forge);
            jj(&root, &["describe", "-r", "@-", "-m", "forged"]);
        }

        let record = repository
            .state_store()
            .undo()
            .mutation("ws-a")
            .expect("record")
            .expect("a bracketed mutation is recorded");
        assert_eq!(record.kind, MutationKind::Forge);
        assert_ne!(record.before, record.after);
    }

    #[test]
    fn a_repository_wide_mutation_is_recorded_under_its_own_key() {
        let (root, repository) = seeded("undo-wide");
        let checkpoint = repository
            .begin_mutation(crate::engine::REPOSITORY_WIDE_MUTATION, MutationKind::Tidy)
            .expect("checkpoint");
        jj(&root, &["describe", "-r", "@-", "-m", "tidied"]);
        repository
            .finish_mutation(checkpoint)
            .expect("finish mutation");

        assert_eq!(
            repository.undo_mutation("ws-a", false).expect("undo"),
            UndoOutcome::Nothing,
            "a repository-wide record must not be offered as one Workspace's"
        );
        assert!(matches!(
            repository
                .undo_mutation(crate::engine::REPOSITORY_WIDE_MUTATION, false)
                .expect("undo"),
            UndoOutcome::Restored { .. }
        ));
    }
}
