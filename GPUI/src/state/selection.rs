//! Several conversations picked at once in the list on screen (MS-305), as Mail and Gmail pick
//! them: ⌘-click (Ctrl elsewhere) adds or drops one, ⇧-click and ⇧↑/↓ select the run from the
//! anchor, ⌘A all the list's conversations, Esc none. A plain click or arrow goes back to one
//! conversation. Actions then act on all of them at once (`actions.rs`), the engine asking the
//! provider once and taking them back in one undo step.
//!
//! The selection belongs to the list it was made in (the mailbox or search, and the open bundle,
//! if any): elsewhere it is empty, and it is forgotten when another list is picked in.

use super::*;

/// The picked conversations, by id, in the list `scope` names.
#[derive(Default)]
pub struct Selection {
    /// The list and the open bundle (none: the list itself) it was made in.
    scope: Option<(ListKey, Option<BundleKind>)>,
    ids: HashSet<String>,
    /// Where a run (⇧-click, ⇧↑/↓) starts: the row last clicked or ⌘-clicked.
    anchor: Option<String>,
    /// What was picked before the run from the anchor, which a new run from it replaces.
    base: HashSet<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// ⌘-click on `id`: adds it or drops it. The first ⌘-click keeps `current` (the conversation
    /// open or highlighted) as well, as Mail does.
    pub fn toggle(&mut self, id: &str, current: Option<&str>) {
        if self.ids.is_empty()
            && let Some(current) = current.filter(|current| *current != id)
        {
            self.ids.insert(current.to_owned());
        }
        if !self.ids.remove(id) {
            self.ids.insert(id.to_owned());
        }
        self.anchor = Some(id.to_owned());
        self.base = self.ids.clone();
    }

    /// ⇧-click on `to`, or ⇧↑/↓ arriving there: the run of `lines` (the list's conversations, in
    /// order) from the anchor (else `current`, else `to`) to `to`, with what was picked before it.
    pub fn extend(&mut self, to: &str, current: Option<&str>, lines: &[String]) {
        if self.anchor.as_ref().is_none_or(|anchor| !lines.contains(anchor)) {
            self.anchor = Some(current.filter(|current| lines.iter().any(|line| line == current)).unwrap_or(to).to_owned());
            self.base = HashSet::new();
        }
        let anchor = self.anchor.as_deref().unwrap_or(to);
        let (Some(from), Some(until)) = (lines.iter().position(|line| line == anchor), lines.iter().position(|line| line == to))
        else {
            return;
        };
        let run = &lines[from.min(until)..=from.max(until)];
        self.ids = self.base.iter().cloned().chain(run.iter().cloned()).collect();
    }

    /// ⌘A: every one of `lines`.
    pub fn select_all(&mut self, lines: &[String]) {
        self.ids = lines.iter().cloned().collect();
        self.base = self.ids.clone();
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.base.clear();
        self.anchor = None;
    }

    /// Keeps only what `lines` still lists (others were archived, trashed or moved away).
    pub fn retain(&mut self, lines: &[String]) {
        let listed: HashSet<&str> = lines.iter().map(String::as_str).collect();
        self.ids.retain(|id| listed.contains(id.as_str()));
        self.base.retain(|id| listed.contains(id.as_str()));
        if self.anchor.as_deref().is_some_and(|anchor| !listed.contains(anchor)) {
            self.anchor = None;
        }
    }
}

impl MailApp {
    /// The list and open bundle a selection made now belongs to.
    fn selection_scope(&self) -> (ListKey, Option<BundleKind>) {
        (self.key(), self.open_bundle())
    }

    /// The selection in the list on screen, if it holds anything; none while Chat or the
    /// calendar is shown.
    pub fn selection(&self) -> Option<&Selection> {
        let selection = &self.selection;
        (self.mail_in_view() && !selection.is_empty() && selection.scope.as_ref() == Some(&self.selection_scope()))
            .then_some(selection)
    }

    /// The selection, made the list on screen's: one made in another list is forgotten.
    fn selection_here(&mut self) -> &mut Selection {
        let scope = self.selection_scope();
        if self.selection.scope.as_ref() != Some(&scope) {
            self.selection = Selection { scope: Some(scope), ..Selection::default() };
        }
        &mut self.selection
    }

    /// Whether row `id` of the list on screen shows as selected: picked, else the highlight.
    pub fn row_selected(&self, id: &str, in_bundle: bool) -> bool {
        match self.selection() {
            Some(selection) => selection.contains(id),
            None => (if in_bundle { &self.bundle_highlight } else { &self.highlight }).as_deref() == Some(id),
        }
    }

    /// The conversations of the list on screen that can be picked, in order: its rows, not the
    /// bundles folded among them or Chat's spaces.
    fn selectable_lines(&self) -> Vec<String> {
        self.active_lines().0.into_iter().map(|(id, _)| id).filter(|id| !opens_only(id)).collect()
    }

    /// The highlight in the list on screen.
    fn current(&self) -> Option<String> {
        if self.open_bundle().is_some() { self.bundle_highlight.clone() } else { self.highlight.clone() }
    }

    fn set_current(&mut self, id: String) {
        if self.open_bundle().is_some() {
            self.bundle_highlight = Some(id);
        } else {
            self.highlight = Some(id);
        }
    }

    /// A click on row `id`: ⌘ (Ctrl elsewhere) adds or drops it, ⇧ selects the run to it, a
    /// plain click opens it alone, as before. A bundle's row and a Chat space always open.
    pub fn click_row(&mut self, id: &str, modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let picking = modifiers.secondary(cx) || modifiers.shift;
        if !picking || opens_only(id) {
            self.selection.clear();
            self.activate(id, window, cx);
            return;
        }
        window.focus(&self.focus, cx);
        let lines = self.selectable_lines();
        let current = self.current().filter(|current| lines.contains(current));
        let selection = self.selection_here();
        if modifiers.shift {
            selection.extend(id, current.as_deref(), &lines);
        } else {
            selection.toggle(id, current.as_deref());
        }
        self.set_current(id.to_owned());
        cx.notify();
    }

    /// ⇧↓ and ⇧↑: the highlight moves and the run from the anchor follows it.
    fn extend_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let (lines, in_bundle) = self.active_lines();
        let picked: Vec<String> = self.selectable_lines();
        let current = self.current();
        let position = current.as_ref().and_then(|id| picked.iter().position(|line| line == id));
        let target = match position {
            None if picked.is_empty() => return,
            None => if delta > 0 { 0 } else { picked.len() - 1 },
            Some(at) => (at as isize + delta).clamp(0, picked.len() as isize - 1) as usize,
        };
        let id = picked[target].clone();
        self.selection_here().extend(&id, current.as_deref(), &picked);
        if let Some((_, scroll_index)) = lines.iter().find(|(line, _)| *line == id) {
            if in_bundle {
                self.bundle_scroll.scroll_to_reveal_item(*scroll_index);
            } else {
                self.list_scroll.scroll_to_reveal_item(*scroll_index);
            }
        }
        self.set_current(id);
        cx.notify();
    }

    pub fn extend_next(&mut self, _: &ExtendNext, _: &mut Window, cx: &mut Context<Self>) {
        self.extend_step(1, cx);
    }

    pub fn extend_previous(&mut self, _: &ExtendPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.extend_step(-1, cx);
    }

    /// ⌘A: every conversation of the list on screen.
    pub fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if !self.mail_in_view() {
            return;
        }
        let lines = self.selectable_lines();
        self.selection_here().select_all(&lines);
        cx.notify();
    }

    /// Esc and the bar's ✕: nothing picked any more. True when something was.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.selection().is_none() {
            return false;
        }
        self.selection.clear();
        cx.notify();
        true
    }

    /// The picked conversations, in the list's order.
    pub fn selected_conversations(&self) -> Vec<Conversation> {
        let Some(selection) = self.selection() else { return Vec::new() };
        let Some(list) = self.list() else { return Vec::new() };
        list.conversations.iter().filter(|conversation| selection.contains(&conversation.id)).cloned().collect()
    }

    /// The list changed: what it no longer lists leaves the selection.
    pub(super) fn trim_selection(&mut self) {
        if self.selection().is_some() {
            let lines = self.selectable_lines();
            self.selection.retain(&lines);
        }
    }
}

/// A row that can't be picked, only opened: a bundle, or a space of the Chat bundle (MS-330).
fn opens_only(id: &str) -> bool {
    id.starts_with("bundle:") || id.starts_with("chat:")
}

#[cfg(test)]
mod tests {
    use super::Selection;

    fn lines() -> Vec<String> {
        ["a", "b", "c", "d", "e"].into_iter().map(String::from).collect()
    }

    fn picked(selection: &Selection) -> Vec<&str> {
        let mut ids: Vec<&str> = selection.ids.iter().map(String::as_str).collect();
        ids.sort();
        ids
    }

    #[test]
    fn a_secondary_click_adds_or_drops_one_keeping_the_open_conversation() {
        let mut selection = Selection::default();
        selection.toggle("c", Some("a"));
        assert_eq!(picked(&selection), ["a", "c"]);
        selection.toggle("d", Some("c"));
        assert_eq!(picked(&selection), ["a", "c", "d"]);
        selection.toggle("a", Some("d"));
        assert_eq!(picked(&selection), ["c", "d"]);
        // The first pick on the open conversation itself picks it alone.
        let mut alone = Selection::default();
        alone.toggle("b", Some("b"));
        assert_eq!(picked(&alone), ["b"]);
        alone.toggle("b", Some("b"));
        assert!(alone.is_empty());
    }

    #[test]
    fn a_shift_click_selects_the_run_from_the_anchor_replacing_the_last_run() {
        let lines = lines();
        let mut selection = Selection::default();
        // No anchor yet: the run starts at the highlight.
        selection.extend("d", Some("b"), &lines);
        assert_eq!(picked(&selection), ["b", "c", "d"]);
        // Another run from the same anchor replaces it, upwards too.
        selection.extend("a", Some("d"), &lines);
        assert_eq!(picked(&selection), ["a", "b"]);
        // A ⌘-click moves the anchor and keeps what was picked; the next run adds to it.
        selection.toggle("e", Some("a"));
        assert_eq!(picked(&selection), ["a", "b", "e"]);
        selection.extend("d", Some("e"), &lines);
        assert_eq!(picked(&selection), ["a", "b", "d", "e"]);
        selection.extend("c", Some("d"), &lines);
        assert_eq!(picked(&selection), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn shift_arrows_grow_and_shrink_the_run_from_where_they_started() {
        let lines = lines();
        let mut selection = Selection::default();
        // ⇧↓ twice from b, then ⇧↑ once: b and c.
        selection.extend("c", Some("b"), &lines);
        selection.extend("d", Some("c"), &lines);
        assert_eq!(picked(&selection), ["b", "c", "d"]);
        selection.extend("c", Some("d"), &lines);
        assert_eq!(picked(&selection), ["b", "c"]);
        // Past the anchor the run turns round.
        selection.extend("b", Some("c"), &lines);
        selection.extend("a", Some("b"), &lines);
        assert_eq!(picked(&selection), ["a", "b"]);
    }

    #[test]
    fn all_none_and_what_leaves_the_list() {
        let lines = lines();
        let mut selection = Selection::default();
        selection.select_all(&lines);
        assert_eq!(picked(&selection), ["a", "b", "c", "d", "e"]);
        // Archived: b and d leave the list and the selection.
        let left: Vec<String> = ["a", "c", "e"].into_iter().map(String::from).collect();
        selection.retain(&left);
        assert_eq!(picked(&selection), ["a", "c", "e"]);
        selection.clear();
        assert!(selection.is_empty());
        // A run whose anchor left the list starts afresh at the highlight.
        selection.toggle("b", None);
        selection.retain(&left);
        selection.extend("e", Some("c"), &left);
        assert_eq!(picked(&selection), ["c", "e"]);
    }
}
