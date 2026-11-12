//! Selection, the layers over the list, keyboard movement and search.

use super::*;
use crate::design::{self, motion};
use crate::swipe::{self, Step};
use gpui::ScrollWheelEvent;

impl MailApp {
    pub fn select(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.source == source && self.query.is_none() && self.chat_view.is_none();
        self.chat_view = None;
        self.context_menu = None;
        self.source = source;
        self.query = None;
        self.search.update(cx, |search, cx| search.clear(cx));
        self.layers.clear();
        if !same {
            self.highlight = None;
            self.list_scroll = list_state(0);
        }
        self.rebuild_items();
        self.load_list(self.key(), false, cx);
        window.focus(&self.focus);
        cx.notify();
    }

    /// Shows Google Chat: every account's spaces (None) or one account's.
    pub fn select_chat(&mut self, account: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_view = Some(account.clone());
        self.context_menu = None;
        self.hovered_row = None;
        self.chat.update(cx, |chat, cx| chat.show(account, window, cx));
        cx.notify();
    }

    pub(super) fn on_search_event(
        &mut self,
        search: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Submit => {
                let text = search.read(cx).text().trim().to_owned();
                if text.is_empty() {
                    self.end_search(cx);
                } else {
                    self.chat_view = None;
                    self.query = Some(text.into());
                    self.layers.clear();
                    self.highlight = None;
                    self.list_scroll = list_state(0);
                    self.rebuild_items();
                    self.load_list(self.key(), false, cx);
                }
                window.focus(&self.focus);
            }
            InputEvent::Cancel => {
                search.update(cx, |search, cx| search.clear(cx));
                self.end_search(cx);
                window.focus(&self.focus);
            }
            InputEvent::Changed => {
                if search.read(cx).text().is_empty() && self.query.is_some() {
                    self.end_search(cx);
                }
            }
        }
        cx.notify();
    }

    fn end_search(&mut self, cx: &mut Context<Self>) {
        if self.query.take().is_some() {
            self.layers.clear();
            self.highlight = None;
            self.list_scroll = list_state(0);
            self.rebuild_items();
            self.load_list(self.key(), false, cx);
        }
    }

    pub fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.search.focus_handle(cx);
        window.focus(&focus);
        self.search.update(cx, |search, cx| search.select_everything(cx));
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
    pub(super) fn active_lines(&self) -> (Vec<(SharedString, usize)>, bool) {
        if let Some(bundle) = self.open_bundle_members() {
            let Some(list) = self.list() else { return (Vec::new(), true) };
            let lines =
                bundle.members.iter().enumerate().map(|(row, &ix)| (list.conversations[ix].id.clone(), row)).collect();
            return (lines, true);
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

    pub fn item_id(&self, item: &Item) -> SharedString {
        match item {
            Item::Section(title) => format!("section:{title}").into(),
            Item::Waiting(_) => "section:waiting".into(),
            Item::Row(index) => self.list().and_then(|l| l.conversations.get(*index)).map(|c| c.id.clone()).unwrap_or_default(),
            Item::Bundle(bundle) => format!("bundle:{}", bundle.category.key()).into(),
        }
    }

    pub fn open_bundle(&self) -> Option<Category> {
        self.live_layers().iter().rev().find_map(|l| match l {
            Layer::Bundle(category) => Some(*category),
            Layer::Reader(_) => None,
        })
    }

    /// The bundle lying over the list.
    pub fn open_bundle_members(&self) -> Option<Rc<Bundle>> {
        let category = self.open_bundle()?;
        self.items.iter().find_map(|item| match item {
            Item::Bundle(bundle) if bundle.category == category => Some(bundle.clone()),
            _ => None,
        })
    }

    /// The layers that stay: all but one sliding off after a back.
    pub fn live_layers(&self) -> &[Layer] {
        if self.closing() { &self.layers[..self.layers.len() - 1] } else { &self.layers }
    }

    /// The top layer is sliding off and goes when it arrives.
    fn closing(&self) -> bool {
        matches!(self.slide, Slide::Settle { back: true, .. })
            && self.layers.last().map(Layer::id).as_ref() == self.slide_layer.as_ref()
    }

    pub fn reader(&self) -> Option<&Conversation> {
        match self.live_layers().last() {
            Some(Layer::Reader(conversation)) => Some(conversation),
            _ => None,
        }
    }

    /// Lays a layer over the others; `animate` slides it in from the right.
    fn push_layer(&mut self, layer: Layer, animate: bool) {
        self.finish_closing();
        self.slide_layer = Some(layer.id());
        self.slide = if animate { Slide::Opening(Instant::now()) } else { Slide::Rest };
        self.layers.push(layer);
    }

    /// A layer still sliding off goes now, so what follows acts on what lies beneath.
    fn finish_closing(&mut self) {
        if self.closing() {
            self.layers.pop();
            self.slide = Slide::Rest;
            self.slide_layer = None;
        }
    }

    /// How far right of its place the top layer sits now, without advancing anything.
    fn current_offset(&self) -> f32 {
        let width = self.content_width;
        match self.slide {
            Slide::Rest => 0.0,
            Slide::Drag(offset) => offset,
            Slide::Opening(start) => (1.0 - design::house_curve(progress(start))) * width,
            Slide::Settle { from, to, start, .. } => from + (to - from) * design::house_curve(progress(start)),
        }
    }

    /// Where the top layer sits in a content area `width` wide, called while rendering: advances
    /// the slide, asks for the next frame while it moves, and drops a layer that slid off.
    pub fn top_offset(&mut self, width: f32, window: &mut Window) -> f32 {
        self.content_width = width;
        let Some(top) = self.layers.last().map(Layer::id) else {
            self.slide = Slide::Rest;
            return 0.0;
        };
        if self.slide_layer.as_ref() != Some(&top) {
            self.slide_layer = Some(top);
            self.slide = Slide::Rest;
        }
        let offset = self.current_offset();
        match self.slide {
            Slide::Rest | Slide::Drag(_) => offset,
            Slide::Opening(start) | Slide::Settle { start, .. } if progress(start) < 1.0 => {
                window.request_animation_frame();
                offset
            }
            Slide::Settle { back: true, .. } => {
                self.finish_closing();
                0.0
            }
            Slide::Opening(_) | Slide::Settle { .. } => {
                self.slide = Slide::Rest;
                0.0
            }
        }
    }

    /// A scroll event in the content area, in the capture phase: true when the back swipe took it.
    pub fn feed_swipe(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        let can_start = !self.live_layers().is_empty() && self.context_menu.is_none();
        match self.swipe.feed(event, can_start) {
            Step::Pass => false,
            Step::Consume => true,
            Step::Drag(offset) => {
                self.finish_closing();
                self.slide_layer = self.layers.last().map(Layer::id);
                self.slide = Slide::Drag(offset);
                cx.notify();
                true
            }
            Step::Release { offset, velocity } => {
                let (back, to) = swipe::settle(offset, velocity, self.content_width);
                self.slide = Slide::Settle { from: offset, to, start: Instant::now(), back };
                cx.notify();
                true
            }
        }
    }

    pub fn toggle_expanded(&mut self, key: &SharedString, cx: &mut Context<Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.clone());
        }
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    pub fn open_context_menu(
        &mut self,
        id: SharedString,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.context_menu = Some(ContextMenu { id, position, focus });
        cx.notify();
    }

    pub fn close_menu(&mut self, _: &CloseMenu, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_menu(window, cx);
    }

    pub fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_closing();
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
        let current = if self.open_bundle().is_some() { &self.bundle_highlight } else { &self.highlight };
        if let Some(id) = current.clone() {
            self.activate(&id, window, cx);
        }
    }

    /// Opens a row: a conversation in the reader, or a bundle over the list.
    pub fn activate(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.open_row(id, true, window, cx);
    }

    fn open_row(&mut self, id: &SharedString, animate: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_closing();
        window.focus(&self.focus);
        if let Some(category) = id.strip_prefix("bundle:") {
            let Some(category) = self.items.iter().find_map(|item| match item {
                Item::Bundle(bundle) if bundle.category.key() == category => Some(bundle.category),
                _ => None,
            }) else {
                return;
            };
            self.highlight = Some(id.clone());
            self.bundle_highlight = None;
            self.bundle_scroll = list_state(0);
            // A conversation open beside the list closes with it.
            self.layers.retain(|layer| matches!(layer, Layer::Bundle(_)));
            self.push_layer(Layer::Bundle(category), animate);
            cx.notify();
            return;
        }
        let Some(conversation) = self.list().and_then(|l| l.conversations.iter().find(|c| &c.id == id)).cloned() else {
            return;
        };
        if self.open_bundle().is_some() {
            self.bundle_highlight = Some(id.clone());
        } else {
            self.highlight = Some(id.clone());
        }
        self.reader_scroll = ScrollHandle::new();
        self.reader_pinned = true;
        self.hovered_message = None;
        self.load_thread(&conversation, false, cx);
        let unread = conversation.unread;
        // A conversation open beside the list gives way to this one in place.
        let replacing = matches!(self.layers.last(), Some(Layer::Reader(_)));
        if replacing {
            self.layers.pop();
        }
        self.push_layer(Layer::Reader(Box::new(conversation.clone())), animate && !replacing);
        if unread {
            self.set_unread(&conversation, false, cx);
        }
        cx.notify();
    }

    pub fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_closing();
        if let Some(top) = self.layers.last() {
            if self.content_width <= 0.0 {
                self.layers.pop();
            } else {
                // The top layer slides off to the right, like a swipe that went all the way.
                let from = self.current_offset();
                self.slide_layer = Some(top.id());
                self.slide = Slide::Settle { from, to: self.content_width, start: Instant::now(), back: true };
            }
        } else if self.query.is_some() {
            self.search.update(cx, |search, cx| search.clear(cx));
            self.end_search(cx);
        }
        window.focus(&self.focus);
        cx.notify();
    }

    pub fn toggle_fold(&mut self, message: &SharedString, cx: &mut Context<Self>) {
        self.reader_pinned = false;
        if !self.unfolded.remove(message) {
            self.unfolded.insert(message.clone());
        }
        cx.notify();
    }

    /// Opens or closes a message's quoted history.
    pub fn toggle_quotes(&mut self, message: &SharedString, cx: &mut Context<Self>) {
        self.reader_pinned = false;
        if !self.quotes_open.remove(message) {
            self.quotes_open.insert(message.clone());
        }
        cx.notify();
    }

    /// Closes the open conversation (⌘W, MS-69); nothing else.
    pub fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_view.is_none() && self.reader().is_some() {
            self.back(&Back, window, cx);
        }
    }
}

/// How far through the standard slide a motion that began at `start` is, 0 to 1.
fn progress(start: Instant) -> f32 {
    (start.elapsed().as_secs_f32() / motion::ENTER.as_secs_f32()).min(1.0)
}
