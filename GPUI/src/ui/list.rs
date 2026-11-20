//! The conversation list and a bundle's list, on the list island's tray: Mail's two-line rows
//! lifting off it (`ConversationRow`, `BundleRow`), date groups, hover actions on a raised capsule,
//! the right-click target.

use super::*;
use crate::chat::UnreadSpace;
use crate::state::{ChatBundle, MORE_AHEAD, line_id};
use gpui::{ListState, MouseDownEvent, list};
use mailstream::Account;

impl MailApp {
    /// The list on screen (while a search is up, its results, on its layer), or with `beneath`
    /// the list under the search's layer (MS-343), each with its own lines and scroll.
    pub(super) fn render_list(&self, beneath: bool, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
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
                let shown = this.view_item_count(PagedView::List);
                if !beneath && ix + MORE_AHEAD >= shown {
                    this.load_more(PagedView::List, shown, false, cx);
                }
                if !beneath && this.query.is_some() {
                    this.row_drawn(ix, cx);
                }
                let row = this.render_item(ix, beneath, compact, p, cx);
                // Both desktop and compact step rows in slightly so lifted cards and hairlines breathe.
                div().px(px(metrics::INSET)).child(row).into_any_element()
            }),
        )
        .size_full()
        .pt(px(if compact { space::S4 } else { space::S6 }))
        .pb(px(if compact { design::compact::DOCK_CLEARANCE } else { space::S6 }))
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
            Item::Section(_) | Item::Waiting(_) | Item::More => None,
            item => Some(line_id(item, Some(list))),
        }
    }

    fn render_item(&self, ix: usize, beneath: bool, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let (items, _) = self.lines_drawn(beneath);
        let (Some(list), Some(item)) = (self.list_drawn(beneath), items.get(ix)) else {
            return div().into_any_element();
        };
        let rule = Self::next_row(items, list, ix).is_some_and(|next| !self.line_selected(&next, beneath));
        match item {
            Item::Section(title) => section_header_for(title.clone(), None, compact, p).into_any_element(),
            Item::Waiting(count) => section_header_for("Waiting on you", Some(*count), compact, p).into_any_element(),
            Item::Row(index) => {
                let Some(conversation) = list.conversations.get(*index) else { return div().into_any_element() };
                let selected = self.line_selected(&conversation.id, beneath);
                if compact {
                    self.render_compact_row(conversation, selected, rule, !beneath, p, cx)
                } else {
                    self.render_row(conversation, selected, rule, !beneath, p, cx)
                }
            }
            Item::Bundle(bundle) => {
                let newest = &list.conversations[bundle.members[0]];
                let id = format!("bundle:{}", bundle.category.as_str());
                let selected = self.line_selected(&id, beneath);
                let unread = bundle.unread > 0;
                let color = design::category_color(bundle.category, p);
                let count = if unread { format!("{} new", bundle.unread) } else { bundle.members.len().to_string() };
                let ts = design::type_scale(compact);
                let title = div()
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .child(
                        div()
                            .when(compact, |d| d.text_size(px(ts.reading)))
                            .font_weight(if unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
                            .child(bundle.category.label()),
                    )
                    .child(tag_for(count, Some(color), compact, p));
                let subtitle = div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.muted)
                    .child(format!("{}  —  {}", bundle.senders, bundle.subject));
                if compact {
                    return div()
                        .id(ElementId::Name(id.clone().into()))
                        .relative()
                        .flex_shrink_0()
                        .h(px(design::compact::bundle_row_height()))
                        .w_full()
                        .pr(px(metrics::TRAILING_INSET))
                        .flex()
                        .items_center()
                        .rounded(px(radius::CARD))
                        .cursor_pointer()
                        .active(|s| s.bg(p.pressed))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_row(&id, event.modifiers(), window, cx)
                        }))
                        .child(unread_dot(unread, p))
                        .child(icon_avatar(Icon::for_category(bundle.category), color, design::compact::AVATAR))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ml(px(metrics::AVATAR_GAP))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(space::S8))
                                        .child(div().flex_1().min_w_0().child(title))
                                        .child(date_for(newest.when.clone().into(), true, p)),
                                )
                                .child(div().text_size(px(ts.footnote)).child(subtitle)),
                        )
                        .when(rule, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .left(px(design::compact::SEPARATOR_X))
                                    .right(px(space::S8))
                                    .bottom_0()
                                    .h(px(stroke::HAIRLINE))
                                    .bg(p.line),
                            )
                        })
                        .into_any_element();
                }
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
                self.render_chat_row(bundle, selected, rule, compact, p, cx)
            }
            Item::More => {
                let shown = self.view_item_count(PagedView::List);
                self.render_more_footer(PagedView::List, shown, list, compact, p, cx)
            }
        }
    }

    /// Compact 3-line touch row aligned with desktop Mailstream's visual grammar (MS-408, MS-417):
    /// clean leading `unread_dot` column, 36px avatar (tap toggles multi-select), calm muted dates,
    /// gold star only when starred, lifted card highlight when selected, and desktop hairline inset.
    fn render_compact_row(
        &self,
        conversation: &Conversation,
        selected: bool,
        rule: bool,
        on_screen: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = conversation.id.clone();
        let senders = if self.source.role().shows_recipients() && conversation.peers.is_empty() {
            conversation
                .participants
                .iter()
                .find(|name| *name != "me")
                .map_or_else(|| conversation.sender_name(), |name| name.clone().into())
        } else {
            conversation.senders()
        };
        let picked = on_screen && self.selection().is_some_and(|selection| selection.contains(&conversation.id));
        let select_id = id.clone();
        let photo: AnyElement = if picked {
            icon_avatar(Icon::Check, p.accent, design::compact::AVATAR).into_any_element()
        } else {
            avatars::avatar(&conversation.avatar_email(), &conversation.avatar, design::compact::AVATAR, p, cx).into_any_element()
        };
        let avatar_hit = div()
            .id(ElementId::Name(format!("avatar:{id}").into()))
            .flex_shrink_0()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.toggle_row_selection(&select_id, window, cx);
            }))
            .child(photo);

        let account_dot = (self.accounts.len() > 1)
            .then(|| self.accounts.iter().position(|a| a.id == conversation.account))
            .flatten()
            .map(|index| mark_dot(design::account_color(index)));

        let ts = design::type_scale(true);
        let line1 = div()
            .flex()
            .items_center()
            .gap(px(space::S6))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ts.reading))
                            .font_weight(if conversation.unread { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
                            .text_color(p.text)
                            .child(senders),
                    )
                    .when(conversation.message_count > 1, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .ml(px(5.))
                                .text_size(px(ts.caption))
                                .text_color(p.muted)
                                .child(conversation.message_count.to_string()),
                        )
                    })
                    .when(conversation.waiting, |d| {
                        d.child(div().ml(px(space::S6)).child(icon(Icon::Reply, 14., p.waiting)))
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(space::S4))
                    .when(conversation.has_attachment, |d| d.child(icon(Icon::Attachment, 14., p.muted)))
                    .children(account_dot.map(|dot| dot.mr(px(2.))))
                    .child(date_for(conversation.when.clone().into(), true, p)),
            );

        let subject: SharedString =
            if conversation.title.is_empty() { "(no subject)".into() } else { conversation.title.clone().into() };
        let line2 = div()
            .flex()
            .items_center()
            .gap(px(space::S6))
            .when(!conversation.tag.is_empty(), |d| {
                d.child(div().flex_shrink_0().text_size(px(ts.body)).child(conversation.tag.clone()))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ts.body))
                    .font_weight(if conversation.unread { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                    .text_color(if conversation.unread { p.text } else { p.glyph })
                    .child(subject),
            )
            .when(conversation.starred, |d| d.child(icon(Icon::Starred, 15., p.star)));

        let snippet_text: SharedString = conversation.snippet.as_deref().unwrap_or("").to_owned().into();
        let topic = conversation.labels.first().map(|label| {
            tag_for(SharedString::from(label.clone()), Some(design::topic_color(label, p.dark)), true, p)
        });
        let line3 = div()
            .flex()
            .items_center()
            .gap(px(space::S6))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ts.footnote))
                    .text_color(p.muted)
                    .child(snippet_text),
            )
            .children(topic);

        let click_id = id.clone();
        let lifted = picked || selected;
        div()
            .id(ElementId::Name(id.into()))
            .relative()
            .flex_shrink_0()
            .h(px(design::compact::row_height()))
            .w_full()
            .cursor_pointer()
            .when(lifted, |d| d.child(lifted_card(true, p)))
            .when(!lifted, |d| d.rounded(px(radius::CARD)).active(|s| s.bg(p.pressed)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if this.selection().is_some() {
                    this.toggle_row_selection(&click_id, window, cx);
                } else {
                    this.click_row(&click_id, event.modifiers(), window, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .size_full()
                    .pr(px(metrics::TRAILING_INSET))
                    .flex()
                    .items_center()
                    .child(unread_dot(conversation.unread && !picked, p))
                    .child(avatar_hit)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ml(px(metrics::AVATAR_GAP))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(line1)
                            .child(line2)
                            .child(line3),
                    ),
            )
            .when(rule && !lifted, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(design::compact::SEPARATOR_X))
                        .right(px(space::S8))
                        .bottom_0()
                        .h(px(stroke::HAIRLINE))
                        .bg(p.line),
                )
            })
            .into_any_element()
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
    pub(super) fn render_bundle(&self, category: Category, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(bundle) = self.bundle_members(category) else {
            return centered_note("Nothing here any more", p).into_any_element();
        };
        let has_more = self.list().is_some_and(|list| list.next.is_some());
        let count = bundle.members.len() + usize::from(has_more);
        if self.bundle_scroll.item_count() != count {
            keep_top(&self.bundle_scroll, count);
        }
        list(
            self.bundle_scroll.clone(),
            cx.processor(move |this, row: usize, _, cx| {
                if row + MORE_AHEAD >= bundle.members.len() {
                    this.load_more(PagedView::Bundle, bundle.members.len(), false, cx);
                }
                let Some(list) = this.list() else { return div().into_any_element() };
                if row == bundle.members.len() {
                    return div()
                        .px(px(metrics::INSET))
                        .child(this.render_more_footer(PagedView::Bundle, bundle.members.len(), list, compact, p, cx))
                        .into_any_element();
                }
                let Some(conversation) = bundle.members.get(row).and_then(|&ix| list.conversations.get(ix)) else {
                    return div().into_any_element();
                };
                let next = bundle.members.get(row + 1).and_then(|&ix| list.conversations.get(ix)).map(|c| &c.id);
                let rule = next.is_some_and(|next| !this.row_selected(next, true));
                let selected = this.row_selected(&conversation.id, true);
                let row = if compact {
                    this.render_compact_row(conversation, selected, rule, true, p, cx)
                } else {
                    this.render_row(conversation, selected, rule, true, p, cx)
                };
                div().px(px(metrics::INSET)).child(row).into_any_element()
            }),
        )
        .size_full()
        .pt(px(if compact { space::S4 } else { space::S6 }))
        .pb(px(if compact { design::compact::DOCK_CLEARANCE } else { space::S6 }))
        .into_any_element()
    }

    /// Bottom footer while older mail can still be loaded: shows progress while fetching, or an
    /// explicit "Load older mail" button (with any error note) when idle or paused.
    fn render_more_footer(
        &self,
        view: PagedView,
        shown: usize,
        list: &List,
        compact: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ts = design::type_scale(compact);
        let id = match view {
            PagedView::List => "list-load-more",
            PagedView::Bundle => "bundle-load-more",
        };
        if list.loading_more {
            return div()
                .w_full()
                .py(px(space::S12))
                .flex()
                .justify_center()
                .text_size(px(ts.caption))
                .text_color(p.muted)
                .child("Loading older mail…")
                .into_any_element();
        }
        let kind = if list.more_error.is_some() { ButtonKind::Secondary } else { ButtonKind::Plain(p.accent) };
        div()
            .w_full()
            .py(px(space::S10))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::S6))
            .when_some(list.more_error.clone(), |d, error| {
                d.child(
                    div()
                        .px(px(space::S12))
                        .text_center()
                        .text_size(px(ts.caption))
                        .text_color(p.muted)
                        .child(error),
                )
            })
            .child(button(id, "Load older mail", kind, ControlSize::small(compact), true, p).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    this.load_more(view, shown, true, cx);
                },
            )))
            .into_any_element()
    }

    /// The inbox's Chat bundle row (MS-330): its spaces with unread messages, named, and the
    /// newest one's last message.
    fn render_chat_row(
        &self,
        bundle: &ChatBundle,
        selected: bool,
        rule: bool,
        compact: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(newest) = bundle.spaces.first() else { return div().into_any_element() };
        let id = BundleKind::Chat.id();
        let color = design::chat_color();
        let ts = design::type_scale(compact);
        let title = div()
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(div().when(compact, |d| d.text_size(px(ts.reading))).font_weight(FontWeight::BOLD).child("Chat"))
            .child(tag_for(format!("{} new", bundle.spaces.len()), Some(color), compact, p));
        let subtitle = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(p.muted)
            .child(format!("{}  —  {}", bundle.titles, newest.snippet));
        if compact {
            return div()
                .id(ElementId::Name(id.clone().into()))
                .relative()
                .flex_shrink_0()
                .h(px(design::compact::bundle_row_height()))
                .w_full()
                .pr(px(metrics::TRAILING_INSET))
                .flex()
                .items_center()
                .rounded(px(radius::CARD))
                .cursor_pointer()
                .active(|s| s.bg(p.pressed))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.click_row(&id, event.modifiers(), window, cx)
                }))
                .child(unread_dot(true, p))
                .child(icon_avatar(Icon::Chat, color, design::compact::AVATAR))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .ml(px(metrics::AVATAR_GAP))
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(space::S8))
                                .child(div().flex_1().min_w_0().child(title))
                                .child(date_for(newest.when.clone(), true, p)),
                        )
                        .child(div().text_size(px(ts.footnote)).child(subtitle)),
                )
                .when(rule, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(design::compact::SEPARATOR_X))
                            .right(px(space::S8))
                            .bottom_0()
                            .h(px(stroke::HAIRLINE))
                            .bg(p.line),
                    )
                })
                .into_any_element();
        }
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
    pub(super) fn render_chat_bundle(&self, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
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
                div().px(px(metrics::INSET)).child(this.render_chat_space(space, rule, compact, p, cx)).into_any_element()
            }),
        )
        .size_full()
        .pt(px(space::S6))
        .pb(px(if compact { design::compact::DOCK_CLEARANCE } else { space::S6 }))
        .into_any_element()
    }

    fn render_chat_space(&self, space: &UnreadSpace, rule: bool, compact: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = space.row_id();
        let selected = self.row_selected(&id, true);
        let ts = design::type_scale(compact);
        let title = div()
            .min_w_0()
            .truncate()
            .when(compact, |d| d.text_size(px(ts.reading)))
            .font_weight(FontWeight::BOLD)
            .child(space.title.clone());
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
            .child(div().text_size(px(if compact { ts.caption } else { text::FOOTNOTE })).text_color(p.accent).child(space.when.clone()));
        let subtitle = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .when(compact, |d| d.text_size(px(ts.footnote)))
            .text_color(p.muted)
            .child(space.snippet.clone());
        list_row(ElementId::Name(id.clone().into()), selected, rule, p)
            .when(compact, |d| d.h(px(design::compact::bundle_row_height())))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&id, event.modifiers(), window, cx)))
            .child(row_layout(
                true,
                avatars::avatar(&space.avatar_email, &space.avatar, if compact { design::compact::AVATAR } else { metrics::AVATAR }, p, cx),
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
    date_for(when, false, p)
}

fn date_for(when: SharedString, compact: bool, p: &Palette) -> Div {
    let ts = design::type_scale(compact);
    div().flex_shrink_0().text_size(px(if compact { ts.caption } else { text::FOOTNOTE })).text_color(p.muted).child(when)
}
