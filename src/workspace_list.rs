//! The default-pinned, attention-grouped, idle-collapsible, name-tracked
//! workspace list (ADR 0008). Owns the list's *mechanics*: classifying each
//! workspace's [`Attention`], pinning `default`, grouping the rest into display
//! rows, folding the idle group away, and tracking the selection by workspace
//! **name** so it follows a workspace as live state re-sorts it between
//! Attention groups.
//!
//! `App` supplies each workspace's raw axes (agent + work + worker) via
//! [`RowInput`]; [`ClassifiedView::build`] does the classification, ordering,
//! grouping, and projection to rows in one place. Because the only constructor
//! groups internally, display order is a property of the type rather than a
//! contract the caller must satisfy.

use crate::agent::Agent;
use crate::attention::{self, Attention};
use crate::store::{DEFAULT_WORKSPACE, Workspace};
use crate::work::WorkState;
use wsg_core::WorkerSnapshot;

/// The raw per-workspace inputs the list classifies and renders from: the two
/// lifecycle axes (agent + work) plus the joined Worker snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowInput<'a> {
    pub workspace: &'a Workspace,
    pub agent: Agent,
    pub work: WorkState,
    pub worker: Option<&'a WorkerSnapshot>,
}

/// One selectable workspace row's presentation data. Carries everything
/// selection (`workspace`) and rendering (`attention`, `agent`, `work`,
/// `worker`) need.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceRow<'a> {
    pub workspace: &'a Workspace,
    pub attention: Attention,
    pub agent: Agent,
    pub work: WorkState,
    pub worker: Option<&'a WorkerSnapshot>,
}

/// One rendered list line: a group header (non-selectable) or a self-describing
/// workspace row. There is no second row enum to hand-map between.
#[derive(Debug, PartialEq)]
pub enum Row<'a> {
    Header(Attention, usize),
    Ws(WorkspaceRow<'a>),
}

/// The workspaces classified by [`Attention`] and laid out as display rows:
/// `default` pinned first, then a header per non-empty group and its workspace
/// rows in [`Attention::ALL`] order (the idle group folded when collapsed).
#[derive(Debug)]
pub struct ClassifiedView<'a> {
    rows: Vec<Row<'a>>,
}

impl<'a> ClassifiedView<'a> {
    /// Classify `inputs`, order and group them, and project to display rows.
    /// `default` is pinned first regardless of its Attention; the remaining
    /// workspaces are grouped needs-you -> working -> ready-to-forge -> idle,
    /// sorted by name within each group.
    pub fn build(inputs: &[RowInput<'a>], idle_collapsed: bool) -> Self {
        let mut rows = Vec::new();

        if let Some(input) = inputs
            .iter()
            .find(|input| input.workspace.name == DEFAULT_WORKSPACE)
        {
            rows.push(row(input));
        }

        for att in Attention::ALL {
            let mut members: Vec<&RowInput<'a>> = inputs
                .iter()
                .filter(|input| input.workspace.name != DEFAULT_WORKSPACE)
                .filter(|input| attention::derive(input.agent.state, input.work) == att)
                .collect();
            if members.is_empty() {
                continue;
            }
            members.sort_by(|a, b| a.workspace.name.cmp(&b.workspace.name));
            rows.push(Row::Header(att, members.len()));
            if att == Attention::Idle && idle_collapsed {
                continue;
            }
            rows.extend(members.into_iter().map(row));
        }

        Self { rows }
    }

    /// The display rows in render order.
    pub fn rows(&self) -> &[Row<'a>] {
        &self.rows
    }

    /// The selectable workspace names in display order (excludes headers and any
    /// workspace hidden in a collapsed idle group).
    pub fn selectable_names(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Ws(WorkspaceRow { workspace, .. }) => Some(workspace.name.clone()),
                Row::Header(..) => None,
            })
            .collect()
    }
}

/// Project one input to its workspace row.
fn row<'a>(input: &RowInput<'a>) -> Row<'a> {
    Row::Ws(WorkspaceRow {
        workspace: input.workspace,
        attention: attention::derive(input.agent.state, input.work),
        agent: input.agent,
        work: input.work,
        worker: input.worker,
    })
}

/// The stateful workspace list: which workspace is selected (by name) and
/// whether the idle group is folded. The render cursor lives with the renderer.
#[derive(Default)]
pub struct WorkspaceList {
    /// Selection tracked by workspace name, not row index, so it follows a
    /// workspace as live state re-sorts it between Attention groups.
    selected: Option<String>,
    /// Whether the idle group is folded away.
    idle_collapsed: bool,
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

    /// Restore the idle-group fold state from persisted UI preferences.
    pub fn set_idle_collapsed(&mut self, collapsed: bool) {
        self.idle_collapsed = collapsed;
    }

    /// Classify and lay out `inputs` into the current view, honoring the idle
    /// fold. The single interface both selection and rendering go through.
    pub fn view<'a>(&self, inputs: &[RowInput<'a>]) -> ClassifiedView<'a> {
        ClassifiedView::build(inputs, self.idle_collapsed)
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

    /// Select the first selectable name (clearing when nothing is selectable).
    pub fn select_first(&mut self, selectable: &[String]) {
        self.selected = selectable.first().cloned();
    }

    /// Select the last selectable name (clearing when nothing is selectable).
    pub fn select_last(&mut self, selectable: &[String]) {
        self.selected = selectable.last().cloned();
    }

    /// Point the selection at a real, currently-selectable workspace, falling
    /// back to the first one when the current target is gone or hidden (and to
    /// `None` when nothing is selectable). `selectable` is the ordered name list
    /// from [`ClassifiedView::selectable_names`].
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentState};
    use std::path::PathBuf;

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.to_string(),
            path: Some(PathBuf::from(format!("/wt/{name}"))),
        }
    }

    fn input<'a>(workspace: &'a Workspace, agent: AgentState, work: WorkState) -> RowInput<'a> {
        RowInput {
            workspace,
            agent: Agent {
                state: agent,
                ..Default::default()
            },
            work,
            worker: None,
        }
    }

    /// The structural shape of a view: header markers and workspace names in
    /// display order, ignoring the per-row presentation fields.
    fn shape(view: &ClassifiedView<'_>) -> Vec<String> {
        view.rows()
            .iter()
            .map(|row| match row {
                Row::Header(att, count) => format!("header:{att:?}({count})"),
                Row::Ws(WorkspaceRow { workspace, .. }) => format!("ws:{}", workspace.name),
            })
            .collect()
    }

    #[test]
    fn build_orders_unsorted_inputs_into_attention_groups() {
        let (a, b, c) = (ws("a"), ws("b"), ws("c"));
        // Deliberately unsorted: idle first, needs-you last.
        let inputs = [
            input(&b, AgentState::Absent, WorkState::Clean),
            input(&a, AgentState::NeedsAttention, WorkState::Clean),
            input(&c, AgentState::Absent, WorkState::Clean),
        ];
        let view = ClassifiedView::build(&inputs, false);
        assert_eq!(
            shape(&view),
            [
                "header:NeedsYou(1)",
                "ws:a",
                "header:Idle(2)",
                "ws:b",
                "ws:c",
            ]
        );
    }

    #[test]
    fn build_pins_default_first_regardless_of_attention() {
        let (d, a) = (ws(DEFAULT_WORKSPACE), ws("a"));
        let inputs = [
            input(&a, AgentState::NeedsAttention, WorkState::Clean),
            input(&d, AgentState::Absent, WorkState::Clean),
        ];
        let view = ClassifiedView::build(&inputs, false);
        assert_eq!(shape(&view), ["ws:default", "header:NeedsYou(1)", "ws:a"]);
    }

    #[test]
    fn selectable_names_list_display_order_skipping_collapsed_idle() {
        let (a, b, c) = (ws("a"), ws("b"), ws("c"));
        let inputs = [
            input(&a, AgentState::NeedsAttention, WorkState::Clean),
            input(&b, AgentState::Absent, WorkState::Clean),
            input(&c, AgentState::Absent, WorkState::Clean),
        ];
        assert_eq!(
            ClassifiedView::build(&inputs, false).selectable_names(),
            ["a", "b", "c"]
        );
        assert_eq!(
            ClassifiedView::build(&inputs, true).selectable_names(),
            ["a"]
        );
    }

    #[test]
    fn collapsing_idle_hides_its_workspace_rows_but_keeps_the_header() {
        let (a, b) = (ws("a"), ws("b"));
        let inputs = [
            input(&a, AgentState::NeedsAttention, WorkState::Clean),
            input(&b, AgentState::Absent, WorkState::Clean),
        ];
        assert_eq!(
            shape(&ClassifiedView::build(&inputs, true)),
            ["header:NeedsYou(1)", "ws:a", "header:Idle(1)"]
        );
    }

    #[test]
    fn view_honors_the_lists_idle_fold() {
        let (a, b) = (ws("a"), ws("b"));
        let inputs = [
            input(&a, AgentState::NeedsAttention, WorkState::Clean),
            input(&b, AgentState::Absent, WorkState::Clean),
        ];
        let mut list = WorkspaceList::default();
        assert_eq!(list.view(&inputs).selectable_names(), ["a", "b"]);
        list.toggle_idle();
        assert_eq!(list.view(&inputs).selectable_names(), ["a"]);
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
    fn select_first_and_last_jump_to_each_edge() {
        let names = ["a", "b", "c"].map(String::from).to_vec();
        let mut list = WorkspaceList::default();

        list.select_last(&names);
        assert_eq!(list.selected(), Some("c"));
        list.select_first(&names);
        assert_eq!(list.selected(), Some("a"));
    }

    #[test]
    fn select_first_and_last_clear_when_nothing_is_selectable() {
        let mut list = WorkspaceList::default();
        list.ensure_selection(&["a".to_string()]);

        list.select_first(&[]);
        assert_eq!(list.selected(), None);
        list.ensure_selection(&["a".to_string()]);
        list.select_last(&[]);
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
}
