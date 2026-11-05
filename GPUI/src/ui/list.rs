//! The conversation list and a bundle's list: rows, hover actions, the right-click target.

use super::*;
use gpui::MouseDownEvent;

/// The row's hover actions: Archive (or Move to Inbox), Trash, read and the star.
const HOVER_ACTIONS_WIDTH: f32 = 4. * icon_size::BUTTON + 3. * 2.;

impl MailApp {
    pub(super) fn render_list(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
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
        let photo = avatars::avatar(&conversation.avatar(), metrics::AVATAR, p, cx);
        // Hover styles apply at paint time and cannot move layout, so the hovered row is state:
        // its subject makes room for the actions, which take the date's place.
        let hovered = self.hovered_row.as_ref() == Some(&id);
        let hover_actions = hovered.then(|| {
            let (file, trash, read, star) = (id.clone(), id.clone(), id.clone(), id.clone());
            let restoring = !conversation.can_archive() && conversation.can_restore();
            let (file_icon, file_tip) =
                if restoring { (Icon::Restore, "Move to Inbox") } else { (Icon::Archive, "Archive") };
            let (read_icon, read_tip) =
                if conversation.unread { (Icon::MarkRead, "Mark as Read") } else { (Icon::MarkUnread, "Mark as Unread") };
            let (star_icon, star_tip, star_color) =
                if conversation.starred { (Icon::Starred, "Unstar", p.star) } else { (Icon::Star, "Star", p.muted) };
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
                .child(
                    icon_button("row-file", file_icon, file_tip, conversation.can_archive() || restoring, p.muted, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let Some(c) = this.conversation(&file) else { return };
                            if c.can_archive() {
                                this.remove(c, "mail.archive", "archive", window, cx);
                            } else if c.can_restore() {
                                this.remove(c, "mail.restore", "move it to the Inbox", window, cx);
                            }
                        })),
                )
                .child(icon_button("row-trash", Icon::Delete, "Move to Trash", true, p.muted, p).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&trash) {
                            this.remove(c, "mail.trash", "move it to the Trash", window, cx);
                        }
                    },
                )))
                .child(icon_button("row-read", read_icon, read_tip, true, p.muted, p).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&read) {
                            this.set_unread(&c, !c.unread, cx);
                        }
                    },
                )))
                .child(icon_button("row-star", star_icon, star_tip, true, star_color, p).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&star) {
                            this.set_starred(&c, !c.starred, cx);
                        }
                    },
                )))
        });
        let hover_id = id.clone();
        let menu_id = id.clone();
        let menu_open = self.context_menu.as_ref().is_some_and(|menu| menu.id == id);
        row_frame(id.clone(), highlighted || menu_open, p)
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if *hovering {
                    this.hovered_row = Some(hover_id.clone());
                } else if this.hovered_row.as_ref() == Some(&hover_id) {
                    this.hovered_row = None;
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_context_menu(menu_id.clone(), event.position, window, cx);
                }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(&id, window, cx)))
            .when(conversation.unread, |d| d.child(unread_dot(p)))
            .child(
                div()
                    .absolute()
                    .left(px(metrics::SENDER_X))
                    .top(px((metrics::ROW_HEIGHT - metrics::AVATAR) / 2.))
                    .child(photo),
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
                        .when(conversation.has_attachment, |d| d.child(icon(Icon::Attachment, text::BODY, p.muted)))
                        .when(conversation.starred, |d| d.child(icon(Icon::Starred, text::BODY, p.star)))
                        .child(conversation.when.clone()),
                )
            })
            .children(hover_actions)
            .into_any_element()
    }

    // MARK: - Bundle

    pub(super) fn render_bundle(&self, category: Category, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
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
