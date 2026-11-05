//! The toolbar, in the native order: sidebar toggle, back, title; then the message actions
//! (enabled for the open or highlighted conversation); then refresh and search.

use super::*;

impl MailApp {
    pub(super) fn render_toolbar(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let layers = self.live_layers();
        let (title, count): (SharedString, Option<usize>) = match (layers.last(), &self.query) {
            (Some(Layer::Reader(_)), _) => ("".into(), None),
            (Some(Layer::Bundle(category)), _) => {
                (category.title().into(), self.open_bundle_members().map(|b| b.members.len()))
            }
            (None, Some(query)) => {
                (format!("“{query}”").into(), self.list().filter(|l| !l.loading).map(|l| l.conversations.len()))
            }
            (None, None) => (self.source.title(), None),
        };
        let back_to: Option<SharedString> = match layers.len() {
            0 if self.query.is_some() => Some(self.source.title()),
            0 => None,
            depth => Some(match layers.get(depth.wrapping_sub(2)) {
                Some(Layer::Bundle(category)) => category.title().into(),
                _ if self.query.is_some() => "Results".into(),
                _ => self.source.title(),
            }),
        };
        let target = self.target();
        let has = target.is_some();
        let can_archive = target.as_ref().is_some_and(Conversation::can_archive);
        let restoring = !can_archive && target.as_ref().is_some_and(Conversation::can_restore);
        let unread = target.as_ref().is_some_and(|c| c.unread);
        let starred = target.as_ref().is_some_and(|c| c.starred);

        title_bar("toolbar")
            .h(px(layout::TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(if self.sidebar_open { 10. } else { layout::TRAFFIC_LIGHTS }))
            .pr(px(14.))
            .border_b_1()
            .border_color(p.line)
            .when(!self.sidebar_open, |d| {
                d.child(icon_button("show-sidebar", Icon::Sidebar, "Show Sidebar", true, p.muted, p).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_sidebar(&ToggleSidebar, window, cx)),
                ))
            })
            .when_some(back_to, |d, back| {
                d.child(
                    div()
                        .id("back")
                        .flex_shrink_0()
                        .h(px(26.))
                        .pl(px(2.))
                        .pr(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(radius::CONTROL))
                        .text_size(px(text::FOOTNOTE))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.accent)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(&Back, window, cx)))
                        .child(icon(Icon::Back, icon_size::TOOLBAR, p.accent))
                        .child(back),
                )
            })
            .child(
                div()
                    .flex_shrink()
                    .min_w_0()
                    .truncate()
                    .ml(px(6.))
                    .text_size(px(text::HEADLINE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when_some(count, |d, count| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .ml(px(6.))
                        .text_color(p.muted)
                        .text_size(px(text::FOOTNOTE))
                        .child(count.to_string()),
                )
            })
            .child(div().flex_1())
            .child(if restoring {
                icon_button("restore", Icon::Restore, "Move to Inbox", true, p.muted, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.restore(&Restore, window, cx)))
            } else {
                icon_button("archive", Icon::Archive, "Archive", can_archive, p.muted, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.archive(&Archive, window, cx)))
            })
            .child(
                icon_button("trash", Icon::Delete, "Move to Trash", has, p.muted, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.trash(&Trash, window, cx))),
            )
            .child(
                icon_button(
                    "read",
                    if unread { Icon::MarkRead } else { Icon::MarkUnread },
                    if unread { "Mark as Read" } else { "Mark as Unread" },
                    has,
                    p.muted,
                    p,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_read(&ToggleRead, window, cx))),
            )
            .child(
                icon_button(
                    "star",
                    if starred { Icon::Starred } else { Icon::Star },
                    if starred { "Unstar" } else { "Star" },
                    has,
                    if starred { p.star } else { p.muted },
                    p,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.star(&Star, window, cx))),
            )
            .child(div().w(px(12.)))
            .child(
                icon_button("refresh", Icon::Refresh, "Get New Mail", true, p.muted, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
            )
            .child(
                div()
                    .id("search")
                    .ml(px(6.))
                    .flex_shrink_0()
                    .w(px(220.))
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(radius::CONTROL))
                    .bg(p.chip)
                    .line_height(px(18.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(icon(Icon::Search, icon_size::SMALL, p.muted))
                    .child(self.search.clone()),
            )
    }
}
