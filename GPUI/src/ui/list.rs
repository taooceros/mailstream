//! The conversation list and a bundle's list, on the list island's tray: Mail's two-line rows
//! lifting off it (`ConversationRow`, `BundleRow`), date groups, hover actions on a raised capsule,
//! the right-click target.

use super::*;
use crate::chat::UnreadSpace;
use crate::state::{ChatBundle, line_id};
use gpui::{ListState, MouseDownEvent, list};
use mailstream::Account;

impl MailApp {
    /// The list on screen (while a search is up, its results, on its layer), or with `beneath`
    /// the list under the search's layer (MS-343), each with its own lines and scroll.
    pub(super) fn render_list(&self, beneath: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let (items, scroll) = self.lines_drawn(beneath);
        if items.is_empty() {
            let note: SharedString = match self.list_drawn(beneath) {
                Some(list) if list.loading || list.fetching => "Loading…".into(),
                Some(List { error: Some(error), .. }) | Some(List { failures: Some(error), .. }) => error.clone(),
                None => "Loading…".into(),
                Some(_) if !beneath && self.query.is_some() => "No matches".into(),
                Some(_) => "No conversations".into(),
            };
            return centered_note(note, p).into_any_element();
        }
        list(
            scroll.clone(),
            cx.processor(move |this, ix: usize, _, cx| {
                // The list beneath only shows as the search slides off; it pages once on screen.
                if !beneath && ix + 20 >= this.items.len() {
                    this.load_more(PagedView::List, this.items.len(), cx);
                }
                if !beneath && this.query.is_some() {
                    this.row_drawn(ix, cx);
                }
                // The list's own padding stops at its top and bottom; rows step in by hand.
                div().px(px(metrics::INSET)).child(this.render_item(ix, beneath, p, cx)).into_any_element()
            }),
        )
        .size_full()
        .py(px(space::S6))
        .into_any_element()
    }

    /// The lines and scroll of the list on screen, or with `beneath` of the list under a search.
    fn lines_drawn(&self, beneath: bool) -> (&[Item], &ListState) {
        match self.beneath.as_ref().filter(|_| beneath) {
            Some(under) => (&under.items, &under.scroll),
            None => (&self.items, &self.list_scroll),
        }
    }

    /// The list whose conversations `lines_drawn(beneath)` index.
    fn list_drawn(&self, beneath: bool) -> Option<&List> {
        match self.beneath.as_ref().filter(|_| beneath) {
            Some(under) => self.lists.get(&under.key()),
            None => self.list(),
        }
    }

    /// Whether row `id` of the list `beneath` names shows as selected: the list under a search
    /// shows its highlight, the list on screen its selection or highlight.
    fn line_selected(&self, id: &str, beneath: bool) -> bool {
        match self.beneath.as_ref().filter(|_| beneath) {
            Some(under) => under.highlight.as_deref() == Some(id),
            None => self.row_selected(id, false),
        }
    }

    /// The accounts of the list on screen that can't connect (MS-278), on one line over its rows,
    /// which keep what the engine holds; a click tries every account again, its × hides it until
    /// the message changes (MS-322).
    pub(super) fn render_account_notice(&self, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let failing: Vec<(&Account, &SharedString)> = self
            .accounts
            .iter()
            .filter(|account| match &self.source {
                Source::Mailbox { account: shown, .. } => *shown == account.id,
                Source::Unified(_) => true,
            })
            .filter_map(|account| self.account_problems.get(&account.id).map(|problem| (account, problem)))
            .collect();
        let (account, problem) = failing.first()?;
        let message = match failing.len() {
            1 => format!("{} can't connect: {problem}", account.title()),
            count => format!("{count} accounts can't connect"),
        };
        if self.dismissed_account_notice.as_deref() == Some(message.as_str()) {
            return None;
        }
        let dismissed = message.clone();
        Some(
            div()
                .flex_shrink_0()
                .px(px(space::S12))
                .pb(px(space::S10))
                .child(
                    notice(Tone::Error, None, message, true, p)
                        .id("account-notice")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx)))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(text::CAPTION))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.accent)
                                .child("Try Again"),
                        )
                        .child(notice_close("account-notice-close", p).on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.dismissed_account_notice = Some(dismissed.clone());
                                cx.notify();
                            },
                        ))),
                )
                .into_any_element(),
        )
    }

    /// The id of the row after `ix` in `items`, lines of `list`, if a row follows.
    fn next_row(items: &[Item], list: &List, ix: usize) -> Option<String> {
        match items.get(ix + 1)? {
            Item::Section(_) | Item::Waiting(_) => None,
            item => Some(line_id(item, Some(list))),
        }
    }

    fn render_item(&self, ix: usize, beneath: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let (items, _) = self.lines_drawn(beneath);
        let (Some(list), Some(item)) = (self.list_drawn(beneath), items.get(ix)) else {
            return div().into_any_element();
        };
        let rule = Self::next_row(items, list, ix).is_some_and(|next| !self.line_selected(&next, beneath));
        match item {
            Item::Section(title) => section_header(title.clone(), None, p).into_any_element(),
            Item::Waiting(count) => section_header("Waiting on you", Some(*count), p).into_any_element(),
            Item::Row(index) => {
                let Some(conversation) = list.conversations.get(*index) else { return div().into_any_element() };
                let selected = self.line_selected(&conversation.id, beneath);
                self.render_row(conversation, selected, rule, !beneath, p, cx)
            }
            Item::Bundle(bundle) => {
                let newest = &list.conversations[bundle.members[0]];
                let id = format!("bundle:{}", bundle.category.as_str());
                let selected = self.line_selected(&id, beneath);
                let unread = bundle.unread > 0;
                let color = design::category_color(bundle.category, p);
                let count = if unread { format!("{} new", bundle.unread) } else { bundle.members.len().to_string() };
                let title = div()
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .child(div().font_weight(if unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD }).child(bundle.category.label()))
                    .child(tag(count, Some(color), p));
                let subtitle = div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.muted)
                    .child(format!("{}  —  {}", bundle.senders, bundle.subject));
                list_row(ElementId::Name(id.clone().into()), selected, rule, p)
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.click_row(&id, event.modifiers(), window, cx)
                    }))
                    .child(row_layout(
                        unread,
                        icon_avatar(Icon::for_category(bundle.category), color, metrics::AVATAR),
                        title,
                        date(newest.when.clone().into(), p),
                        subtitle,
                        Vec::new(),
                        p,
                    ))
                    .into_any_element()
            }
            Item::Chat(bundle) => {
                let selected = self.line_selected(&BundleKind::Chat.id(), beneath);
                self.render_chat_row(bundle, selected, rule, p, cx)
            }
        }
    }

    /// A conversation's row; `on_screen` (not the list under a search, MS-343) shows what is
    /// picked, hovered or right-clicked.
    fn render_row(
        &self,
        conversation: &Conversation,
        selected: bool,
        rule: bool,
        on_screen: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = conversation.id.clone();
        // Sent mail names who it went to.
        let senders = if self.source.role().shows_recipients() && conversation.peers.is_empty() {
            conversation
                .participants
                .iter()
                .find(|name| *name != "me")
                .map_or_else(|| conversation.sender_name(), |name| name.clone().into())
        } else {
            conversation.senders()
        };
        // A picked row (MS-305) shows a check where its avatar was.
        let picked = on_screen && self.selection().is_some_and(|selection| selection.contains(&conversation.id));
        let photo = if picked {
            icon_avatar(Icon::Check, p.accent, metrics::AVATAR).into_any_element()
        } else {
            avatars::avatar(&conversation.avatar_email(), &conversation.avatar, metrics::AVATAR, p, cx).into_any_element()
        };
        let hovered = on_screen && self.hovered_row.as_ref() == Some(&id);
        let menu_open = on_screen && self.context_menu.as_ref().is_some_and(|menu| menu.id == id);

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
            .child(date(conversation.when.clone().into(), p));

        let subject: SharedString =
            if conversation.title.is_empty() { "(no subject)".into() } else { conversation.title.clone().into() };
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
            marks.push(tag(SharedString::from(topic.clone()), Some(design::topic_color(topic, p.dark)), p).ml(px(space::S2)).into_any_element());
        }
        if conversation.starred {
            marks.push(icon(Icon::Starred, 13., p.star).into_any_element());
        }

        let hover_id = id.clone();
        let menu_id = id.clone();
        list_row(ElementId::Name(id.clone().into()), selected || menu_open, rule, p)
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
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&id, event.modifiers(), window, cx)))
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
                                this.set_waiting(&[c], false, cx);
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
                                this.remove(vec![c], Removal::Archive, window, cx);
                            } else if c.can_restore() {
                                this.remove(vec![c], Removal::Restore, window, cx);
                            }
                        },
                    )))
                })
                .child(icon_button("row-trash", Icon::Delete, "Move to Trash", true, p.glyph).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&trash) {
                            this.remove(vec![c], Removal::Trash, window, cx);
                        }
                    },
                )))
                .child(icon_button("row-read", read_icon, read_tip, true, p.glyph).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&read) {
                            let unread = !c.unread;
                            this.set_unread(&[c], unread, cx);
                        }
                    },
                )))
                .child(icon_button("row-star", star_icon, star_tip, true, star_color).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = this.conversation(&star) {
                            let starred = !c.starred;
                            this.set_starred(&[c], starred, cx);
                        }
                    },
                ))),
        )
    }

    // MARK: - Bundle

    /// An open bundle: its rows on a page of the tray sliding over the list's.
    pub(super) fn render_bundle(&self, category: Category, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(bundle) = self.bundle_members(category) else {
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
                let rule = next.is_some_and(|next| !this.row_selected(next, true));
                let selected = this.row_selected(&conversation.id, true);
                div().px(px(metrics::INSET)).child(this.render_row(conversation, selected, rule, true, p, cx)).into_any_element()
            }),
        )
        .size_full()
        .py(px(space::S6))
        .into_any_element()
    }

    /// The inbox's Chat bundle row (MS-330): its spaces with unread messages, named, and the
    /// newest one's last message.
    fn render_chat_row(
        &self,
        bundle: &ChatBundle,
        selected: bool,
        rule: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(newest) = bundle.spaces.first() else { return div().into_any_element() };
        let id = BundleKind::Chat.id();
        let color = design::chat_color();
        let title = div()
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(div().font_weight(FontWeight::BOLD).child("Chat"))
            .child(tag(format!("{} new", bundle.spaces.len()), Some(color), p));
        let subtitle = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(p.muted)
            .child(format!("{}  —  {}", bundle.titles, newest.snippet));
        list_row(ElementId::Name(id.clone().into()), selected, rule, p)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&id, event.modifiers(), window, cx)))
            .child(row_layout(
                true,
                icon_avatar(Icon::Chat, color, metrics::AVATAR),
                title,
                date(newest.when.clone(), p),
                subtitle,
                Vec::new(),
                p,
            ))
            .into_any_element()
    }

    /// The open Chat bundle: a row per space with unread messages, as Chat's list draws it; a
    /// click opens the space in Chat.
    pub(super) fn render_chat_bundle(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(bundle) = self.chat_bundle() else {
            return centered_note("Nothing here any more", p).into_any_element();
        };
        let count = bundle.spaces.len();
        if self.bundle_scroll.item_count() != count {
            keep_top(&self.bundle_scroll, count);
        }
        list(
            self.bundle_scroll.clone(),
            cx.processor(move |this, row: usize, _, cx| {
                let Some(space) = bundle.spaces.get(row) else { return div().into_any_element() };
                let rule = bundle.spaces.get(row + 1).is_some_and(|next| !this.row_selected(&next.row_id(), true));
                div().px(px(metrics::INSET)).child(this.render_chat_space(space, rule, p, cx)).into_any_element()
            }),
        )
        .size_full()
        .py(px(space::S6))
        .into_any_element()
    }

    fn render_chat_space(&self, space: &UnreadSpace, rule: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = space.row_id();
        let selected = self.row_selected(&id, true);
        let title = div().min_w_0().truncate().font_weight(FontWeight::BOLD).child(space.title.clone());
        let account_dot = (self.accounts.len() > 1)
            .then(|| self.accounts.iter().position(|a| a.id == space.account.as_ref()))
            .flatten()
            .map(|index| mark_dot(design::account_color(index)));
        let meta = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::S4))
            .children(account_dot.map(|dot| dot.mr(px(2.))))
            .child(div().text_size(px(text::FOOTNOTE)).text_color(p.accent).child(space.when.clone()));
        let subtitle = div().flex_1().min_w_0().truncate().text_color(p.muted).child(space.snippet.clone());
        list_row(ElementId::Name(id.clone().into()), selected, rule, p)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&id, event.modifiers(), window, cx)))
            .child(row_layout(
                true,
                avatars::avatar(&space.avatar_email, &space.avatar, metrics::AVATAR, p, cx),
                title,
                meta,
                subtitle,
                Vec::new(),
                p,
            ))
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
