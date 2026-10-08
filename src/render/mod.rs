//! Rendering modules split out of the `app` god module. Each module here draws
//! from a read-only view model rather than reaching into `App`.

pub(crate) mod detail;
pub(crate) mod graph;
pub(crate) mod help;
pub(crate) mod home;
pub(crate) mod pool;
pub(crate) mod state;
pub(crate) mod style;
pub(crate) mod view;

#[cfg(test)]
mod tests {
    /// The render modules must draw from view models, never reach into `App`.
    #[test]
    fn render_modules_do_not_depend_on_app() {
        for (name, src) in [
            ("style", include_str!("style.rs")),
            ("view", include_str!("view.rs")),
            ("state", include_str!("state.rs")),
            ("graph", include_str!("graph.rs")),
            ("home", include_str!("home.rs")),
            ("pool", include_str!("pool.rs")),
            ("detail", include_str!("detail.rs")),
            ("help", include_str!("help.rs")),
        ] {
            assert!(
                !src.contains("crate::app"),
                "render::{name} must not depend on App"
            );
        }
    }
}
