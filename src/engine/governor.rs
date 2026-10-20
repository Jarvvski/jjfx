//! The dispatch capacity governor: an optional ceiling on how many Runs may be
//! in flight at once.
//!
//! Two dials bound concurrency, and they are not the same dial:
//!
//! - **Worker Pool capacity** is a declaration of *how many Workers exist*. A
//!   pool of N can hold at most N Runs, because every Run needs a Worker to run
//!   on. It is a property of the pool (`jjfx pool N`), sized for the work a lane
//!   is expected to hold.
//! - **The governor** is a ceiling on *how much is in flight*. It is a property
//!   of the Repository's dispatch settings (`[dispatch] max_concurrent`), and it
//!   exists to pace a burst rather than to size a lane: a pool of ten under a cap
//!   of three still fires three.
//!
//! Both bind, and the smaller wins. A pool smaller than the cap is
//! capacity-bound; a cap smaller than the pool is governor-bound, and the excess
//! Tickets are held back rather than fired.
//!
//! The cap counts the Runs already in flight - every Worker the Pool records as
//! busy, however that Run started - so repeated Dispatches stay under the same
//! limit rather than each firing a fresh batch of `limit`. That is what makes it
//! a governor rather than a batch size: it survives across Dispatches.

/// The most Runs Dispatch may keep in flight at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyCap(usize);

impl ConcurrencyCap {
    /// A cap of at least one Run.
    ///
    /// Zero is not a cap: it would hold every Dispatch back forever with nothing
    /// able to release it, so it is rejected rather than silently accepted.
    pub fn new(limit: usize) -> Option<Self> {
        (limit > 0).then_some(Self(limit))
    }

    /// Returns the number of Runs the cap allows in flight.
    pub const fn limit(self) -> usize {
        self.0
    }
}

/// How many of a batch may start now, given the Runs already in flight.
///
/// A Pool already at the ceiling has no room, and a Pool over it (a lowered cap,
/// or Runs started before the cap was configured) has none either rather than a
/// negative count.
pub fn room(cap: ConcurrencyCap, in_flight: usize) -> usize {
    cap.limit().saturating_sub(in_flight)
}

/// Splits a batch into the prefix that may fire now and the rest, held back.
///
/// Input order is preserved on both sides, so the caller's per-Ticket outcomes
/// stay in the order the caller asked for them.
pub fn pace<T>(cap: ConcurrencyCap, in_flight: usize, batch: &[T]) -> (&[T], &[T]) {
    batch.split_at(room(cap, in_flight).min(batch.len()))
}

/// The reason a Ticket the concurrency cap held back carries.
///
/// `in_flight` is the Runs already in flight and `fired` the prefix this batch
/// launches before the cap is reached. The two together are the count that
/// actually bound the batch - the count `status --json`'s `governor.running`
/// reports once the batch has fired. Naming the pre-launch count instead reads
/// `0` for a first Dispatch even though the cap is exactly what held it, and
/// disagrees with a later Dispatch that reports the same state.
pub fn cap_hold_detail(cap: ConcurrencyCap, in_flight: usize, fired: usize) -> String {
    format!(
        "held back by the concurrency cap (dispatch.max_concurrent = {}): {} Run(s) in flight, \
         so a Run must finish before another may start",
        cap.limit(),
        in_flight.saturating_add(fired)
    )
}

/// The reason a Ticket held back for want of a free Worker carries.
///
/// The Worker Pool is the other dial that bounds Dispatch, and a Pool with no
/// idle Worker is a capacity gap, not a failure: the Ticket is *waiting* for a
/// Run to finish, exactly as one the cap holds back is. The same detail is used
/// wherever a Ticket cannot get a Worker, so `dispatch --all` and a named
/// `dispatch` report the gap the same way.
pub const POOL_CAPACITY_GAP_DETAIL: &str = "held back by the Worker Pool: no idle Worker is available, so a Run must finish before another may start";

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(limit: usize) -> ConcurrencyCap {
        ConcurrencyCap::new(limit).expect("a usable cap")
    }

    #[test]
    fn a_cap_of_zero_is_not_a_cap() {
        assert!(ConcurrencyCap::new(0).is_none());
        assert_eq!(cap(4).limit(), 4);
    }

    #[test]
    fn a_batch_under_the_cap_fires_whole() {
        let batch = ["A", "B"];
        assert_eq!(pace(cap(4), 0, &batch), (&batch[..], &[][..]));
    }

    #[test]
    fn a_batch_over_the_cap_is_split_in_input_order() {
        let batch = ["A", "B", "C", "D"];
        let (fire, held) = pace(cap(2), 0, &batch);
        assert_eq!(fire, ["A", "B"]);
        assert_eq!(held, ["C", "D"]);
    }

    #[test]
    fn runs_already_in_flight_count_against_the_cap() {
        let batch = ["A", "B", "C", "D"];
        let (fire, held) = pace(cap(3), 2, &batch);
        assert_eq!(fire, ["A"]);
        assert_eq!(held, ["B", "C", "D"]);
    }

    #[test]
    fn a_pool_at_or_over_the_cap_holds_the_whole_batch() {
        let batch = ["A", "B"];
        for in_flight in [4, 9] {
            let (fire, held) = pace(cap(4), in_flight, &batch);
            assert!(fire.is_empty(), "{in_flight} in flight");
            assert_eq!(held, ["A", "B"]);
            assert_eq!(room(cap(4), in_flight), 0);
        }
    }

    #[test]
    fn a_first_dispatch_names_the_count_its_own_launches_reach() {
        // Nothing is in flight yet, so the pre-launch count is 0; the batch
        // fires its two and the cap is what holds the rest, so the count that
        // actually bound it is 2.
        let detail = cap_hold_detail(cap(2), 0, 2);
        assert!(detail.contains("2 Run(s) in flight"), "{detail}");
        assert!(!detail.contains("0 Run(s) in flight"), "{detail}");
        assert!(detail.contains("max_concurrent = 2"), "{detail}");
    }

    #[test]
    fn the_same_state_reached_twice_names_the_same_count() {
        // The first Dispatch fired two Runs; a second Dispatch in the same
        // window finds two already in flight and fires none. Both name the
        // count the governor reports, so the two messages agree about one state.
        let first = cap_hold_detail(cap(2), 0, 2);
        let second = cap_hold_detail(cap(2), 2, 0);
        assert_eq!(first, second);
    }

    #[test]
    fn a_pool_over_a_lowered_cap_names_its_real_in_flight_count() {
        // A cap lowered under Runs already in flight fires nothing, so the
        // count that bound the batch is what is really running, not the cap.
        let detail = cap_hold_detail(cap(2), 5, 0);
        assert!(detail.contains("5 Run(s) in flight"), "{detail}");
    }
}
