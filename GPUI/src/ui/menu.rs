//! The row's right-click menu (`ConversationListViewController.menu`): reply and forward, read and star toggles,
//! "Waiting on you", then filing. It is gpui-kit's `PopupMenu` (MS-226), built as it opens, its entries drawn
//! in the app's row (the glyph in the secondary label before the title, in the body text); the kit
//! steps through them with the arrows, runs one with Return and closes on Escape or a press outside.

use super::*;
use gpui::component::menu::{PopupMenu, PopupMenuItem};
use gpui::{Entity, anchored, deferred};

/// One entry: icon, title, what it runs.
struct Entry {
    glyph: Icon,
    title: &'static str,
    run: fn(&mut MailApp, &mut Window, &mut Context<MailApp>),
}

impl MailApp {
    /// The menu for the row `id`, or none once its conversation is gone.
    pub(crate) fn build_context_menu(
        &self,
        id: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<PopupMenu>> {
        let conversation = self.conversation(id)?;

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

        let app = cx.weak_entity();
        Some(PopupMenu::build(window, cx, move |mut menu, _, _| {
            menu = menu.min_w(px(layout::MENU_WIDTH));
            for (index, group) in groups.into_iter().enumerate() {
                if index > 0 {
                    menu = menu.separator();
                }
                for Entry { glyph, title, run } in group {
                    let app = app.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |window, _| {
                            let p = design::palette(window.appearance());
                            div()
                                .h(px(layout::MENU_ROW))
                                .flex()
                                .items_center()
                                .gap(px(space::S10))
                                .font_family(design::ui_font())
                                .text_size(px(text::BODY))
                                .child(icon(glyph, icon_size::ROW, p.muted))
                                .child(title)
                        })
                        .on_click(move |_, window, cx| {
                            app.update(cx, |this, cx| run(this, window, cx)).ok();
                        }),
                    );
                }
            }
            menu
        }))
    }

    pub(super) fn render_context_menu(&self) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        Some(
            deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(menu.menu.clone()))
                .into_any_element(),
        )
    }
}
