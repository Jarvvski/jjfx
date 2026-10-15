//! The dispatch capacity governor: an optional ceiling on how many Runs may be
//! in flight at once.
//!
//! `pool N` sizes the Pool and `dispatch --all` fires the whole batch, so a
//! large Pool plus a large batch can trip a provider's rate limit in one go.
//! A cap paces a batch instead: the first `limit - in_flight` Tickets fire, and
//! the rest are held back in a waiting state the owner can see.
//!
//! The cap is a *ceiling on concurrent Runs*, not a batch size: it counts the
//! Runs already in flight, so repeated Dispatches stay under the same limit
//! rather than each firing a fresh batch of `limit`.

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
