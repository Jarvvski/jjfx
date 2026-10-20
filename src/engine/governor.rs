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
}
