//! The attention-grouped, idle-collapsible, name-tracked workspace list
//! (ADR 0008). Owns the list's *mechanics*: grouping the classified workspaces
//! into display rows, folding the idle group away, and tracking the selection by
//! workspace **name** so it follows a workspace as live state re-sorts it between
//! Attention groups.
//!
//! `App` derives each workspace's [`Attention`] (that needs `agents`/`work`, so
//! it stays there) and hands the already-sorted `(Attention, &Workspace)` pairs
//! in; this module hands back the display rows and the selected name.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{List, ListItem, ListState};

use crate::attention::Attention;
use crate::store::Workspace;

/// One rendered list line: a group header (non-selectable) or a workspace row.
#[derive(Debug, PartialEq)]
pub enum Row<'a> {
    Header(Attention, usize),
    Ws(&'a Workspace, Attention),
}

/// The stateful workspace list: which workspace is selected (by name), whether
/// the idle group is folded, and the render-only highlight cursor.
#[derive(Default)]
pub struct WorkspaceList {
    /// Selection tracked by workspace name, not row index, so it follows a
    /// workspace as live state re-sorts it between Attention groups.
    selected: Option<String>,
    /// Whether the idle group is folded away.
    idle_collapsed: bool,
    /// Render-only: the highlighted row index, recomputed from `selected` each
    /// draw (the list interleaves non-selectable group headers).
    list_state: ListState,
}

impl WorkspaceList {
    /// The currently-selected workspace name, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Whether the idle group is currently folded.
    pub fn idle_collapsed(&self) -> bool {
        self.idle_collapsed
    }

    /// Fold or unfold the idle group.
    pub fn toggle_idle(&mut self) {
        self.idle_collapsed = !self.idle_collapsed;
    }

    /// The display rows for the classified workspaces: a header per non-empty
    /// group, then its workspace rows (unless the idle group is collapsed).
    ///
    /// `classified` must be grouped by Attention (contiguous runs), which is how
    /// `App::classified` sorts it.
    pub fn rows<'a>(&self, classified: &[(Attention, &'a Workspace)]) -> Vec<Row<'a>> {
        let mut rows = Vec::new();
        let mut idx = 0;
        while idx < classified.len() {
            let att = classified[idx].0;
            let end = idx
                + classified[idx..]
                    .iter()
                    .take_while(|(a, _)| *a == att)
                    .count();
            rows.push(Row::Header(att, end - idx));
            if !(att == Attention::Idle && self.idle_collapsed) {
                for (a, w) in &classified[idx..end] {
                    rows.push(Row::Ws(w, *a));
                }
            }
            idx = end;
        }
        rows
    }

    /// The selectable workspace names in display order (excludes headers and any
    /// workspace hidden in a collapsed idle group). Owned, so callers can mutate
    /// the list afterwards without holding a borrow of the store.
    pub fn selectable(&self, classified: &[(Attention, &Workspace)]) -> Vec<String> {
        self.rows(classified)
            .into_iter()
            .filter_map(|r| match r {
                Row::Ws(w, _) => Some(w.name.clone()),
                Row::Header(..) => None,
            })
            .collect()
    }

    /// Move the selection by `delta` among the ordered selectable names,
    /// clamping at both ends (and clearing when nothing is selectable).
    pub fn move_selection(&mut self, selectable: &[String], delta: isize) {
        if selectable.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| selectable.iter().position(|n| n == s))
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, selectable.len() as isize - 1);
        self.selected = Some(selectable[next as usize].clone());
    }

    /// Point the selection at a real, currently-selectable workspace, falling
    /// back to the first one when the current target is gone or hidden (and to
    /// `None` when nothing is selectable). `selectable` is the ordered name list
    /// from [`WorkspaceList::selectable`].
    pub fn ensure_selection(&mut self, selectable: &[String]) {
        let valid = self
            .selected
            .as_ref()
            .is_some_and(|s| selectable.contains(s));
        if !valid {
            self.selected = selectable.first().cloned();
        }
    }

    /// Test-only: force the selection to a specific workspace name, standing in
    /// for the navigation a real session would perform to land on it.
    #[cfg(test)]
    pub fn select(&mut self, name: &str) {
        self.selected = Some(name.to_string());
    }

    /// Render the list into `area`, highlighting row `cursor` (owns `list_state`).
    pub fn render_body(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        items: Vec<ListItem<'static>>,
        cursor: Option<usize>,
    ) {
        self.list_state.select(cursor);
        frame.render_stateful_widget(List::new(items), area, &mut self.list_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.to_string(),
            path: Some(PathBuf::from(format!("/wt/{name}"))),
        }
    }

    #[test]
    fn rows_emit_a_header_per_group_then_its_workspaces() {
        let (a, b, c) = (ws("a"), ws("b"), ws("c"));
        let classified = [
            (Attention::NeedsYou, &a),
            (Attention::Idle, &b),
            (Attention::Idle, &c),
        ];
        let list = WorkspaceList::default();
        assert_eq!(
            list.rows(&classified),
            vec![
                Row::Header(Attention::NeedsYou, 1),
                Row::Ws(&a, Attention::NeedsYou),
                Row::Header(Attention::Idle, 2),
                Row::Ws(&b, Attention::Idle),
                Row::Ws(&c, Attention::Idle),
            ]
        );
    }

    #[test]
    fn selectable_lists_workspace_names_in_display_order_skipping_collapsed_idle() {
        let (a, b, c) = (ws("a"), ws("b"), ws("c"));
        let classified = [
            (Attention::NeedsYou, &a),
            (Attention::Idle, &b),
            (Attention::Idle, &c),
        ];
        let mut list = WorkspaceList::default();
        assert_eq!(list.selectable(&classified), vec!["a", "b", "c"]);
        list.toggle_idle();
        assert_eq!(list.selectable(&classified), vec!["a"]);
    }

    #[test]
    fn move_selection_steps_and_clamps_at_both_ends() {
        let names = ["a", "b", "c"].map(String::from).to_vec();
        let mut list = WorkspaceList::default();
        list.ensure_selection(&names);
        assert_eq!(list.selected(), Some("a"));

        list.move_selection(&names, -1); // clamp at top
        assert_eq!(list.selected(), Some("a"));
        list.move_selection(&names, 1);
        assert_eq!(list.selected(), Some("b"));
        list.move_selection(&names, 1);
        list.move_selection(&names, 1); // clamp at bottom
        assert_eq!(list.selected(), Some("c"));
    }

    #[test]
    fn move_selection_clears_when_nothing_is_selectable() {
        let mut list = WorkspaceList::default();
        list.ensure_selection(&["a".to_string()]);
        list.move_selection(&[], 1);
        assert_eq!(list.selected(), None);
    }

    #[test]
    fn ensure_selection_keeps_a_valid_target_and_otherwise_falls_back() {
        let names = |ns: &[&str]| ns.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut list = WorkspaceList::default();

        // No selection yet -> first selectable.
        list.ensure_selection(&names(&["a", "b"]));
        assert_eq!(list.selected(), Some("a"));

        // A still-valid selection is left untouched.
        list.ensure_selection(&names(&["a", "b"]));
        assert_eq!(list.selected(), Some("a"));

        // The target vanished (folded/deleted) -> first of what remains.
        list.ensure_selection(&names(&["b"]));
        assert_eq!(list.selected(), Some("b"));

        // Nothing selectable -> selection clears.
        list.ensure_selection(&[]);
        assert_eq!(list.selected(), None);
    }

    #[test]
    fn collapsing_idle_hides_its_workspace_rows_but_keeps_the_header() {
        let (a, b) = (ws("a"), ws("b"));
        let classified = [(Attention::NeedsYou, &a), (Attention::Idle, &b)];
        let mut list = WorkspaceList::default();
        list.toggle_idle();
        assert_eq!(
            list.rows(&classified),
            vec![
                Row::Header(Attention::NeedsYou, 1),
                Row::Ws(&a, Attention::NeedsYou),
                Row::Header(Attention::Idle, 1),
            ]
        );
    }
}
