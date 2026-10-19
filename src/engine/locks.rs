//! The two lock scopes a Repository has, and the fixed order between them.
//!
//! A Repository holds several Worker Pools, and the state each mutates splits
//! into two scopes: **repository-scoped** state that every pool shares - the jj
//! Workspace store, `ws-cache`, occupancy, undo, and the legacy-layout
//! conversion - and **pool-scoped** state inside one pool's directory. The
//! scopes have a fixed acquisition order, **repository first, never
//! pool-then-repository**: an operation that held a pool lock while waiting for
//! a repository lock could deadlock against one that holds the repository lock
//! and wants that pool. Two pools provisioning Workspaces at once is the case
//! that makes it matter.
//!
//! The order is enforced here rather than left to convention. [`lock_scoped`]
//! sorts the set of locks it is given so every repository lock is taken before
//! every pool lock, whatever order the caller lists them in, and by path within
//! a scope so two callers that take the same pair can never take it in opposite
//! orders. A bare pair of `flock` calls can do neither. An operation that needs
//! both scopes hands the helper the whole set in one call - nesting a pool lock
//! and then reaching for a repository lock is exactly the order this replaces.
//!
//! Only blocking acquisitions route through here. A non-blocking `try_lock`
//! ([`crate::engine::atomic::try_lock_exclusive`]) cannot deadlock, so it stays
//! outside the ordering - the detached orchestration runner's own lock is the
//! one such caller.

use std::fs::File;
use std::path::PathBuf;

use crate::engine::atomic::{self, LockError};

/// The scope a lock guards, ordered so repository locks are taken first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LockScope {
    /// State every Worker Pool shares.
    Repository,
    /// One Worker Pool's own state, inside its directory.
    Pool,
}

/// One lock to take, tagged with the scope that fixes its acquisition order.
#[derive(Debug, Clone)]
pub(crate) struct ScopedLock {
    scope: LockScope,
    path: PathBuf,
}

impl ScopedLock {
    /// A lock guarding repository-scoped state.
    pub(crate) fn repository(path: impl Into<PathBuf>) -> Self {
        Self {
            scope: LockScope::Repository,
            path: path.into(),
        }
    }

    /// A lock guarding one Worker Pool's state.
    pub(crate) fn pool(path: impl Into<PathBuf>) -> Self {
        Self {
            scope: LockScope::Pool,
            path: path.into(),
        }
    }
}

/// Locks held in the fixed repository-then-pool order.
///
/// Dropping the set releases every lock. The set may be moved between threads -
/// a Worker operation guard is handed to a restoration thread - because a held
/// advisory lock is owned by the file, not the thread.
#[derive(Debug)]
pub(crate) struct LockSet {
    _files: Vec<File>,
}

/// Takes `locks` in the fixed order: every repository lock, then every pool
/// lock, each group sorted by path.
///
/// Returns the held set; dropping it releases the locks. The ordering is a
/// property of this call, so a caller that needs a repository lock and a pool
/// lock together must pass both here rather than nest two calls.
pub(crate) fn lock_scoped(locks: &[ScopedLock]) -> Result<LockSet, LockError> {
    let ordered = ordered(locks);
    let mut files = Vec::with_capacity(ordered.len());
    for lock in &ordered {
        files.push(atomic::lock_exclusive(&lock.path)?);
    }
    Ok(LockSet { _files: files })
}

/// `locks` deduplicated and sorted into acquisition order.
///
/// Sorting by scope first is the whole rule: repository locks before pool locks.
fn ordered(locks: &[ScopedLock]) -> Vec<ScopedLock> {
    let mut ordered = locks.to_vec();
    ordered.sort_by(|left, right| {
        left.scope
            .cmp(&right.scope)
            .then_with(|| left.path.cmp(&right.path))
    });
    ordered.dedup_by(|left, right| left.path == right.path);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::LIVENESS_TIMEOUT;
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// How long the ordering test lets a reversed acquisition misbehave before
    /// it looks.
    ///
    /// This is an observation window, not a liveness budget, so it is not
    /// [`LIVENESS_TIMEOUT`]: a reversed order takes the pool lock within
    /// milliseconds of the helper starting, so a second is ample to let it show.
    const REVERSED_ORDER_WINDOW: Duration = Duration::from_secs(1);

    /// How long the ordering test keeps sampling the pool lock for it to read as
    /// free.
    ///
    /// A reversed order holds the pool lock for the *whole* wait - the helper
    /// blocks on the repository lock while holding it - so it never clears; a
    /// momentary `Busy` from an unrelated holder on a shared machine clears well
    /// inside this.
    const POOL_FREE_SAMPLE: Duration = Duration::from_millis(100);

    fn file_names(locks: &[ScopedLock]) -> Vec<String> {
        ordered(locks)
            .into_iter()
            .map(|lock| {
                lock.path
                    .file_name()
                    .expect("lock file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    /// The rule itself: a pool lock listed before the repository lock must still
    /// be ordered after it. Reversing the sort makes this fail, which is what
    /// makes the ordering checkable rather than conventional.
    #[test]
    fn a_pool_lock_listed_first_is_still_ordered_after_the_repository_lock() {
        assert_eq!(
            file_names(&[
                ScopedLock::pool("/pool/.dispatch.lock"),
                ScopedLock::repository("/.repository.lock"),
                ScopedLock::pool("/pool/worker-01.json.lock"),
                ScopedLock::repository("/ws-cache.lock"),
            ]),
            [
                ".repository.lock",
                "ws-cache.lock",
                ".dispatch.lock",
                "worker-01.json.lock",
            ],
            "every repository lock must precede every pool lock"
        );
    }

    /// The reverse of the ordering rule, caught at the acquisition itself: while
    /// the repository lock is held, a thread asking for both must block on the
    /// repository lock and must *not* be holding the pool lock. A reversed order
    /// would take the pool lock first, and the assertion would see it busy.
    #[test]
    fn a_blocked_acquisition_does_not_hold_the_pool_lock_while_it_waits() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let repository_lock = directory.path().join("repository.lock");
        let pool_lock = directory.path().join("pool.lock");

        let held = atomic::lock_exclusive(&repository_lock).expect("hold the repository lock");
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let thread_pool = pool_lock.clone();
        let thread_repository = repository_lock.clone();
        let handle = thread::spawn(move || {
            // Listed pool-first on purpose: the helper must still take the
            // repository lock first.
            let set = lock_scoped(&[
                ScopedLock::pool(thread_pool),
                ScopedLock::repository(thread_repository),
            ])
            .expect("locks");
            acquired_tx.send(()).expect("report acquisition");
            drop(set);
        });

        // Give a reversed order time to take the pool lock and start waiting on
        // the repository lock: it would then hold the pool lock for the rest of
        // the wait, because the repository lock is held here until after the
        // check below. The correct order leaves the pool lock free.
        thread::sleep(REVERSED_ORDER_WINDOW);

        // Then the pool lock must be free. Sampling rather than a single probe -
        // and requiring it to *clear* rather than never reading `Busy` - is what
        // makes this a statement about the order and not about the machine: a
        // reproduction under a six-way full-suite load saw a momentary `Busy`
        // clear within 0.5-4ms while the helper was correctly blocked on the
        // repository lock, and a busy poll that failed on the first `Busy`
        // turned that into a false failure. Only a lock that stays held is the
        // reversed order.
        let sample_deadline = std::time::Instant::now() + POOL_FREE_SAMPLE;
        let mut pool_was_free = false;
        loop {
            match atomic::try_lock_exclusive(&pool_lock).expect("try the pool lock") {
                atomic::LockOutcome::Acquired(file) => {
                    drop(file);
                    pool_was_free = true;
                    break;
                }
                atomic::LockOutcome::Busy => {
                    if std::time::Instant::now() >= sample_deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
        assert!(
            acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "the acquisition must still be blocked on the repository lock"
        );
        drop(held);
        acquired_rx
            .recv_timeout(LIVENESS_TIMEOUT)
            .expect("the blocked acquisition completes once the repository lock is released");
        handle.join().expect("thread");
        assert!(
            pool_was_free,
            "the pool lock was taken while the repository lock was held"
        );
    }

    /// Two pools provisioning at once: each round takes the shared repository
    /// lock and its own pool lock, then does a read-modify-write on the shared
    /// repository document and on its own pool document. The test proves the
    /// ordering neither deadlocks (both threads finish) nor corrupts the shared
    /// document (no lost update).
    #[test]
    fn two_pools_provision_concurrently_without_deadlock_or_lost_updates() {
        const ROUNDS: usize = 200;

        let directory = tempfile::tempdir().expect("temporary directory");
        let repository_lock = directory.path().join("repository.lock");
        let shared = directory.path().join("shared-repository-state");
        fs::write(&shared, "0").expect("seed shared state");

        let (done_tx, done_rx) = mpsc::channel();
        let mut handles = Vec::new();
        for name in ["pool-a", "pool-b"] {
            let pool_lock = directory.path().join(format!("{name}.lock"));
            let pool_state = directory.path().join(format!("{name}-state"));
            let repository_lock = repository_lock.clone();
            let shared = shared.clone();
            let done_tx = done_tx.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..ROUNDS {
                    let _set = lock_scoped(&[
                        ScopedLock::repository(repository_lock.clone()),
                        ScopedLock::pool(pool_lock.clone()),
                    ])
                    .expect("locks");
                    let current: usize = fs::read_to_string(&shared)
                        .expect("read shared state")
                        .trim()
                        .parse()
                        .expect("shared number");
                    fs::write(&shared, (current + 1).to_string()).expect("write shared state");
                    let current: usize = fs::read_to_string(&pool_state)
                        .ok()
                        .and_then(|text| text.trim().parse().ok())
                        .unwrap_or(0);
                    fs::write(&pool_state, (current + 1).to_string()).expect("write pool state");
                }
                done_tx.send(()).expect("report completion");
            }));
        }

        for _ in 0..2 {
            done_rx
                .recv_timeout(LIVENESS_TIMEOUT)
                .expect("neither pool deadlocks");
        }
        for handle in handles {
            handle.join().expect("thread");
        }
        assert_eq!(
            fs::read_to_string(&shared)
                .expect("read shared state")
                .trim(),
            (ROUNDS * 2).to_string(),
            "the repository lock must serialise the shared read-modify-write"
        );
    }
}
