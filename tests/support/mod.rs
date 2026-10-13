//! Values shared by the integration tests.
//!
//! Each file directly under `tests/` is its own crate, so anything shared has to
//! live in a module like this one and be pulled in with `mod support;`.

use std::time::Duration;

/// Liveness budget for waiting on a helper subprocess.
///
/// This is a guard against a *hang*, not a statement of how long the work is
/// expected to take. The helpers do real `jj`/`git` work - creating
/// repositories, `jj workspace add`, destroy, forge steps - whose wall-clock
/// duration tracks machine load rather than CPU: a single helper has been
/// measured burning only ~1.3s of CPU while taking 7-48s of wall clock while
/// several cargo builds shared the machine. A budget tuned to the *expected*
/// duration therefore goes red under load with no regression behind it, which
/// is the worst kind of failure: a re-run at best, a wrong conclusion at worst.
///
/// So it is deliberately generous - roughly 2.5x the worst legitimate wait
/// observed above. Two minutes is invisible next to the cost of a false red,
/// and a genuinely hung helper still fails its test within this bound instead of
/// hanging the suite forever. Do not "tidy" it back down to a few seconds; that
/// reintroduces the load-sensitive flake this exists to remove.
pub const HELPER_LIVENESS_TIMEOUT: Duration = Duration::from_secs(120);
