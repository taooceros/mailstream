//! The islands' title rows: the list's (the mailbox and its count, Back in a bundle or a search,
//! refresh and compose on a capsule) and the conversation's toolbar (close or back, then its
//! actions on capsules floating over the message, which scrolls under them).

use crate::controls::{list_header, page_scrim, page_toolbar};

use super::*;
use crate::state::ReaderTop;

impl MailApp {
    pub(super) fn render_list_header(
        &self,
        compact: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.selection().is_some() {
            return self.render_selection_bar(compact, p, window, cx).into_any_element();
        }
        if compact && (self.compact_search || self.query.is_some()) {
            return self.render_compact_search_bar(p, window, cx);
        }
        // The open bundle's title and count, while it is still in the list.
        let bundle: Option<(SharedString, SharedString)> = match self.open_bundle() {
            Some(BundleKind::Mail(_)) => self
                .open_bundle_members()
                .map(|bundle| (bundle.category.label().into(), format!("{} conversations", bundle.members.len()).into())),
            Some(BundleKind::Chat) => self.chat_bundle().map(|chat| ("Chat".into(), format!("{} unread", chat.spaces.len()).into())),
            None => None,
        };
        let (title, subtitle): (SharedString, Option<SharedString>) = match (&bundle, &self.query) {
            (Some((title, subtitle)), _) => (title.clone(), Some(subtitle.clone())),
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
        let loading = self.list().is_some_and(|list| list.loading || list.fetching);
        let subtitle = if loading { Some("Loading…".into()) } else { subtitle };
        if compact {
            let ts = design::type_scale(true);
            let leading = if bundle.is_some() || self.query.is_some() {
                compact_icon_button("back", Icon::Back, "Back (Esc)", true, p.glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.list_back(window, cx)))
            } else {
                compact_icon_button("compact-menu", Icon::Menu, "Mailboxes", true, p.glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_drawer(cx)))
            };
            return div()
                .id("list-header")
                .flex_shrink_0()
                .h(px(design::compact::TOP_BAR_HEIGHT))
                .px(px(space::S12))
                .flex()
                .items_center()
                .gap(px(space::S10))
                .child(compact_capsule(p).child(leading))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(ts.header_title))
                                .line_height(relative(1.15))
                                .font_weight(FontWeight::BOLD)
                                .child(title),
                        )
                        .when_some(subtitle, |d, subtitle| {
                            d.child(div().truncate().text_size(px(ts.caption)).text_color(p.muted).child(subtitle))
                        }),
                )
                .child(
                    compact_capsule(p)
                        .child(
                            compact_icon_button("compact-search-open", Icon::Search, "Search", true, p.glyph)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_compact_search(window, cx))),
                        )
                        .child(
                            compact_icon_button("refresh", Icon::Refresh, "Get New Mail", true, p.glyph)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
                        ),
                )
                .into_any_element();
        }
        let back = (bundle.is_some() || self.query.is_some()).then(|| {
            icon_button("back", Icon::Back, "Back (Esc)", true, p.glyph)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.list_back(window, cx)))
        });
        let actions = capsule(p)
            .child(
                icon_button("refresh", Icon::Refresh, "Get New Mail", true, p.glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
            )
            .child(icon_button("compose", Icon::Compose, "New Message", true, p.glyph).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.new_message(&NewMessage, window, cx)),
            ));
        list_header("list-header", back, title, subtitle, actions, p, window, cx).into_any_element()
    }

    /// Expanded inline search header for compact mode: `[‹] [Search input…]`, saving a dedicated
    /// search row while browsing the inbox.
    fn render_compact_search_bar(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.search.focus_handle(cx).is_focused(window);
        let ts = design::type_scale(true);
        div()
            .id("compact-search-header")
            .flex_shrink_0()
            .h(px(design::compact::TOP_BAR_HEIGHT))
            .px(px(space::S12))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(
                compact_capsule(p).child(
                    compact_icon_button("compact-search-back", Icon::Back, "Close Search", true, p.glyph).on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            this.cancel_search(&gpui::component::input::Escape, window, cx);
                        },
                    )),
                ),
            )
            .child(
                crate::controls::field_frame(focused, true, p)
                    .id("search")
                    .flex_1()
                    .min_w_0()
                    .h(px(design::compact::SEARCH_HEIGHT))
                    .pl(px(space::S12))
                    .pr(px(space::S10))
                    .gap(px(space::S8))
                    .text_size(px(ts.body))
                    .line_height(px((ts.body * 1.35).round()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_action(cx.listener(Self::cancel_search))
                    .child(icon(Icon::Search, 17., p.muted))
                    .child(div().flex_1().min_w_0().child(input::field(&self.search))),
            )
            .into_any_element()
    }

    /// The list's title row while conversations are picked (MS-305): ✕ ends the selection, then
    /// how many are picked and the actions that apply to all of them, each one call to the
    /// engine and one undo step.
    fn render_selection_bar(&self, compact: bool, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = self.selected_conversations();
        let any = |test: fn(&Conversation) -> bool| picked.iter().any(test);
        let (read_icon, read_tip) = if any(|c| c.unread) { (Icon::MarkRead, "Mark as Read (U)") } else { (Icon::MarkUnread, "Mark as Unread (U)") };
        let (star_icon, star_tip, star_color) =
            if picked.iter().all(|c| c.starred) { (Icon::Starred, "Unstar (S)", p.star) } else { (Icon::Star, "Star (S)", p.glyph) };
        let waiting = picked.iter().filter(|c| c.can_mark_waiting()).map(|c| c.waiting).reduce(|a, b| a || b);
        let filing = if any(Conversation::can_archive) {
            Some(("selection-archive", Icon::Archive, "Archive (E)", Removal::Archive))
        } else if any(Conversation::can_restore) {
            Some(("selection-restore", Icon::Restore, "Move to Inbox", Removal::Restore))
        } else {
            None
        };
        let ts = design::type_scale(compact);
        let bar_capsule = |p: &'static Palette| if compact { compact_capsule(p) } else { capsule(p) };
        let bar_icon = |id: &'static str, glyph: Icon, tip: &'static str, enabled: bool, color: gpui::Hsla| {
            if compact { compact_icon_button(id, glyph, tip, enabled, color) } else { icon_button(id, glyph, tip, enabled, color) }
        };
        title_bar("list-header", window, cx)
            .flex_shrink_0()
            .h(px(if compact { design::compact::TOP_BAR_HEIGHT } else { layout::HEADER_HEIGHT }))
            .px(px(if compact { space::S12 } else { space::S10 }))
            .flex()
            .items_center()
            .gap(px(if compact { space::S8 } else { space::S10 }))
            .child(bar_capsule(p).child(bar_icon("selection-clear", Icon::Close, "Clear Selection (Esc)", true, p.glyph).on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.clear_selection(cx);
                }),
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ts.pane_title))
                    .font_weight(FontWeight::BOLD)
                    .child(format!("{} selected", picked.len())),
            )
            .child(
                bar_capsule(p)
                    .when_some(filing, |d, (id, glyph, tip, removal)| {
                        d.child(bar_icon(id, glyph, tip, true, p.glyph).on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| match removal {
                                Removal::Restore => this.restore(&Restore, window, cx),
                                _ => this.archive(&Archive, window, cx),
                            },
                        )))
                    })
                    .child(
                        bar_icon(
                            "selection-trash",
                            Icon::Delete,
                            if cfg!(target_os = "macos") { "Move to Trash (⌘⌫)" } else { "Move to Trash (Ctrl+Backspace)" },
                            true,
                            p.glyph,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.trash(&Trash, window, cx))),
                    )
                    .child(bar_icon("selection-read", read_icon, read_tip, true, p.glyph).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_read(&ToggleRead, window, cx)),
                    ))
                    .child(bar_icon("selection-star", star_icon, star_tip, true, star_color).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.star(&Star, window, cx)),
                    ))
                    .when_some(waiting, |d, waiting| {
                        let (glyph, tip, color) = if waiting {
                            (Icon::Resolve, "Resolve (D)", p.waiting)
                        } else {
                            (Icon::Waiting, "Mark as Waiting on You (D)", p.glyph)
                        };
                        d.child(bar_icon("selection-waiting", glyph, tip, true, color).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_waiting(&ToggleWaiting, window, cx)),
                        ))
                    }),
            )
    }

    /// The conversation's island: the message scrolls under its toolbar, behind a scrim of the
    /// page's surface once anything has (MS-255). A press on it outside what takes presses of its
    /// own (the bodies, the reply box, buttons) may drag the layer `layer` off (MS-266).
    pub(super) fn render_detail(
        &self,
        conversation: &Conversation,
        layer: SharedString,
        two_pane: bool,
        compact: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reply_open = self.reply.as_ref().is_some_and(|r| r.conversation == conversation.id);
        if compact {
            return div()
                .id(ElementId::Name(format!("detail:{}", conversation.id).into()))
                .relative()
                .overflow_hidden()
                .bg(p.background)
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, Self::press_layer_listener(layer.clone(), cx))
                .child(self.render_reader(conversation, true, p, window, cx))
                .when(self.reader_top != ReaderTop::Top, |d| d.child(page_scrim(p)))
                .child(self.render_compact_detail_toolbar(conversation, layer, p, cx))
                .when(!reply_open, |d| d.child(self.render_compact_reply_bar(p, cx)))
                .into_any_element();
        }
        island(p.background, p)
            .id(ElementId::Name(format!("detail:{}", conversation.id).into()))
            .size_full()
            // The conversation keeps the keyboard; clicks never reach the list beneath.
            .occlude()
            .on_mouse_down(MouseButton::Left, Self::press_layer_listener(layer.clone(), cx))
            .child(self.render_reader(conversation, false, p, window, cx))
            .when(self.reader_top != ReaderTop::Top, |d| d.child(page_scrim(p)))
            .child(self.render_detail_toolbar(conversation, layer, two_pane, p, window, cx))
            .into_any_element()
    }

    /// Compact reader top app bar (MS-408): Back + scrolled title + triage actions only. Reply,
    /// Reply All and Forward live in the sticky thumb-zone bottom bar so they never crowd or
    /// overlap the message header on narrow phone screens.
    fn render_compact_detail_toolbar(
        &self,
        conversation: &Conversation,
        layer: SharedString,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let can_archive = conversation.can_archive();
        let restoring = !can_archive && conversation.can_restore();
        let unread = conversation.unread;
        let starred = conversation.starred;
        let ts = design::type_scale(true);
        let title = (self.reader_top == ReaderTop::PastTitle).then(|| SharedString::from(conversation.title.clone()));
        div()
            .id("detail-toolbar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(design::compact::TOP_BAR_HEIGHT))
            .px(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .on_mouse_down(MouseButton::Left, Self::press_layer_listener(layer, cx))
            .child(
                compact_capsule(p).child(
                    compact_icon_button("close", Icon::Back, "Back (Esc)", true, p.glyph)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(&Back, window, cx))),
                ),
            )
            .child(div().flex_1().min_w_0().flex().justify_center().when_some(title, |d, title| {
                d.child(div().min_w_0().truncate().text_size(px(ts.body)).font_weight(FontWeight::SEMIBOLD).child(title))
            }))
            .child(
                compact_capsule(p)
                    .when(conversation.waiting, |d| {
                        d.child(
                            compact_icon_button("resolve", Icon::Resolve, "Resolve (D)", true, p.waiting).on_click(cx.listener(
                                |this, _: &ClickEvent, window, cx| this.toggle_waiting(&ToggleWaiting, window, cx),
                            )),
                        )
                    })
                    .child(if restoring {
                        compact_icon_button("restore", Icon::Restore, "Move to Inbox", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.restore(&Restore, window, cx)))
                    } else {
                        compact_icon_button("archive", Icon::Archive, "Archive (E)", can_archive, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.archive(&Archive, window, cx)))
                    })
                    .child(
                        compact_icon_button("trash", Icon::Delete, "Move to Trash", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.trash(&Trash, window, cx))),
                    )
                    .child(
                        compact_icon_button(
                            "read",
                            if unread { Icon::MarkRead } else { Icon::MarkUnread },
                            if unread { "Mark as Read (U)" } else { "Mark as Unread (U)" },
                            true,
                            p.glyph,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_read(&ToggleRead, window, cx))),
                    )
                    .child(
                        compact_icon_button(
                            "star",
                            if starred { Icon::Starred } else { Icon::Star },
                            if starred { "Unstar (S)" } else { "Star (S)" },
                            true,
                            if starred { p.star } else { p.glyph },
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.star(&Star, window, cx))),
                    ),
            )
    }

    /// Floating glass reply capsule at the bottom of the compact reader (MS-408, MS-417): echoes
    /// desktop's floating glass capsules over a soft bottom fade scrim.
    fn render_compact_reply_bar(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let label_size = 14.5 * design::compact::chrome_font_scale();
        let action_btn = |id: &'static str, glyph: Icon, label: &'static str, primary: bool| {
            let fg = if primary { p.on_accent } else { p.text };
            let glyph_color = if primary { p.on_accent } else { p.glyph };
            div()
                .id(id)
                .flex_1()
                .h_full()
                .px(px(space::S10))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(space::S6))
                .rounded_full()
                .cursor_pointer()
                .when(primary, |d| d.bg(p.accent).active(|s| s.opacity(0.86)))
                .when(!primary, |d| d.hover(|s| s.bg(p.hover)).active(|s| s.bg(p.pressed)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(icon(glyph, 17., glyph_color))
                .child(
                    div()
                        .text_size(px(label_size))
                        .font_weight(if primary { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                        .text_color(fg)
                        .child(label),
                )
        };
        div()
            .id("compact-reply-bar")
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .child(bottom_scrim(p.background, design::compact::REPLY_BAR_HEIGHT + space::S32))
            .child(
                div()
                    .relative()
                    .w_full()
                    .px(px(design::compact::DOCK_MARGIN_X))
                    .pb(px(design::compact::DOCK_MARGIN_B))
                    .child(
                        div()
                            .w_full()
                            .h(px(design::compact::REPLY_BAR_HEIGHT))
                            .p(px(design::compact::DOCK_PADDING))
                            .rounded_full()
                            .bg(gpui::linear_gradient(
                                180.,
                                gpui::linear_color_stop(p.capsule_glass[0], 0.),
                                gpui::linear_color_stop(p.capsule_glass[1], 1.),
                            ))
                            .backdrop_blur(px(12.))
                            .border(px(stroke::HAIRLINE))
                            .border_color(p.capsule_rim)
                            .shadow(p.capsule_shadow.clone())
                            .occlude()
                            .flex()
                            .items_center()
                            .gap(px(space::S4))
                            .child(
                                action_btn("reply", Icon::Reply, "Reply", true)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.reply(&Reply, window, cx))),
                            )
                            .child(
                                action_btn("reply-all", Icon::ReplyAll, "Reply All", false)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.reply_all(&ReplyAll, window, cx))),
                            )
                            .child(
                                action_btn("forward", Icon::Forward, "Forward", false)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.forward(&Forward, window, cx))),
                            ),
                    ),
            )
    }

    /// Floats over the top of the conversation: ✕ closes it beside the list, ‹ goes back to the
    /// list it covers; then its actions. A press on it may drag the layer `layer` off (MS-266):
    /// it hears the press before the rows scrolled under it, which may keep theirs.
    fn render_detail_toolbar(
        &self,
        conversation: &Conversation,
        layer: SharedString,
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
        let close = icon_button("close", close_icon, close_tip, true, p.glyph)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(&Back, window, cx)));
        // Once the title has scrolled away, the subject in one line between the capsules.
        let title = (self.reader_top == ReaderTop::PastTitle).then(|| SharedString::from(conversation.title.clone()));
        page_toolbar("detail-toolbar", close, title, p, window, cx)
            .on_mouse_down(MouseButton::Left, Self::press_layer_listener(layer, cx))
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
