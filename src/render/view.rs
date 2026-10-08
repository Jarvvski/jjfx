//! Read-only view models: the projections a renderer consumes. These carry
//! borrowed content only - no `App` handle, no mutable state - so rendering is a
//! pure function of the model and the layout.

use crate::agent::Agent;
use crate::attention::Attention;
use crate::forge;
use crate::store::Workspace;
use crate::work::WorkState;
use wsg_core::WorkerSnapshot;

/// What transient row-level state rides alongside the two lifecycle axes while a
/// row is drawn: a deletion tombstone, a background lift, or a live forge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowMarker<'a> {
    None,
    Deleting,
    Lifting,
    Queued,
    Forge(&'a forge::Progress),
}

/// A read-only projection of one workspace row for the home list: the classified
/// lifecycle axes plus the row-level facts rendering needs (`behind`, marker).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HomeRow<'a> {
    pub workspace: &'a Workspace,
    pub attention: Attention,
    pub agent: Agent,
    pub work: WorkState,
    pub worker: Option<&'a WorkerSnapshot>,
    pub behind: u32,
    pub marker: RowMarker<'a>,
}
