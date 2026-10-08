//! Rendering modules split out of the `app` god module. Each module here draws
//! from a read-only view model rather than reaching into `App`.

pub(crate) mod detail;
pub(crate) mod graph;
pub(crate) mod help;
pub(crate) mod home;
pub(crate) mod pool;
pub(crate) mod style;
pub(crate) mod view;
