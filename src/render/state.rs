//! Render-owned scroll state: the viewport offsets that survive between frames
//! but are not domain state. Keeping them here lets `App` hand rendering a
//! read-only view model while the geometry is refreshed outside draw.

use crate::viewport::Viewport;

/// The mutable render bookkeeping `App` owns: the inline world pane's scroll
/// (and whether it is enabled) and the full-screen graph's scroll.
#[derive(Default)]
pub(crate) struct RenderState {
    /// The inline world pane under the home list: `Some` while enabled.
    pub(crate) world: Option<Viewport>,
    /// Scroll of the full-screen world graph.
    pub(crate) graph_viewport: Viewport,
}
