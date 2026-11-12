//! The conversation list and a bundle's list, on the list island's tray: Mail's two-line rows
//! lifting off it (`ConversationRow`, `BundleRow`), date groups, hover actions on a raised capsule,
//! the right-click target.

use super::*;
use gpui::{ListState, MouseDownEvent, list};

impl MailApp {
    pub(super) fn render_list(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.items.is_empty() {
            let list = self.list();
            let note: SharedString = match list {
                Some(list) if list.loading => "Loading…".into(),
                Some(List { error: Some(error), .. }) => error.clone(),
                None => "Loading…".into(),
                Some(_) if self.query.is_some() => "No matches".into(),
                Some(_) => "No conversations".into(),
            };
            return centered_note(note, p).into_any_element();
        }
        list(
            self.list_scroll.clone(),
            cx.processor(move |this, ix: usize, _, cx| {
                if ix + 20 >= this.items.len() {
                    this.load_more(PagedView::List, this.items.len(), cx);
                }
                // The list's own padding stops at its top and bottom; rows step in by hand.
                div().px(px(metrics::INSET)).child(this.render_item(ix, p, cx)).into_any_element()
            }),
        )
        .size_full()
        .py(px(space::S6))
        .into_any_element()
    }

    /// The id of the row after `ix` in the main list, if a row follows.
    fn next_row(&self, ix: usize) -> Option<SharedString> {
        match self.items.get(ix + 1)? {
            Item::Section(_) | Item::Waiting(_) => None,
            item => Some(self.item_id(item)),
        }
    }

    fn render_item(&self, ix: usize, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(list) = self.list() else { return div().into_any_element() };
        let rule = self.next_row(ix).is_some_and(|next| self.highlight.as_ref() != Some(&next));
        match &self.items[ix] {
            Item::Section(title) => section_header(title.clone(), None, p).into_any_element(),
            Item::Waiting(count) => section_header("Waiting on you", Some(*count), p).into_any_element(),
            Item::Row(index) => {
                let conversation = &list.conversations[*index];
                let selected = self.highlight.as_ref() == Some(&conversation.id);
                self.render_row(conversation, selected, rule, p, cx)
            }
            Item::Bundle(bundle) => {
                let newest = &list.conversations[bundle.members[0]];
                let id: SharedString = format!("bundle:{}", bundle.category.key()).into();
                let selected = self.highlight.as_ref() == Some(&id);
                let unread = bundle.unread > 0;
                let color = design::category_color(bundle.category, p);
                let count = if unread { format!("{} new", bundle.unread) } else { bundle.members.len().to_string() };
                let title = div()
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .child(div().font_weight(if unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD }).child(bundle.category.title()))
                    .child(tag(count, Some(color), p));
                let subtitle = div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.muted)
                    .child(format!("{}  —  {}", bundle.senders, bundle.subject));
                list_row(ElementId::Name(id.clone()), selected, rule, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(&id, window, cx)))
                    .child(row_layout(
                        unread,
                        icon_avatar(Icon::for_category(bundle.category), color, metrics::AVATAR),
                        title,
                        date(newest.when.clone(), p),
                        subtitle,
                        Vec::new(),
                        p,
                    ))
                    .into_any_element()
            }
        }
    }

    fn render_row(
        &self,
        conversation: &Conversation,
        selected: bool,
        rule: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = conversation.id.clone();
        // Sent mail names who it went to.
        let senders = if self.source.role().shows_recipients() {
            conversation
                .participants
                .iter()
                .find(|name| name.as_ref() != "me")
                .cloned()
                .unwrap_or_else(|| conversation.sender.clone())
        } else {
            conversation.senders.clone()
        };
        let photo = avatars::avatar(&conversation.avatar_email(), &conversation.avatar, metrics::AVATAR, p, cx);
        let hovered = self.hovered_row.as_ref() == Some(&id);
        let menu_open = self.context_menu.as_ref().is_some_and(|menu| menu.id == id);

        let title = div()
            .flex()
            .items_center()
            .min_w_0()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(if conversation.unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
                    .child(senders),
            )
            .when(conversation.message_count > 1, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .ml(px(5.))
                        .text_size(px(text::CAPTION))
                        .text_color(p.muted)
                        .child(conversation.message_count.to_string()),
                )
            })
            .when(conversation.waiting, |d| d.child(div().ml(px(space::S6)).child(icon(Icon::Reply, icon_size::INLINE, p.waiting))));

        let account_dot = (self.accounts.len() > 1)
            .then(|| self.accounts.iter().position(|a| a.id == conversation.account))
            .flatten()
            .map(|index| mark_dot(design::account_color(index)));
        let meta = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::S4))
            .when(conversation.has_attachment, |d| d.child(icon(Icon::Attachment, 13., p.muted)))
            .children(account_dot.map(|dot| dot.mr(px(2.))))
            .child(date(conversation.when.clone(), p));

        let subject: SharedString =
            if conversation.title.is_empty() { "(no subject)".into() } else { conversation.title.clone() };
        let subtitle = div()
            .flex_1()
            .flex()
            .items_center()
            .min_w_0()
            .overflow_hidden()
            .text_color(p.muted)
            .when(!conversation.tag.is_empty(), |d| {
                d.child(div().flex_shrink_0().mr(px(space::S4)).child(conversation.tag.clone()))
            })
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(relative(0.75))
                    .truncate()
                    .text_color(p.text)
                    .font_weight(if conversation.unread { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                    .child(subject),
            )
            .when_some(conversation.snippet.as_ref().filter(|s| !s.is_empty()), |d, snippet| {
                d.child(div().flex_1().min_w_0().truncate().child(format!("  —  {snippet}")))
            });

        let mut marks = Vec::new();
        if let Some(topic) = conversation.labels.first() {
            marks.push(tag(topic.clone(), Some(design::topic_color(topic, p.dark)), p).ml(px(space::S2)).into_any_element());
        }
        if conversation.starred {
            marks.push(icon(Icon::Starred, 13., p.star).into_any_element());
        }

        let hover_id = id.clone();
        let menu_id = id.clone();
        list_row(ElementId::Name(id.clone()), selected || menu_open, rule, p)
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
            .child(row_layout(conversation.unread, photo, title, meta, subtitle, marks, p))
            .when(hovered, |d| d.child(self.render_row_actions(conversation, p, cx)))
            .into_any_element()
    }

    /// The row's hover actions, floating over its end on a raised capsule: Resolve, Archive (or
    /// Move to Inbox), Trash, read and the star.
    fn render_row_actions(&self, conversation: &Conversation, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let id = &conversation.id;
        let (resolve, file, trash, read, star) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        let restoring = !conversation.can_archive() && conversation.can_restore();
        let (file_icon, file_tip) = if restoring { (Icon::Restore, "Move to Inbox") } else { (Icon::Archive, "Archive") };
        let (read_icon, read_tip) =
            if conversation.unread { (Icon::MarkRead, "Mark as Read") } else { (Icon::MarkUnread, "Mark as Unread") };
        let (star_icon, star_tip, star_color) =
            if conversation.starred { (Icon::Starred, "Unstar", p.star) } else { (Icon::Star, "Star", p.glyph) };
        div().absolute().right(px(space::S8)).top_0().bottom_0().flex().items_center().child(
            raised_capsule(p)
                .when(conversation.waiting, |d| {
                    d.child(icon_button("row-resolve", Icon::Resolve, "Resolve (D)", true, p.waiting).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if let Some(c) = this.conversation(&resolve) {
                                this.set_waiting(&c, false, cx);
                            }
                        }),
                    ))
                })
                .when(conversation.can_archive() || restoring, |d| {
                    d.child(icon_button("row-file", file_icon, file_tip, true, p.glyph).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let Some(c) = this.conversation(&file) else { return };
                            if c.can_archive() {
                                this.remove(c, Removal::Archive, window, cx);
                            } else if c.can_restore() {
                                this.remove(c, Removal::Restore, window, cx);
                            }
                        },
                    )))
                })
                .child(icon_button("row-trash", Icon::Delete, "Move to Trash", true, p.glyph).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&trash) {
                            this.remove(c, Removal::Trash, window, cx);
                        }
                    },
                )))
                .child(icon_button("row-read", read_icon, read_tip, true, p.glyph).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&read) {
                            this.set_unread(&c, !c.unread, cx);
                        }
                    },
                )))
                .child(icon_button("row-star", star_icon, star_tip, true, star_color).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&star) {
                            this.set_starred(&c, !c.starred, cx);
                        }
                    },
                ))),
        )
    }

    // MARK: - Bundle

    /// An open bundle: its rows on a page of the tray sliding over the list's.
    pub(super) fn render_bundle(&self, category: Category, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(bundle) = self.open_bundle_members().filter(|b| b.category == category) else {
            return centered_note("Nothing here any more", p).into_any_element();
        };
        let count = bundle.members.len();
        if self.bundle_scroll.item_count() != count {
            keep_top(&self.bundle_scroll, count);
        }
        list(
            self.bundle_scroll.clone(),
            cx.processor(move |this, row: usize, _, cx| {
                if row + 20 >= bundle.members.len() {
                    this.load_more(PagedView::Bundle, bundle.members.len(), cx);
                }
                let Some(list) = this.list() else { return div().into_any_element() };
                let Some(conversation) = bundle.members.get(row).and_then(|&ix| list.conversations.get(ix)) else {
                    return div().into_any_element();
                };
                let next = bundle.members.get(row + 1).and_then(|&ix| list.conversations.get(ix)).map(|c| &c.id);
                let rule = next.is_some_and(|next| this.bundle_highlight.as_ref() != Some(next));
                let selected = this.bundle_highlight.as_ref() == Some(&conversation.id);
                div().px(px(metrics::INSET)).child(this.render_row(conversation, selected, rule, p, cx)).into_any_element()
            }),
        )
        .size_full()
        .py(px(space::S6))
        .into_any_element()
    }
}

/// Recounts a list of same-height rows, keeping the row at its top.
fn keep_top(state: &ListState, count: usize) {
    let top = state.logical_scroll_top();
    state.reset(count);
    state.scroll_to(top);
}

/// A row's date, in the secondary label.
fn date(when: SharedString, p: &Palette) -> Div {
    div().flex_shrink_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(when)
}
