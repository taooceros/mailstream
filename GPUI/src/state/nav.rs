//! Selection, the layers over the list, keyboard movement and search.

use super::*;
use crate::swipe::{Pointer, Step};
use gpui::{DismissEvent, ScrollEvent, TouchPhase};

impl MailApp {
    /// Mail is in view: neither Chat nor the calendar is.
    pub fn mail_in_view(&self) -> bool {
        self.chat_view.is_none() && !self.calendar_view
    }

    /// Chat or the calendar gives way to mail.
    fn leave_panes(&mut self, cx: &mut Context<Self>) {
        self.chat_view = None;
        if std::mem::take(&mut self.calendar_view) {
            self.calendar.update(cx, |calendar, cx| calendar.hide(cx));
        }
    }

    pub fn select(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.source == source && self.query.is_none() && self.mail_in_view();
        self.leave_panes(cx);
        self.context_menu = None;
        self.tag_editor = None;
        self.source = source;
        self.query = None;
        self.search.update(cx, |search, cx| search.clean(window, cx));
        self.layers.clear();
        if !same {
            self.highlight = None;
            self.list_scroll = list_state(0);
        }
        self.rebuild_items();
        self.load_list(self.key(), false, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Shows Google Chat: every account's spaces (None) or one account's.
    pub fn select_chat(&mut self, account: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_panes(cx);
        self.chat_view = Some(account.clone());
        self.context_menu = None;
        self.tag_editor = None;
        self.hovered_row = None;
        self.chat.update(cx, |chat, cx| chat.show(account, window, cx));
        cx.notify();
    }

    /// Shows the calendar.
    pub fn select_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_view = None;
        self.calendar_view = true;
        self.context_menu = None;
        self.tag_editor = None;
        self.hovered_row = None;
        self.calendar.update(cx, |calendar, cx| calendar.show(window, cx));
        cx.notify();
    }

    pub(super) fn on_search_event(
        &mut self,
        search: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { secondary: false, .. } => {
                let text = search.read(cx).value().trim().to_owned();
                if text.is_empty() {
                    self.end_search(cx);
                } else {
                    self.start_search(text.into(), cx);
                }
                window.focus(&self.focus, cx);
            }
            InputEvent::Change => {
                if search.read(cx).value().is_empty() && self.query.is_some() {
                    self.end_search(cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    /// Mail searches `query` on a layer over the list.
    fn start_search(&mut self, query: SharedString, cx: &mut Context<Self>) {
        self.leave_panes(cx);
        self.query = Some(query);
        self.layers.clear();
        self.push_layer(Layer::Search, true);
        self.highlight = None;
        self.list_scroll = list_state(0);
        self.rebuild_items();
        self.load_list(self.key(), false, cx);
    }

    /// The mail exchanged with `address`, from Chat's direct message with them (MS-331): mail
    /// searches every account for it, the address in the search field, as if typed there; Esc
    /// ends it as any search.
    pub fn search_mail_with(&mut self, address: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.hovered_row = None;
        if let Source::Mailbox { .. } = self.source {
            self.source = Source::Unified(MailboxRole::Inbox);
        }
        self.search.update(cx, |search, cx| search.set_value(address.clone(), window, cx));
        self.start_search(address, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Esc in the search field clears it, ends the search and gives the keys back to the list.
    pub fn cancel_search(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.clean(window, cx));
        self.end_search(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn end_search(&mut self, cx: &mut Context<Self>) {
        if self.query.take().is_some() {
            self.layers.retain(|layer| !matches!(layer, Layer::Search));
            self.highlight = None;
            self.list_scroll = list_state(0);
            self.rebuild_items();
            self.load_list(self.key(), false, cx);
        }
    }

    pub fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.search.focus_handle(cx);
        window.focus(&focus, cx);
        self.search.update(cx, |search, cx| search.select_all(window, cx));
    }

    pub fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        // The engine syncs every account; its change events reload the lists as mail comes in.
        self.background(cx, engine().refresh(), |this, result, cx| {
            if let Err(error) = result {
                this.show_toast(format!("Couldn't refresh: {error}"), cx);
            }
        });
        self.load_list(self.key(), true, cx);
    }

    /// The list the keyboard moves in: the open bundle's, else the main one. Each line is an id
    /// and the row's index in its list view.
    pub(super) fn active_lines(&self) -> (Vec<(String, usize)>, bool) {
        match self.open_bundle() {
            Some(BundleKind::Mail(category)) => {
                let (Some(bundle), Some(list)) = (self.bundle_members(category), self.list()) else {
                    return (Vec::new(), true);
                };
                let lines = bundle
                    .members
                    .iter()
                    .enumerate()
                    .map(|(row, &ix)| (list.conversations[ix].id.clone(), row))
                    .collect();
                return (lines, true);
            }
            Some(BundleKind::Chat) => {
                let Some(bundle) = self.chat_bundle() else { return (Vec::new(), true) };
                return (bundle.spaces.iter().enumerate().map(|(row, space)| (space.row_id(), row)).collect(), true);
            }
            None => {}
        }
        let lines = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(position, item)| match item {
                Item::Section(_) | Item::Waiting(_) => None,
                _ => Some((self.item_id(item), position)),
            })
            .collect();
        (lines, false)
    }

    pub fn item_id(&self, item: &Item) -> String {
        match item {
            Item::Section(title) => format!("section:{title}"),
            Item::Waiting(_) => "section:waiting".into(),
            Item::Row(index) => self.list().and_then(|l| l.conversations.get(*index)).map(|c| c.id.clone()).unwrap_or_default(),
            Item::Bundle(bundle) => BundleKind::Mail(bundle.category).id(),
            Item::Chat(_) => BundleKind::Chat.id(),
        }
    }

    pub fn open_bundle(&self) -> Option<BundleKind> {
        self.live_layers().rev().find_map(|l| match l {
            Layer::Bundle(kind) => Some(*kind),
            Layer::Reader(_) | Layer::Search => None,
        })
    }

    /// The mail bundle lying over the list.
    pub fn open_bundle_members(&self) -> Option<Rc<Bundle>> {
        match self.open_bundle()? {
            BundleKind::Mail(category) => self.bundle_members(category),
            BundleKind::Chat => None,
        }
    }

    /// `category`'s bundle in the list, also while its page slides off (MS-289).
    pub fn bundle_members(&self, category: Category) -> Option<Rc<Bundle>> {
        self.items.iter().find_map(|item| match item {
            Item::Bundle(bundle) if bundle.category == category => Some(bundle.clone()),
            _ => None,
        })
    }

    /// The Chat bundle in the list, also while its page slides off (MS-330).
    pub fn chat_bundle(&self) -> Option<Rc<ChatBundle>> {
        self.items.iter().find_map(|item| match item {
            Item::Chat(bundle) => Some(bundle.clone()),
            _ => None,
        })
    }

    /// The layers that stay: all but one sliding off after a back, bottom to top.
    pub fn live_layers(&self) -> impl DoubleEndedIterator<Item = &Layer> {
        let closing = self.closing().then(|| self.slide_layer.clone()).flatten();
        self.layers.iter().filter(move |layer| closing.as_ref() != Some(&layer.id()))
    }

    /// Where the layer `slide` belongs to lies in `layers`.
    pub fn sliding(&self) -> Option<usize> {
        let id = self.slide_layer.as_ref()?;
        self.layers.iter().position(|layer| &layer.id() == id)
    }

    /// A layer is sliding off and goes when it arrives.
    pub fn closing(&self) -> bool {
        matches!(self.slide, Slide::Settle { back: true, .. }) && self.sliding().is_some()
    }

    /// The conversation beside the list is the layer sliding off.
    pub fn closing_reader(&self) -> bool {
        self.closing() && matches!(self.sliding().map(|index| &self.layers[index]), Some(Layer::Reader(_)))
    }

    pub fn reader(&self) -> Option<&Conversation> {
        match self.live_layers().next_back() {
            Some(Layer::Reader(conversation)) => Some(conversation),
            _ => None,
        }
    }

    /// Lays a layer over the others; `animate` slides it in from the right. A bundle goes under a
    /// conversation open beside the list, which stays open over it (MS-307).
    fn push_layer(&mut self, layer: Layer, animate: bool) {
        self.finish_closing();
        let at = match layer {
            Layer::Bundle(_) | Layer::Search => {
                self.layers.iter().position(|layer| matches!(layer, Layer::Reader(_))).unwrap_or(self.layers.len())
            }
            Layer::Reader(_) => self.layers.len(),
        };
        self.slide_layer = Some(layer.id());
        self.slide = if animate { Slide::opening() } else { Slide::Rest };
        self.layers.insert(at, layer);
    }

    /// A layer still sliding off goes now, so what follows acts on what stays.
    fn finish_closing(&mut self) {
        if self.closing() {
            if let Some(index) = self.sliding() {
                self.layers.remove(index);
            }
            self.slide = Slide::Rest;
            self.slide_layer = None;
        }
    }

    /// How far layer `index` travels between in and out, as last laid out: beside the list, the
    /// conversation the room it takes and a bundle under it the list's island; otherwise the list
    /// island's width.
    pub fn travel(&self, index: usize) -> f32 {
        match (self.beside, &self.layers[index]) {
            (Some(beside), Layer::Reader(_)) => beside.travel,
            (Some(beside), Layer::Bundle(_) | Layer::Search)
                if self.layers[index..].iter().any(|l| matches!(l, Layer::Reader(_))) =>
            {
                beside.list
            }
            _ => self.content_width,
        }
    }

    /// How far right of its place the sliding layer sits now, without advancing anything.
    fn current_offset(&self) -> f32 {
        self.sliding().map_or(0., |index| self.slide.offset(self.travel(index)))
    }

    /// Which layer slides, and how far right of its place it sits, called while rendering with
    /// the layout's widths (`content_width`, `beside`): advances the slide, asks for the next
    /// frame while it moves, and drops a layer that slid off.
    pub fn slide_offset(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<(usize, f32)> {
        if self.layers.is_empty() {
            self.slide = Slide::Rest;
            self.slide_layer = None;
            return None;
        }
        let index = match self.sliding() {
            Some(index) => index,
            None => {
                self.slide_layer = self.layers.last().map(Layer::id);
                self.slide = Slide::Rest;
                self.layers.len() - 1
            }
        };
        let travel = self.travel(index);
        match self.slide.advance(travel, window, cx) {
            Some(offset) => Some((index, offset)),
            None => {
                let removed = self.layers.remove(index);
                if matches!(removed, Layer::Search) {
                    self.search.update(cx, |search, cx| search.clean(window, cx));
                    self.end_search(cx);
                }
                self.slide_layer = self.layers.last().map(Layer::id);
                self.layers.last().map(|_| (self.layers.len() - 1, 0.))
            }
        }
    }

    /// A scroll event in the content area (`at`, from its top left), in the capture phase: true
    /// when the back swipe took it. A swipe closes the layer under the fingers (MS-299).
    pub fn feed_swipe(&mut self, event: &ScrollEvent, at: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if event.touch_phase == TouchPhase::Started {
            self.gesture_layer = self.layer_under(f32::from(at.x));
        }
        let can_start = self.gesture_layer.is_some() && self.context_menu.is_none();
        let step = self.swipe.feed(event, can_start);
        self.follow(step, cx)
    }

    /// The layer a gesture at `x` (from the content area's left) closes (MS-299).
    fn layer_under(&self, x: f32) -> Option<SharedString> {
        let live: Vec<&Layer> = self.live_layers().collect();
        let readers: Vec<bool> = live.iter().map(|layer| matches!(layer, Layer::Reader(_))).collect();
        gesture_target(&readers, self.beside.map(|beside| beside.edge), x).map(|index| live[index].id())
    }

    /// A single left press on layer `layer` at `at`: a sideways drag from there closes it, the
    /// layer under the pointer, whichever lies on top (MS-266, MS-299).
    pub fn press_layer(&mut self, layer: &SharedString, at: Point<Pixels>) {
        let live = self.live_layers().any(|live| &live.id() == layer);
        self.gesture_layer = live.then(|| layer.clone());
        self.drag.press(at, live && self.context_menu.is_none());
    }

    /// The mouse after a press on a layer, in the capture phase: true when the drag took it.
    pub fn feed_drag(&mut self, pointer: Pointer, cx: &mut Context<Self>) -> bool {
        let step = self.drag.feed(pointer);
        self.follow(step, cx)
    }

    /// The gesture's layer follows a swipe's or a drag's step: true when the gesture took the event.
    fn follow(&mut self, step: Step, cx: &mut Context<Self>) -> bool {
        let moves = !matches!(step, Step::Pass | Step::Consume);
        if matches!(step, Step::Drag(_)) && self.slide_layer != self.gesture_layer {
            self.finish_closing();
            self.slide_layer = self.gesture_layer.clone();
        }
        let Some(index) = self.sliding() else { return !matches!(step, Step::Pass) };
        let took = self.slide.follow(step, self.travel(index));
        if moves {
            cx.notify();
        }
        took
    }

    pub fn toggle_expanded(&mut self, key: &SharedString, cx: &mut Context<Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.clone());
        }
        cx.notify();
    }

    /// Whether the sidebar shows, rather than the rail.
    pub fn sidebar_shown(&self) -> bool {
        self.sidebar_open && (!self.sidebar_folded || self.sidebar_beside_compose)
    }

    /// Opened while the docked compose column folds it, the sidebar stays beside the column.
    pub fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_shown() {
            self.sidebar_open = false;
            self.sidebar_beside_compose = false;
        } else {
            self.sidebar_open = true;
            self.sidebar_beside_compose = self.sidebar_folded;
        }
        cx.notify();
    }

    pub fn open_context_menu(
        &mut self,
        id: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A row outside the selection is acted on alone (MS-305).
        if self.selection().is_some_and(|selection| !selection.contains(&id)) {
            self.selection.clear();
        }
        let Some(menu) = self.build_context_menu(&id, window, cx) else { return };
        let dismissed = cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
            if this.context_menu.as_ref().is_some_and(|open| &open.menu == menu) {
                this.dismiss_menu(window, cx);
            }
        });
        menu.focus_handle(cx).focus(window, cx);
        self.context_menu = Some(ContextMenu { id, position, menu, _dismissed: dismissed });
        cx.notify();
    }

    pub fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// ↑/↓: the highlight moves to the row before or after, alone (MS-305: it ends a selection).
    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_closing();
        self.clear_selection(cx);
        let (lines, in_bundle) = self.active_lines();
        if lines.is_empty() {
            return;
        }
        let current = if in_bundle { &self.bundle_highlight } else { &self.highlight };
        let position = current.as_ref().and_then(|id| lines.iter().position(|(line, _)| line == id));
        let target = match position {
            None => if delta > 0 { 0 } else { lines.len() - 1 },
            Some(at) => (at as isize + delta).clamp(0, lines.len() as isize - 1) as usize,
        };
        let (id, scroll_index) = lines[target].clone();
        let reading = self.reader().is_some();
        if in_bundle {
            self.bundle_highlight = Some(id.clone());
            self.bundle_scroll.scroll_to_reveal_item(scroll_index);
        } else {
            self.highlight = Some(id.clone());
            self.list_scroll.scroll_to_reveal_item(scroll_index);
        }
        // In the reader, the keys page through the conversations under it, in place.
        if reading && Some(target) != position {
            self.layers.pop();
            self.open_row(&id, false, window, cx);
        }
        cx.notify();
    }

    pub fn next(&mut self, _: &Next, window: &mut Window, cx: &mut Context<Self>) {
        self.step(1, window, cx);
    }

    pub fn previous(&mut self, _: &Previous, window: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, window, cx);
    }

    pub fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader().is_some() {
            return;
        }
        self.clear_selection(cx);
        let current = if self.open_bundle().is_some() { &self.bundle_highlight } else { &self.highlight };
        if let Some(id) = current.clone() {
            self.activate(&id, window, cx);
        }
    }

    /// Opens a row: a conversation in the reader, or a bundle over the list.
    pub fn activate(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_row(id, true, window, cx);
    }

    fn open_row(&mut self, id: &str, animate: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_closing();
        window.focus(&self.focus, cx);
        if id.starts_with("bundle:") {
            let Some(kind) = self.items.iter().find_map(|item| match item {
                Item::Bundle(bundle) if BundleKind::Mail(bundle.category).id() == id => Some(BundleKind::Mail(bundle.category)),
                Item::Chat(_) if BundleKind::Chat.id() == id => Some(BundleKind::Chat),
                _ => None,
            }) else {
                return;
            };
            // A conversation open beside the list stays open beside the bundle (MS-307), and its
            // row in the list stays the one picked.
            if self.reader().is_none() {
                self.highlight = Some(id.to_owned());
            }
            self.bundle_highlight = None;
            self.bundle_scroll = list_state(0);
            self.push_layer(Layer::Bundle(kind), animate);
            cx.notify();
            return;
        }
        // A space of the Chat bundle opens in Chat (MS-330).
        if id.starts_with("chat:") {
            let Some(space) = self.chat_bundle().and_then(|bundle| bundle.spaces.iter().find(|s| s.row_id() == id).cloned())
            else {
                return;
            };
            self.bundle_highlight = Some(id.to_owned());
            self.select_chat(None, window, cx);
            self.chat.update(cx, |chat, cx| chat.open_space(space.account, space.space, window, cx));
            return;
        }
        let Some(conversation) = self.list().and_then(|l| l.conversations.iter().find(|c| c.id == id)).cloned() else {
            return;
        };
        if let Some(kind) = self.open_bundle() {
            self.bundle_highlight = Some(id.to_owned());
            // The list under the bundle points at the bundle the open conversation is in.
            self.highlight = Some(kind.id());
        } else {
            self.highlight = Some(id.to_owned());
        }
        self.reader_scroll = ScrollHandle::new();
        self.reader_pin = ReaderPin::Opening;
        self.reader_top = ReaderTop::Top;
        self.hovered_message = None;
        self.load_thread(&conversation, false, cx);
        let unread = conversation.unread;
        // A conversation open beside the list gives way to this one in place.
        let replacing = matches!(self.layers.last(), Some(Layer::Reader(_)));
        if replacing {
            self.layers.pop();
        }
        self.push_layer(Layer::Reader(Box::new(conversation.clone())), animate && !replacing);
        // Loaded already: it opens where its messages say now.
        self.pin_opening(&conversation.id);
        if unread {
            self.set_unread(std::slice::from_ref(&conversation), false, cx);
        }
        cx.notify();
    }

    /// Esc: ends a selection (MS-305), else the top layer goes, else the search.
    pub fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        if self.clear_selection(cx) {
            return;
        }
        self.finish_closing();
        // No pointer: the top layer goes, whatever lies under the pointer (MS-299).
        if !self.layers.is_empty() {
            self.slide_off(self.layers.len() - 1);
        } else if self.query.is_some() {
            self.search.update(cx, |search, cx| search.clean(window, cx));
            self.end_search(cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The list's ‹ (its title row): the open bundle goes, a conversation beside the list staying
    /// open (MS-307); without a bundle, as Back.
    pub fn list_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_closing();
        let Some(index) = self.layers.iter().rposition(|layer| matches!(layer, Layer::Bundle(_) | Layer::Search)) else {
            return self.back(&Back, window, cx);
        };
        self.slide_off(index);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Layer `index` slides off to the right, like a swipe that went all the way, and closes.
    fn slide_off(&mut self, index: usize) {
        let travel = self.travel(index);
        if travel <= 0.0 {
            self.layers.remove(index);
            return;
        }
        let from = if self.sliding() == Some(index) { self.current_offset() } else { 0. };
        self.slide_layer = Some(self.layers[index].id());
        self.slide = Slide::settle(from, travel, true);
    }

    pub fn toggle_fold(&mut self, message: &str, cx: &mut Context<Self>) {
        self.reader_pin = ReaderPin::Free;
        if !self.unfolded.remove(message) {
            self.unfolded.insert(message.to_owned());
        }
        cx.notify();
    }

    /// Opens or closes a message's quoted history.
    pub fn toggle_quotes(&mut self, message: &str, cx: &mut Context<Self>) {
        self.reader_pin = ReaderPin::Free;
        if !self.quotes_open.remove(message) {
            self.quotes_open.insert(message.to_owned());
        }
        cx.notify();
    }

    /// Opens or closes a message's full headers.
    pub fn toggle_headers(&mut self, message: &str, cx: &mut Context<Self>) {
        self.reader_pin = ReaderPin::Free;
        if !self.headers_open.remove(message) {
            self.headers_open.insert(message.to_owned());
        }
        cx.notify();
    }

    /// Closes the open conversation, Chat's open space or new-chat form, or the calendar's open
    /// event (⌘W, MS-69); nothing else.
    pub fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_view.is_some() {
            self.chat.update(cx, |chat, cx| chat.close_space(window, cx));
        } else if self.calendar_view {
            self.calendar.update(cx, |calendar, cx| calendar.close_event(cx));
        } else if self.reader().is_some() {
            self.back(&Back, window, cx);
        }
    }
}

/// The layer a close gesture at `x` (from the content area's left) takes, of `readers` (the live
/// layers bottom to top, true for a conversation) with a conversation beside the list starting at
/// `beside` (MS-299): over that conversation, it; over the list's island, the bundle on it, or
/// none; otherwise (one pane, every layer covering the list's island) the top one.
fn gesture_target(readers: &[bool], beside: Option<f32>, x: f32) -> Option<usize> {
    let top = readers.len().checked_sub(1)?;
    match beside {
        Some(edge) if readers[top] && x < edge => readers.iter().rposition(|reader| !reader),
        _ => Some(top),
    }
}

#[cfg(test)]
mod tests {
    use super::gesture_target;

    #[test]
    fn a_gesture_closes_the_layer_under_the_pointer() {
        // A bundle with a conversation beside the list, which ends at 400.
        let bundle_and_reader = [false, true];
        assert_eq!(gesture_target(&bundle_and_reader, Some(400.), 120.), Some(0), "over the bundle");
        assert_eq!(gesture_target(&bundle_and_reader, Some(400.), 600.), Some(1), "over the conversation");
        // A conversation beside the bare list: over the list, nothing to close.
        assert_eq!(gesture_target(&[true], Some(400.), 120.), None);
        assert_eq!(gesture_target(&[true], Some(400.), 600.), Some(0));
        // A bundle alone, beside nothing.
        assert_eq!(gesture_target(&[false], None, 120.), Some(0));
        // One pane: the conversation covers the bundle and the list.
        assert_eq!(gesture_target(&bundle_and_reader, None, 120.), Some(1));
        assert_eq!(gesture_target(&[], Some(400.), 120.), None);
    }
}
