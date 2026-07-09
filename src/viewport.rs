//! A vertical scroll viewport: the offset of the top visible line plus the
//! geometry needed to clamp it. Both the diff pane and the world graph scroll
//! through one of these, so the paging and clamp math lives in exactly one
//! place instead of being copy-pasted per view.

/// One scrollable viewport. `height` (the inner rows on screen) and `total`
/// (the content's line count) are refreshed each render; `scroll` is the
/// persisted top line. Every movement clamps against [`Viewport::max_scroll`],
/// so the last content line can always be brought just into view and no
/// further.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Top visible line.
    scroll: u16,
    /// Inner height at the last render.
    height: u16,
    /// Total content lines at the last render.
    total: u16,
}

impl Viewport {
    /// The current top line, for feeding the render offset.
    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    /// Record the geometry seen at render time and pull `scroll` back into
    /// range, so a shrunk viewport or a shorter document can never leave the
    /// offset stranded past the end.
    pub fn resize(&mut self, height: u16, total: u16) {
        self.height = height;
        self.total = total;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Update just the content length - the viewport height is unchanged - and
    /// re-clamp. Used when the content behind the viewport swaps for something of
    /// a different length between renders (the diff pane changing files).
    pub fn set_total(&mut self, total: u16) {
        self.total = total;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The furthest `scroll` may travel so the last line still shows.
    fn max_scroll(&self) -> u16 {
        self.total.saturating_sub(self.height)
    }

    /// Scroll one line toward the end.
    pub fn line_down(&mut self) {
        self.scroll = (self.scroll + 1).min(self.max_scroll());
    }

    /// Scroll one line toward the start.
    pub fn line_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    /// Scroll a full viewport toward the end.
    pub fn page_down(&mut self) {
        self.scroll = (self.scroll + self.height).min(self.max_scroll());
    }

    /// Scroll a full viewport toward the start.
    pub fn page_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(self.height);
    }

    /// Scroll half a viewport toward the end (a `Ctrl-d`-style nudge).
    pub fn half_page_down(&mut self) {
        self.scroll = (self.scroll + self.height / 2).min(self.max_scroll());
    }

    /// Scroll half a viewport toward the start (a `Ctrl-u`-style nudge).
    pub fn half_page_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(self.height / 2);
    }

    /// Jump to the first line.
    pub fn jump_top(&mut self) {
        self.scroll = 0;
    }

    /// Jump so the last line sits at the bottom of the viewport.
    pub fn jump_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10-line document in a 4-row viewport: the last line shows when the top
    /// line is 6 (`10 - 4`), so that is the furthest a line-by-line walk climbs.
    #[test]
    fn line_stepping_clamps_at_the_last_full_screen() {
        let mut v = Viewport::default();
        v.resize(4, 10);
        for _ in 0..100 {
            v.line_down();
        }
        assert_eq!(v.scroll(), 6, "cannot scroll past the last full screen");
        for _ in 0..100 {
            v.line_up();
        }
        assert_eq!(v.scroll(), 0, "line_up saturates at the top");
    }

    #[test]
    fn paging_moves_by_a_viewport_height_and_clamps() {
        let mut v = Viewport::default();
        v.resize(4, 10);
        v.page_down();
        assert_eq!(v.scroll(), 4, "one page is one viewport height");
        v.page_down();
        assert_eq!(v.scroll(), 6, "the second page clamps to the last screen");
        v.page_up();
        assert_eq!(v.scroll(), 2, "page_up steps back a whole height");
        v.page_up();
        assert_eq!(v.scroll(), 0, "page_up saturates at the top");
    }

    #[test]
    fn half_paging_moves_by_half_a_viewport_height() {
        let mut v = Viewport::default();
        v.resize(4, 10);
        v.half_page_down();
        assert_eq!(v.scroll(), 2, "half a 4-row viewport is 2 lines");
        v.half_page_down();
        assert_eq!(v.scroll(), 4);
        v.half_page_down();
        assert_eq!(v.scroll(), 6, "clamps to the last screen");
        v.half_page_up();
        assert_eq!(v.scroll(), 4);
        v.half_page_up();
        v.half_page_up();
        assert_eq!(v.scroll(), 0, "half_page_up saturates at the top");
    }

    #[test]
    fn set_total_reclamps_when_the_content_shrinks() {
        let mut v = Viewport::default();
        v.resize(4, 100);
        v.jump_bottom();
        assert_eq!(v.scroll(), 96);
        // The viewport now shows a shorter document without a resize; the offset
        // must be reeled back in against the new length.
        v.set_total(10);
        assert_eq!(v.scroll(), 6);
    }

    #[test]
    fn top_and_bottom_jump_to_the_extremes() {
        let mut v = Viewport::default();
        v.resize(4, 10);
        v.jump_bottom();
        assert_eq!(v.scroll(), 6);
        v.jump_top();
        assert_eq!(v.scroll(), 0);
    }

    #[test]
    fn a_document_shorter_than_the_viewport_never_scrolls() {
        let mut v = Viewport::default();
        v.resize(20, 3);
        v.line_down();
        v.page_down();
        v.jump_bottom();
        assert_eq!(v.scroll(), 0, "nothing to scroll when it all fits");
    }

    #[test]
    fn resize_pulls_a_stranded_scroll_back_into_range() {
        let mut v = Viewport::default();
        v.resize(4, 100);
        v.jump_bottom();
        assert_eq!(v.scroll(), 96);
        // The document shrinks under the offset; resize must reel it back in.
        v.resize(4, 10);
        assert_eq!(v.scroll(), 6, "resize clamps a now-out-of-range offset");
    }
}
