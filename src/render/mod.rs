//! Rendering modules split out of the `app` god module. Each module here draws
//! from a read-only view model rather than reaching into `App`.

pub(crate) mod graph;
pub(crate) mod style;
