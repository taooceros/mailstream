//! The row's right-click menu (`ConversationListViewController.menu`): reply and forward, read and star toggles,
//! "Waiting on you", then filing. Escape or a click outside closes it.

use super::*;
use gpui::{anchored, deferred};

/// One entry: icon, title, what it runs.
struct Entry {
    glyph: Icon,
    title: &'static str,
    run: fn(&mut MailApp, &mut Window, &mut Context<MailApp>),
}

impl MailApp {
    pub(super) fn render_context_menu(&self, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let conversation = self.conversation(&menu.id)?;

        let mut groups: Vec<Vec<Entry>> = vec![vec![
            Entry { glyph: Icon::Reply, title: "Reply", run: |this, w, cx| this.reply(&Reply, w, cx) },
            Entry { glyph: Icon::ReplyAll, title: "Reply All", run: |this, w, cx| this.reply_all(&ReplyAll, w, cx) },
            Entry { glyph: Icon::Forward, title: "Forward", run: |this, w, cx| this.forward(&Forward, w, cx) },
        ]];
        groups.push(vec![
            if conversation.unread {
                Entry { glyph: Icon::MarkRead, title: "Mark as Read", run: |this, w, cx| this.toggle_read(&ToggleRead, w, cx) }
            } else {
                Entry { glyph: Icon::MarkUnread, title: "Mark as Unread", run: |this, w, cx| this.toggle_read(&ToggleRead, w, cx) }
            },
            if conversation.starred {
                Entry { glyph: Icon::Starred, title: "Unstar", run: |this, w, cx| this.star(&Star, w, cx) }
            } else {
                Entry { glyph: Icon::Star, title: "Star", run: |this, w, cx| this.star(&Star, w, cx) }
            },
        ]);
        if conversation.can_mark_waiting() {
            groups.push(vec![if conversation.waiting {
                Entry { glyph: Icon::Resolve, title: "Resolve", run: |this, w, cx| this.toggle_waiting(&ToggleWaiting, w, cx) }
            } else {
                Entry {
                    glyph: Icon::Waiting,
                    title: "Mark as Waiting on You",
                    run: |this, w, cx| this.toggle_waiting(&ToggleWaiting, w, cx),
                }
            }]);
        }
        let mut filing = Vec::new();
        if conversation.can_archive() {
            filing.push(Entry { glyph: Icon::Archive, title: "Archive", run: |this, w, cx| this.archive(&Archive, w, cx) });
        }
        if conversation.can_restore() {
            filing.push(Entry {
                glyph: Icon::Restore,
                title: "Move to Inbox",
                run: |this, w, cx| this.restore(&Restore, w, cx),
            });
        }
        filing.push(Entry { glyph: Icon::Delete, title: "Move to Trash", run: |this, w, cx| this.trash(&Trash, w, cx) });
        groups.push(filing);

        let count = groups.len();
        let mut body = div()
            .id("context-menu")
            .key_context("ContextMenu")
            .track_focus(&menu.focus)
            .on_action(cx.listener(Self::close_menu))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx)))
            .occlude()
            .w(px(layout::MENU_WIDTH))
            .py(px(4.))
            .flex()
            .flex_col()
            .rounded(px(radius::CARD))
            .bg(p.popover)
            .border_1()
            .border_color(p.line)
            .shadow(popover_shadow(p))
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .text_color(p.text);
        for (index, group) in groups.into_iter().enumerate() {
            for entry in group {
                let run = entry.run;
                body = body.child(
                    div()
                        .id(entry.title)
                        .mx(px(4.))
                        .h(px(layout::MENU_ROW))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(radius::SMALL))
                        .cursor_pointer()
                        .hover(|s| s.bg(p.selection))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| run(this, window, cx)))
                        .child(icon(entry.glyph, icon_size::SMALL, p.muted))
                        .child(entry.title),
                );
            }
            if index + 1 < count {
                body = body.child(div().my(px(4.)).h(px(1.)).bg(p.line));
            }
        }
        Some(deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(body)).into_any_element())
    }
}
