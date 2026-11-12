//! The islands' title rows: the list's (the mailbox and its count, Back in a bundle or a search,
//! refresh and compose on a capsule) and the conversation's toolbar (close or back, then its
//! actions on capsules floating over the message, which scrolls under them).

use super::*;

impl MailApp {
    pub(super) fn render_list_header(
        &self,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bundle = self.open_bundle_members();
        let (title, subtitle): (SharedString, Option<SharedString>) = match (&bundle, &self.query) {
            (Some(bundle), _) => {
                (bundle.category.title().into(), Some(format!("{} conversations", bundle.members.len()).into()))
            }
            (None, Some(query)) => ("Search".into(), Some(format!("“{query}”").into())),
            (None, None) => {
                let unread = self.list().map_or(0, |list| list.conversations.iter().filter(|c| c.unread).count());
                let account = match &self.source {
                    Source::Mailbox { account, .. } => {
                        self.accounts.iter().find(|a| &a.id == account).map(|a| a.title())
                    }
                    Source::Unified(_) => None,
                };
                let parts: Vec<SharedString> =
                    account.into_iter().chain((unread > 0).then(|| format!("{unread} unread").into())).collect();
                (self.source.title(), (!parts.is_empty()).then(|| parts.join(" · ").into()))
            }
        };
        let loading = self.list().is_some_and(|list| list.loading);
        let can_go_back = bundle.is_some() || self.query.is_some();
        title_bar("list-header", window, cx)
            .flex_shrink_0()
            .h(px(layout::HEADER_HEIGHT))
            .px(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S10))
            .when(can_go_back, |d| {
                d.child(capsule(p).child(icon_button("back", Icon::Back, "Back (Esc)", true, p.glyph).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.back(&Back, window, cx)),
                )))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(!can_go_back, |d| d.pl(px(space::S6)))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::PANE_TITLE))
                            .font_weight(FontWeight::BOLD)
                            .child(title),
                    )
                    .when_some(if loading { Some("Loading…".into()) } else { subtitle }, |d, subtitle: SharedString| {
                        d.child(div().truncate().text_size(px(text::CAPTION)).text_color(p.muted).child(subtitle))
                    }),
            )
            .child(
                capsule(p)
                    .child(
                        icon_button("refresh", Icon::Refresh, "Get New Mail", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
                    )
                    .child(icon_button("compose", Icon::Compose, "New Message", true, p.glyph).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.new_message(&NewMessage, window, cx)),
                    )),
            )
    }

    /// The conversation's island: the message scrolls under its toolbar.
    pub(super) fn render_detail(
        &self,
        conversation: &Conversation,
        two_pane: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        island(p.background, p)
            .id(ElementId::Name(format!("detail:{}", conversation.id).into()))
            .size_full()
            // The conversation keeps the keyboard; clicks never reach the list beneath.
            .occlude()
            .child(self.render_reader(conversation, p, cx))
            .child(self.render_detail_toolbar(conversation, two_pane, p, window, cx))
            .into_any_element()
    }

    /// Floats over the top of the conversation: ✕ closes it beside the list, ‹ goes back to the
    /// list it covers; then its actions.
    fn render_detail_toolbar(
        &self,
        conversation: &Conversation,
        two_pane: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let can_archive = conversation.can_archive();
        let restoring = !can_archive && conversation.can_restore();
        let unread = conversation.unread;
        let starred = conversation.starred;
        let (close_icon, close_tip) = if two_pane { (Icon::Close, "Close (Esc)") } else { (Icon::Back, "Back (Esc)") };
        title_bar("detail-toolbar", window, cx)
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(layout::HEADER_HEIGHT))
            .px(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(capsule(p).child(
                icon_button("close", close_icon, close_tip, true, p.glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(&Back, window, cx))),
            ))
            .child(div().flex_1())
            .child(
                capsule(p)
                    .when(conversation.waiting, |d| {
                        d.child(icon_button("resolve", Icon::Resolve, "Resolve (D)", true, p.waiting).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_waiting(&ToggleWaiting, window, cx)),
                        ))
                    })
                    .child(if restoring {
                        icon_button("restore", Icon::Restore, "Move to Inbox", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.restore(&Restore, window, cx)))
                    } else {
                        icon_button("archive", Icon::Archive, "Archive (E)", can_archive, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.archive(&Archive, window, cx)))
                    })
                    .child(
                        icon_button("trash", Icon::Delete, "Move to Trash", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.trash(&Trash, window, cx))),
                    )
                    .child(
                        icon_button(
                            "read",
                            if unread { Icon::MarkRead } else { Icon::MarkUnread },
                            if unread { "Mark as Read (U)" } else { "Mark as Unread (U)" },
                            true,
                            p.muted,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_read(&ToggleRead, window, cx))),
                    )
                    .child(
                        icon_button(
                            "star",
                            if starred { Icon::Starred } else { Icon::Star },
                            if starred { "Unstar (S)" } else { "Star (S)" },
                            true,
                            if starred { p.star } else { p.muted },
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.star(&Star, window, cx))),
                    ),
            )
            .child(
                capsule(p)
                    .child(
                        icon_button("reply", Icon::Reply, "Reply (R)", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.reply(&Reply, window, cx))),
                    )
                    .child(
                        icon_button("reply-all", Icon::ReplyAll, "Reply All (A)", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.reply_all(&ReplyAll, window, cx))),
                    )
                    .child(
                        icon_button("forward", Icon::Forward, "Forward (F)", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.forward(&Forward, window, cx))),
                    ),
            )
    }
}
