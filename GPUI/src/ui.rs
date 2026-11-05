//! The window: sidebar, toolbar, the conversation list, and the layers that slide over it (a
//! Smart Inbox bundle, the reader). The list stays mounted under its layers.

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, BoxShadow, ClickEvent, Context, Div, ElementId, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, div, point, px, relative, uniform_list,
};

use crate::design::{self, Palette, UI_FONT, layout, list as metrics, motion, radius, text};
use crate::format;
use crate::model::{Address, Category, Conversation, Message, Role};
use crate::state::{CONTEXT, Item, Layer, List, MailApp, PagedView, Source, ThreadState};

/// The row's hover actions: Archive, Trash and the star.
const HOVER_ACTIONS_WIDTH: f32 = 150.0;

impl Render for MailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(window.appearance());
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::trash))
            .on_action(cx.listener(Self::star))
            .on_action(cx.listener(Self::toggle_read))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::refresh))
            .size_full()
            .flex()
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .text_color(p.text)
            .bg(p.page)
            .child(self.render_sidebar(p, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_toolbar(p, cx))
                    .child(self.render_content(p, window, cx)),
            )
    }
}

/// Mouse-down in the title bar region moves the window; a double click zooms it.
fn title_bar(id: &'static str) -> Stateful<Div> {
    div().id(id).on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

/// A text button that keeps its click from moving the window.
fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .text_size(px(text::FOOTNOTE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .active(|s| s.bg(p.selection))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into())
}

fn avatar(address: &Address, size: f32, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(design::avatar_color(&address.email, p.dark))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.43))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.on_accent)
        .child(address.initial())
}

fn chip(label: SharedString, color: Hsla) -> Div {
    let mut fill = color;
    fill.a *= 0.14;
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(radius::SMALL))
        .bg(fill)
        .text_color(color)
        .text_size(px(text::MICRO))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn centered_note(note: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .items_center()
        .justify_center()
        .px(px(32.))
        .text_color(p.muted)
        .child(div().text_center().child(note.into()))
}

impl MailApp {
    // MARK: - Sidebar

    fn render_sidebar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let inbox_unread: u32 = self
            .mailboxes
            .values()
            .flatten()
            .filter(|b| b.role == Role::Inbox)
            .filter_map(|b| b.unread)
            .sum();
        let mut rows: Vec<AnyElement> = Role::UNIFIED
            .iter()
            .map(|&role| {
                let unread = if role == Role::Inbox { inbox_unread } else { 0 };
                self.sidebar_row(Source::Unified(role), unread, p, cx).into_any_element()
            })
            .collect();
        for (index, account) in self.accounts.iter().enumerate() {
            rows.push(
                div()
                    .mt(px(16.))
                    .mb(px(4.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(text::CAPTION))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted)
                    .child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(design::account_color(index)))
                    .child(div().truncate().child(account.title()))
                    .into_any_element(),
            );
            let boxes = self.mailboxes.get(&account.id).map(Vec::as_slice).unwrap_or_default();
            for b in boxes.iter().filter(|b| b.role != Role::Starred && b.role != Role::Important) {
                let source = Source::Mailbox {
                    account: account.id.clone(),
                    id: b.id.clone(),
                    name: if b.name.is_empty() { b.role.title().into() } else { b.name.clone() },
                    role: b.role,
                };
                let unread = if matches!(b.role, Role::Inbox | Role::Other) { b.unread.unwrap_or(0) } else { 0 };
                rows.push(self.sidebar_row(source, unread, p, cx).into_any_element());
            }
        }
        div()
            .w(px(layout::SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.line)
            .child(title_bar("sidebar-title").h(px(layout::TOOLBAR_HEIGHT)).flex_shrink_0())
            .child(
                div()
                    .id("sidebar")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(10.))
                    .pb(px(16.))
                    .children(rows),
            )
    }

    fn sidebar_row(&self, source: Source, unread: u32, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.source == source && self.query.is_none();
        let id: SharedString = match &source {
            Source::Unified(role) => format!("unified:{}", role.arg()).into(),
            Source::Mailbox { account, id, .. } => format!("box:{account}:{id}").into(),
        };
        let title = source.title();
        div()
            .id(ElementId::Name(id))
            .h(px(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(radius::CONTROL))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection).font_weight(FontWeight::MEDIUM))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select(source.clone(), window, cx)))
            .child(div().flex_1().min_w_0().truncate().child(title))
            .when(unread > 0, |d| {
                d.child(div().flex_shrink_0().text_size(px(text::CAPTION)).text_color(p.muted).child(unread.to_string()))
            })
    }

    // MARK: - Toolbar

    fn render_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, count): (SharedString, Option<usize>) = match (self.layers.last(), &self.query) {
            (Some(Layer::Reader(_)), _) => ("".into(), None),
            (Some(Layer::Bundle(category)), _) => {
                (category.title().into(), self.open_bundle_members().map(|b| b.members.len()))
            }
            (None, Some(query)) => {
                (format!("“{query}”").into(), self.list().filter(|l| !l.loading).map(|l| l.conversations.len()))
            }
            (None, None) => (self.source.title(), None),
        };
        let back_to: Option<SharedString> = match self.layers.len() {
            0 if self.query.is_some() => Some(self.source.title()),
            0 => None,
            depth => Some(match self.layers.get(depth.wrapping_sub(2)) {
                Some(Layer::Bundle(category)) => category.title().into(),
                _ if self.query.is_some() => "Results".into(),
                _ => self.source.title(),
            }),
        };
        let reader = self.reader().cloned();

        title_bar("toolbar")
            .h(px(layout::TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(14.))
            .border_b_1()
            .border_color(p.line)
            .when_some(back_to, |d, back| {
                d.child(
                    button("back", format!("‹  {back}"), p)
                        .text_color(p.accent)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(&crate::state::Back, window, cx))),
                )
            })
            .child(
                div()
                    .flex_shrink()
                    .min_w_0()
                    .truncate()
                    .ml(px(4.))
                    .text_size(px(text::HEADLINE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when_some(count, |d, count| {
                d.child(div().flex_shrink_0().text_color(p.muted).text_size(px(text::FOOTNOTE)).child(count.to_string()))
            })
            .child(div().flex_1())
            .when_some(reader, |d, conversation| {
                let star = if conversation.starred { "★ Starred" } else { "☆ Star" };
                let read = if conversation.unread { "Mark Read" } else { "Mark Unread" };
                d.child(
                    button("archive", "Archive", p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.archive(&crate::state::Archive, window, cx))),
                )
                .child(
                    button("trash", "Trash", p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.trash(&crate::state::Trash, window, cx))),
                )
                .child(
                    button("star", star, p)
                        .when(conversation.starred, |b| b.text_color(p.waiting))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.star(&crate::state::Star, window, cx))),
                )
                .child(button("read", read, p).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_read(&crate::state::ToggleRead, window, cx)),
                ))
                .child(div().w(px(6.)))
            })
            .child(
                div()
                    .id("search")
                    .flex_shrink_0()
                    .w(px(240.))
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(radius::CONTROL))
                    .bg(p.chip)
                    .line_height(px(18.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().text_color(p.muted).child("⌕"))
                    .child(self.search.clone()),
            )
            .child(
                button("refresh", "Refresh", p)
                    .text_color(p.muted)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&crate::state::Refresh, window, cx))),
            )
    }

    // MARK: - Content

    fn render_content(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width) - layout::SIDEBAR_WIDTH;
        let layers: Vec<AnyElement> = self
            .layers
            .iter()
            .map(|layer| {
                let (id, body): (SharedString, AnyElement) = match layer {
                    Layer::Bundle(category) => {
                        (format!("bundle:{}", category.key()).into(), self.render_bundle(*category, p, cx))
                    }
                    Layer::Reader(conversation) => {
                        (format!("reader:{}", conversation.id).into(), self.render_reader(conversation, p, cx))
                    }
                };
                div()
                    .id(ElementId::Name(id.clone()))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w_full()
                    .bg(p.page)
                    .shadow(vec![BoxShadow {
                        color: p.shadow,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(14.),
                        spread_radius: px(0.),
                    }])
                    // The layer keeps the keyboard; clicks never reach the list beneath.
                    .occlude()
                    .child(body)
                    .with_animation(
                        ElementId::Name(format!("slide:{id}").into()),
                        Animation::new(motion::STANDARD).with_easing(design::house_curve),
                        move |layer, progress| layer.left(px((1.0 - progress) * width)),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(self.render_list(p, cx))
            .children(layers)
            .when_some(self.toast.as_ref(), |d, toast| {
                d.child(
                    div().absolute().bottom(px(20.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .max_w(px(560.))
                            .px(px(14.))
                            .py(px(8.))
                            .rounded(px(radius::CARD))
                            .bg(p.text)
                            .text_color(p.page)
                            .text_size(px(text::FOOTNOTE))
                            .child(toast.text.clone()),
                    ),
                )
            })
    }

    fn render_list(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.items.is_empty() {
            let list = self.list();
            if let Some(offline) = &self.offline {
                return centered_note(offline.clone(), p)
                    .child(
                        button("retry", "Try Again", p)
                            .text_color(p.accent)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.retry(cx))),
                    )
                    .into_any_element();
            }
            let note: SharedString = match list {
                Some(list) if list.loading => "Loading…".into(),
                Some(List { error: Some(error), .. }) => error.clone(),
                None => "Loading…".into(),
                Some(_) if self.query.is_some() => "No results".into(),
                Some(_) => "No conversations".into(),
            };
            return centered_note(note, p).into_any_element();
        }
        uniform_list(
            "conversations",
            self.items.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                if range.end + 20 >= this.items.len() {
                    this.load_more(PagedView::List, this.items.len(), cx);
                }
                let role = this.source.role();
                range.map(|ix| this.render_item(ix, role, p, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list_scroll.clone())
        .size_full()
        .px(px(metrics::INSET))
        .py(px(6.))
        .into_any_element()
    }

    fn render_item(&self, ix: usize, role: Role, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(list) = self.list() else { return div().into_any_element() };
        match &self.items[ix] {
            Item::Section(title) => div()
                .h(px(metrics::ROW_HEIGHT))
                .flex()
                .items_end()
                .pb(px(6.))
                .pl(px(metrics::SENDER_X))
                .text_size(px(text::CAPTION))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted)
                .child(title.clone())
                .into_any_element(),
            Item::Row(index) => {
                let conversation = &list.conversations[*index];
                let highlighted = self.highlight.as_ref() == Some(&conversation.id);
                self.render_row(conversation, highlighted, role, p, cx)
            }
            Item::Bundle(bundle) => {
                let newest = &list.conversations[bundle.members[0]];
                let id: SharedString = format!("bundle:{}", bundle.category.key()).into();
                let highlighted = self.highlight.as_ref() == Some(&id);
                let weight = if bundle.unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL };
                row_frame(id.clone(), highlighted, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(&id, window, cx)))
                    .when(bundle.unread, |d| d.child(unread_dot(p)))
                    .child(
                        div()
                            .absolute()
                            .left(px(metrics::SENDER_X))
                            .top(px((metrics::ROW_HEIGHT - metrics::AVATAR) / 2.))
                            .size(px(metrics::AVATAR))
                            .rounded_full()
                            .bg(p.chip)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(text::CAPTION))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.muted)
                            .child(if bundle.members.len() > 99 { "99+".to_owned() } else { bundle.members.len().to_string() }),
                    )
                    .child(sender_cell(bundle.category.title().into(), weight))
                    .child(
                        subject_cell()
                            .child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(bundle.senders.clone())),
                    )
                    .child(date_cell(p).child(newest.when.clone()))
                    .into_any_element()
            }
        }
    }

    fn render_row(
        &self,
        conversation: &Conversation,
        highlighted: bool,
        role: Role,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = conversation.id.clone();
        let weight = if conversation.unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL };
        // Sent mail names who it went to.
        let sender = if role.shows_recipients() {
            conversation
                .participants
                .iter()
                .find(|name| name.as_ref() != "me")
                .cloned()
                .unwrap_or_else(|| conversation.sender.clone())
        } else {
            conversation.sender.clone()
        };
        // Hover styles apply at paint time and cannot move layout, so the hovered row is state:
        // its subject makes room for the actions, which take the date's place.
        let hovered = self.hovered_row.as_ref() == Some(&id);
        let hover_actions = hovered.then(|| {
            let (archive, trash, star) = (id.clone(), id.clone(), id.clone());
            div()
                .absolute()
                .right(px(metrics::TRAILING_INSET - 6.))
                .w(px(HOVER_ACTIONS_WIDTH))
                .top_0()
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(2.))
                .child(button("row-archive", "Archive", p).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if let Some(c) = this.find(&archive) {
                        this.remove(c, "mail.archive", "archive", window, cx);
                    }
                })))
                .child(button("row-trash", "Trash", p).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if let Some(c) = this.find(&trash) {
                        this.remove(c, "mail.trash", "move it to the Trash", window, cx);
                    }
                })))
                .child(
                    button("row-star", if conversation.starred { "★" } else { "☆" }, p)
                        .when(conversation.starred, |b| b.text_color(p.waiting))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if let Some(c) = this.find(&star) {
                                this.set_starred(&c, !c.starred, cx);
                            }
                        })),
                )
        });
        let hover_id = id.clone();
        row_frame(id.clone(), highlighted, p)
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if *hovering {
                    this.hovered_row = Some(hover_id.clone());
                } else if this.hovered_row.as_ref() == Some(&hover_id) {
                    this.hovered_row = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(&id, window, cx)))
            .when(conversation.unread, |d| d.child(unread_dot(p)))
            .child(
                avatar(&conversation.avatar(), metrics::AVATAR, p)
                    .absolute()
                    .left(px(metrics::SENDER_X))
                    .top(px((metrics::ROW_HEIGHT - metrics::AVATAR) / 2.)),
            )
            .child(sender_cell(sender, weight).when(conversation.message_count > 1, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(text::CAPTION))
                        .text_color(p.muted)
                        .child(conversation.message_count.to_string()),
                )
            }))
            .child(
                subject_cell()
                    .when(hovered, |d| d.right(px(metrics::TRAILING_INSET + HOVER_ACTIONS_WIDTH)))
                    .when(conversation.waiting, |d| d.child(chip("Waiting on you".into(), p.waiting)))
                    .when(!conversation.tag.is_empty(), |d| {
                        d.child(div().flex_shrink_0().text_color(p.muted).child(conversation.tag.clone()))
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(relative(0.7))
                            .truncate()
                            .font_weight(weight)
                            .child(conversation.title.clone()),
                    )
                    .when_some(conversation.snippet.as_ref().filter(|s| !s.is_empty()), |d, snippet| {
                        d.child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(snippet.clone()))
                    })
                    .children(
                        conversation
                            .labels
                            .iter()
                            .take(2)
                            .map(|label| chip(label.clone(), design::topic_color(label, p.dark))),
                    ),
            )
            .when(!hovered, |d| {
                d.child(
                    date_cell(p)
                        .when(conversation.has_attachment, |d| d.child("📎"))
                        .when(conversation.starred, |d| d.child(div().text_color(p.star).child("★")))
                        .child(conversation.when.clone()),
                )
            })
            .children(hover_actions)
            .into_any_element()
    }

    fn find(&self, id: &SharedString) -> Option<Conversation> {
        self.list()?.conversations.iter().find(|c| &c.id == id).cloned()
    }

    // MARK: - Bundle

    fn render_bundle(&self, category: Category, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(bundle) = self.open_bundle_members().filter(|b| b.category == category) else {
            return centered_note("Nothing here any more", p).into_any_element();
        };
        uniform_list(
            "bundle",
            bundle.members.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                if range.end + 20 >= bundle.members.len() {
                    this.load_more(PagedView::Bundle, bundle.members.len(), cx);
                }
                let Some(list) = this.list() else { return Vec::new() };
                let role = this.source.role();
                range
                    .filter_map(|row| bundle.members.get(row).and_then(|&ix| list.conversations.get(ix)))
                    .map(|conversation| {
                        let highlighted = this.bundle_highlight.as_ref() == Some(&conversation.id);
                        this.render_row(conversation, highlighted, role, p, cx)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.bundle_scroll.clone())
        .size_full()
        .px(px(metrics::INSET))
        .py(px(6.))
        .into_any_element()
    }

    // MARK: - Reader

    fn render_reader(&self, conversation: &Conversation, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match self.threads.get(&conversation.id).map(|t| &t.state) {
            None | Some(ThreadState::Loading) => {
                div().py(px(24.)).text_color(p.muted).child("Loading messages…").into_any_element()
            }
            Some(ThreadState::Failed(error)) => {
                div().py(px(24.)).text_color(p.danger).child(error.clone()).into_any_element()
            }
            Some(ThreadState::Loaded(messages)) => {
                let last = messages.len().saturating_sub(1);
                div()
                    .flex()
                    .flex_col()
                    .children(messages.iter().enumerate().map(|(index, message)| {
                        let open = index == last || message.unread || self.unfolded.contains(&message.id);
                        self.render_message(message, open, index == last, p, cx)
                    }))
                    .into_any_element()
            }
        };
        div()
            .id("reader")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.reader_scroll)
            .child(
                div().w_full().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(layout::READING_WIDTH))
                        .px(px(32.))
                        .pt(px(28.))
                        .pb(px(64.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            div()
                                .text_size(px(text::DISPLAY))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(conversation.title.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(text::FOOTNOTE))
                                .text_color(p.muted)
                                .when(conversation.waiting, |d| d.child(chip("Waiting on you".into(), p.waiting)))
                                .children(
                                    conversation
                                        .labels
                                        .iter()
                                        .map(|label| chip(label.clone(), design::topic_color(label, p.dark))),
                                )
                                .when(!conversation.tag.is_empty(), |d| d.child(conversation.tag.clone()))
                                .child(conversation.account.clone()),
                        )
                        .child(div().mt(px(6.)).child(body)),
                ),
            )
            .into_any_element()
    }

    fn render_message(
        &self,
        message: &Message,
        open: bool,
        last: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sender = message.sender();
        let id = message.id.clone();
        let header = div()
            .id(ElementId::Name(format!("message:{id}").into()))
            .flex()
            .items_center()
            .gap(px(10.))
            .when(!last, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_fold(&id, cx)))
            })
            .child(avatar(&sender, 32., p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .items_baseline()
                            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(sender.display_name()))
                            .when(sender.name.is_some(), |d| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(text::FOOTNOTE))
                                        .text_color(p.muted)
                                        .child(sender.email.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::FOOTNOTE))
                            .text_color(p.muted)
                            .child(if open { message.recipients.clone() } else { message.preview.clone() }),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(text::FOOTNOTE))
                    .text_color(p.muted)
                    .child(message.when.clone()),
            );
        let card = div().py(px(14.)).border_t_1().border_color(p.line).flex().flex_col().gap(px(12.)).child(header);
        if !open {
            return card.into_any_element();
        }
        card.when_some(message.text.as_ref(), |d, text| {
            d.child(
                div()
                    .pl(px(42.))
                    .text_size(px(text::READING))
                    .line_height(relative(1.55))
                    .child(text.clone()),
            )
        })
        .when(message.truncated, |d| {
            d.child(
                div()
                    .pl(px(42.))
                    .text_size(px(text::FOOTNOTE))
                    .text_color(p.muted)
                    .child("The message continues; open it in Mailstream to read the rest."),
            )
        })
        .when_some(message.error.as_ref(), |d, error| {
            d.child(div().pl(px(42.)).text_color(p.danger).child(error.clone()))
        })
        .when(!message.attachments.is_empty(), |d| {
            d.child(div().pl(px(42.)).flex().flex_wrap().gap(px(8.)).children(message.attachments.iter().enumerate().map(
                |(index, attachment)| {
                    let path = attachment.path.clone();
                    div()
                        .id(ElementId::Name(format!("attachment:{}:{index}", message.id).into()))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(radius::CONTROL))
                        .border_1()
                        .border_color(p.line)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .flex()
                        .gap(px(6.))
                        .text_size(px(text::FOOTNOTE))
                        .child(format!("📎 {}", attachment.filename))
                        .child(div().text_color(p.muted).child(format::file_size(attachment.size)))
                        .on_click(move |_, _, cx| {
                            if !path.is_empty() {
                                cx.open_with_system(std::path::Path::new(path.as_ref()));
                            }
                        })
                },
            )))
        })
        .into_any_element()
    }
}


/// A list row: fixed height, hover and keyboard highlight, the `row` hover group.
fn row_frame(id: SharedString, highlighted: bool, p: &'static Palette) -> Stateful<Div> {
    div()
        .id(ElementId::Name(id))
        .relative()
        .h(px(metrics::ROW_HEIGHT))
        .w_full()
        .rounded(px(radius::CONTROL))
        .cursor_pointer()
        .when(highlighted, |d| d.bg(p.selection))
        .when(!highlighted, |d| d.hover(|s| s.bg(p.hover)))
}

fn unread_dot(p: &Palette) -> Div {
    div()
        .absolute()
        .left(px(metrics::DOT_X - 3.))
        .top(px(metrics::ROW_HEIGHT / 2. - 3.))
        .size(px(6.))
        .rounded_full()
        .bg(p.accent)
}

fn sender_cell(name: SharedString, weight: FontWeight) -> Div {
    div()
        .absolute()
        .left(px(metrics::NAME_X))
        .w(px(metrics::SENDER_X + metrics::SENDER_WIDTH - metrics::NAME_X))
        .top_0()
        .h_full()
        .flex()
        .items_center()
        .gap(px(5.))
        .child(div().min_w_0().truncate().font_weight(weight).child(name))
}

fn subject_cell() -> Div {
    div()
        .absolute()
        .left(px(metrics::SUBJECT_X))
        .right(px(metrics::TRAILING_INSET + metrics::DATE_WIDTH + 8.))
        .top_0()
        .h_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .overflow_hidden()
}

fn date_cell(p: &Palette) -> Div {
    div()
        .absolute()
        .right(px(metrics::TRAILING_INSET))
        .w(px(metrics::DATE_WIDTH + 30.))
        .top_0()
        .h_full()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(4.))
        .text_size(px(text::CAPTION))
        .text_color(p.muted)
}
