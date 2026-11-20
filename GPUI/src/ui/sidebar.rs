//! The sidebar tree, as in `SidebarViewController`: unified mailboxes first (Inbox expands into
//! each account's inbox), then an "Accounts" section with each account's mailboxes and its
//! labels or folders. Expansion is remembered by node key.

use super::*;
use crate::controls::{disclosure_chevron, expander, tooltip};

use mailstream::{Account, Mailbox};
use crate::settings::ShowSettings;

/// What a row draws before its title.
enum Leading {
    Icon(Icon),
    Dot(gpui::Hsla),
}

/// One row of the tree.
struct Node {
    key: SharedString,
    depth: usize,
    title: SharedString,
    leading: Leading,
    badge: u32,
    /// Whether it has children, and so a disclosure triangle.
    expandable: bool,
    /// What selecting it shows; `None` rows only fold.
    source: Option<Pick>,
    tooltip: Option<SharedString>,
    /// Why the row's account can't connect (MS-278): a warning glyph at the row's end says so.
    problem: Option<SharedString>,
}

/// What a selectable row shows.
#[derive(Clone, PartialEq)]
enum Pick {
    Mail(Source),
    /// Google Chat: every account's spaces, or one account's.
    Chat(Option<SharedString>),
    Calendar,
}

impl MailApp {
    /// The sidebar, on the window: a top band that moves the window and hides the sidebar, then
    /// the tree under "Mailboxes", and Add Account… at its end.
    pub(super) fn render_sidebar(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for node in self.sidebar_nodes(cx) {
            let is_calendar = node.key.as_ref() == "calendar";
            rows.push(self.sidebar_row(node, p, cx));
            if is_calendar && self.calendar_view {
                rows.extend(self.calendar.update(cx, |pane, cx| pane.render_sidebar_section(p, window, cx)));
            }
        }
        rows.push(
            destination("add-account".into(), 0, false, p)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowSettings), cx))
                .child(icon(Icon::Add, icon_size::TOOLBAR, p.accent))
                .child(div().flex_1().min_w_0().truncate().child("Add Account…"))
                .into_any_element(),
        );
        div()
            .w(px(layout::SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(
                title_bar("sidebar-title", window, cx)
                    .h(px(layout::SIDEBAR_TOP))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(space::S8))
                    .child(
                        icon_button("hide-sidebar", Icon::Sidebar, "Hide Sidebar", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_sidebar(&ToggleSidebar, window, cx)
                            })),
                    ),
            )
            .child(group_title("Mailboxes", p))
            .child(
                div()
                    .id("sidebar")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(space::S10))
                    .pb(px(space::S12))
                    // A press on the rows or between them never moves the window.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(rows),
            )
    }

    /// The rail standing in for the hidden sidebar: a button that brings it back, then the unified
    /// mailboxes and Chat, each a glyph over its name.
    pub(super) fn render_rail(&self, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let chat = self.chat.read(cx);
        let mut items: Vec<AnyElement> = Vec::new();
        for role in UNIFIED {
            let members: Vec<&Mailbox> = self
                .accounts
                .iter()
                .filter_map(|account| {
                    // Archive falls back to each account's All Mail, as in the sidebar.
                    let all = self.mailboxes.get(&account.id)?;
                    all.iter()
                        .find(|b| b.role == role)
                        .or_else(|| if role == MailboxRole::Archive { all.iter().find(|b| b.role == MailboxRole::All) } else { None })
                })
                .collect();
            if members.is_empty() {
                continue;
            }
            let source = Source::Unified(role);
            let selected = self.mail_in_view() && self.source == source && self.query.is_none();
            let count = members.iter().map(|b| b.badge()).sum::<u32>();
            items.push(
                rail_item(role.as_str(), Icon::for_role(role), role.label(), count, selected, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select(source.clone(), window, cx)))
                    .into_any_element(),
            );
        }
        if chat.shown() {
            let count = chat.unread(None) as u32;
            items.push(
                rail_item("chat", Icon::Chat, "Chat", count, self.chat_view.is_some(), p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.select_chat(None, window, cx)))
                    .into_any_element(),
            );
        }
        if !self.accounts.is_empty() {
            items.push(
                rail_item("calendar", Icon::Calendar, "Calendar", 0, self.calendar_view, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.select_calendar(window, cx)))
                    .into_any_element(),
            );
        }
        div()
            .w(px(layout::RAIL_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .child(
                title_bar("rail-title", window, cx).w_full().pt(px(layout::RAIL_TOP)).pb(px(space::S8)).flex().justify_center().child(
                    icon_button("show-sidebar", Icon::Sidebar, "Show Sidebar", true, p.glyph)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_sidebar(&ToggleSidebar, window, cx))),
                ),
            )
            .child(
                div()
                    .id("rail")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .px(px(space::S8))
                    // As in the sidebar, a press on the destinations never moves the window.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(items),
            )
            .into_any_element()
    }

    /// Slide-over navigation drawer for compact viewports (MS-408, MS-417): matches desktop's
    /// warm `p.window` sidebar surface, `p.accent` icons, and warm `p.selection` row highlight.
    pub(super) fn render_drawer(&self, viewport_width: f32, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let drawer_width = design::compact::DRAWER_WIDTH.min((viewport_width * 0.84).max(248.));
        let mut rows: Vec<AnyElement> = Vec::new();
        for node in self.sidebar_nodes(cx) {
            let is_calendar = node.key.as_ref() == "calendar";
            rows.push(self.drawer_row(node, p, cx));
            if is_calendar && self.calendar_view {
                rows.extend(self.calendar.update(cx, |pane, cx| pane.render_sidebar_section(p, window, cx)));
            }
        }
        let account_summary: Option<SharedString> = match self.accounts.len() {
            0 => None,
            1 => Some("1 account".into()),
            n => Some(format!("{n} accounts").into()),
        };
        let scrim_alpha = if p.dark { 0.54 } else { 0.32 };
        div()
            .id("compact-drawer-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .bg(gpui::hsla(0., 0., 0., scrim_alpha))
            .occlude()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_drawer(cx)))
            .child(
                div()
                    .id("compact-drawer-panel")
                    .w(px(drawer_width))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .bg(p.window)
                    .rounded_r(px(radius::PANE + 4.))
                    .overflow_hidden()
                    .pt(px(design::compact::safe_area_insets().0))
                    .pb(px(design::compact::safe_area_insets().1))
                    .border_r(px(stroke::HAIRLINE))
                    .border_color(p.island_rim)
                    .shadow(p.popover_shadow.clone())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(design::compact::TOP_BAR_HEIGHT))
                            .pl(px(space::S16))
                            .pr(px(space::S10))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(space::S8))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(design::compact::HEADER_TITLE))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(p.text)
                                            .child("Mailstream"),
                                    )
                                    .when_some(account_summary, |d, summary| {
                                        d.child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(summary))
                                    }),
                            )
                            .child(
                                capsule(p).child(
                                    icon_button("drawer-close", Icon::Close, "Close Menu", true, p.glyph)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_drawer(cx))),
                                ),
                            ),
                    )
                    .child(group_title("Mailboxes", p).pt(px(space::S6)).pl(px(space::S16)))
                    .child(
                        div()
                            .id("drawer-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(space::S10))
                            .py(px(space::S4))
                            .children(rows),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(space::S10))
                            .pt(px(space::S8))
                            .pb(px(space::S12))
                            .child(
                                div()
                                    .id("drawer-add-account")
                                    .h(px(design::compact::DRAWER_ROW + 2.))
                                    .px(px(space::S12))
                                    .flex()
                                    .items_center()
                                    .gap(px(space::S10))
                                    .rounded(px(radius::CARD))
                                    .bg(p.card)
                                    .border(px(stroke::HAIRLINE))
                                    .border_color(p.card_border)
                                    .shadow(p.card_shadow.clone())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(p.half_lifted))
                                    .active(|s| s.bg(p.pressed))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.close_drawer(cx);
                                        this.show_settings(&ShowSettings, window, cx);
                                    }))
                                    .child(icon(Icon::Add, icon_size::TOOLBAR, p.accent))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(text::BODY))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.text)
                                            .child("Settings & Accounts"),
                                    )
                                    .child(icon(Icon::Disclosure, 12., p.muted)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Touch-friendly drawer row matching desktop `sidebar_row` styling (accent icons, warm
    /// `p.selection` rounded rect, muted count).
    fn drawer_row(&self, node: Node, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let selected = node.source.as_ref().is_some_and(|pick| match pick {
            Pick::Mail(source) => self.mail_in_view() && self.source == *source && self.query.is_none(),
            Pick::Chat(account) => self.chat_view.as_ref() == Some(account),
            Pick::Calendar => self.calendar_view,
        });
        let expanded = self.expanded.contains(&node.key);
        let key = node.key.clone();
        let toggle_key = node.key.clone();
        let source = node.source.clone();

        if node.key.starts_with("section:") {
            return div()
                .pt(px(space::S10))
                .pb(px(space::S2))
                .child(
                    expander(ElementId::Name(format!("drawer:{}", key).into()), expanded, node.title, p)
                        .title_color(p.muted)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_expanded(&key, cx))),
                )
                .into_any_element();
        }

        let leading: AnyElement = match node.leading {
            Leading::Icon(glyph) => icon(glyph, icon_size::TOOLBAR, p.accent).into_any_element(),
            Leading::Dot(color) => div()
                .flex_shrink_0()
                .w(px(icon_size::TOOLBAR))
                .flex()
                .justify_center()
                .child(mark_dot(color))
                .into_any_element(),
        };
        let disclosure = node.expandable.then(|| {
            div()
                .id(ElementId::Name(format!("drawer-disclose:{}", node.key).into()))
                .flex_shrink_0()
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .hover(|s| s.bg(p.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_expanded(&toggle_key, cx);
                }))
                .child(disclosure_chevron(expanded, p.muted))
        });

        div()
            .id(ElementId::Name(format!("drawer:{}", node.key).into()))
            .h(px(design::compact::DRAWER_ROW))
            .mb(px(2.))
            .ml(px(node.depth as f32 * layout::SIDEBAR_INDENT))
            .pl(px(space::S10))
            .pr(px(space::S8))
            .flex()
            .items_center()
            .gap(px(space::S10))
            .rounded(px(radius::CONTROL))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)).active(|s| s.bg(p.pressed)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| match &source {
                Some(Pick::Mail(source)) => this.select(source.clone(), window, cx),
                Some(Pick::Chat(account)) => this.select_chat(account.clone(), window, cx),
                Some(Pick::Calendar) => this.select_calendar(window, cx),
                None => this.toggle_expanded(&key, cx),
            }))
            .child(leading)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(text::READING))
                    .text_color(p.text)
                    .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                    .child(node.title),
            )
            .when(node.badge > 0, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(text::FOOTNOTE))
                        .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                        .text_color(p.muted)
                        .child(count(node.badge)),
                )
            })
            .when(node.problem.is_some(), |d| d.child(icon(Icon::Spam, icon_size::ROW, p.danger)))
            .children(disclosure)
            .into_any_element()
    }

    /// Floating glass bottom dock for compact viewports (MS-417): Mailstream's signature glass
    /// capsule holding Mail / Chat / Calendar tabs beside an integrated circular Compose button,
    /// over a soft bottom fade scrim so list rows glide smoothly underneath without obstruction.
    pub(super) fn render_compact_dock(
        &self,
        show_fab: bool,
        safe_bottom: f32,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mail_unread: u32 = self
            .accounts
            .iter()
            .filter_map(|account| self.mailboxes.get(&account.id)?.iter().find(|b| b.role == MailboxRole::Inbox))
            .map(|b| b.badge())
            .sum();
        let chat = self.chat.read(cx);
        let show_chat = chat.shown();
        let chat_unread = chat.unread(None) as u32;

        let mail_selected = self.mail_in_view();
        let chat_selected = self.chat_view.is_some();
        let cal_selected = self.calendar_view;

        let mut nav = div()
            .id("compact-bottom-bar")
            .flex_1()
            .min_w_0()
            .h(px(design::compact::DOCK_HEIGHT))
            .p(px(design::compact::DOCK_PADDING))
            .rounded_full()
            .bg(gpui::linear_gradient(
                180.,
                gpui::linear_color_stop(p.capsule_glass[0], 0.),
                gpui::linear_color_stop(p.capsule_glass[1], 1.),
            ))
            .border(px(stroke::HAIRLINE))
            .border_color(p.capsule_rim)
            .shadow(p.capsule_shadow.clone())
            .occlude()
            .flex()
            .items_center()
            .gap(px(space::S2))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        nav = nav.child(
            bottom_nav_item("nav-mail", Icon::Inbox, "Mail", mail_unread, mail_selected, p).on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| {
                    if !this.mail_in_view() {
                        this.select(this.source.clone(), window, cx);
                    } else if !this.layers.is_empty() {
                        this.select(Source::Unified(MailboxRole::Inbox), window, cx);
                    }
                },
            )),
        );
        if show_chat {
            nav = nav.child(
                bottom_nav_item("nav-chat", Icon::Chat, "Chat", chat_unread, chat_selected, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.select_chat(None, window, cx))),
            );
        }
        if !self.accounts.is_empty() {
            nav = nav.child(
                bottom_nav_item("nav-calendar", Icon::Calendar, "Calendar", 0, cal_selected, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.select_calendar(window, cx))),
            );
        }

        let fab = show_fab.then(|| {
            let (id, glyph) = if self.chat_view.is_some() {
                ("fab-new-chat", Icon::Compose)
            } else if self.calendar_view {
                ("fab-new-event", Icon::AddDraft)
            } else {
                ("fab-compose", Icon::Compose)
            };
            let in_chat = self.chat_view.is_some();
            let in_calendar = self.calendar_view;
            let top_accent = p.accent.blend(gpui::hsla(0., 0., 1., 0.14));
            div()
                .id(id)
                .flex_shrink_0()
                .size(px(design::compact::FAB_SIZE))
                .rounded_full()
                .bg(gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(top_accent, 0.),
                    gpui::linear_color_stop(p.accent, 1.),
                ))
                .border(px(stroke::HAIRLINE))
                .border_color(gpui::hsla(0., 0., 1., if p.dark { 0.22 } else { 0.28 }))
                .shadow(p.lifted_shadow.clone())
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .active(|s| s.opacity(0.86))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if in_chat {
                        this.chat.update(cx, |chat, cx| chat.start_new_chat(window, cx));
                    } else if in_calendar {
                        this.calendar.update(cx, |calendar, cx| calendar.start_new_event(window, cx));
                    } else {
                        this.new_message(&NewMessage, window, cx);
                    }
                }))
                .child(icon(glyph, 20., p.on_accent))
        });

        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .child(bottom_scrim(p.tray, design::compact::DOCK_CLEARANCE + safe_bottom))
            .child(
                div()
                    .relative()
                    .w_full()
                    .px(px(design::compact::DOCK_MARGIN_X))
                    .pb(px(safe_bottom + design::compact::DOCK_MARGIN_B))
                    .flex()
                    .items_center()
                    .gap(px(space::S10))
                    .child(nav)
                    .children(fab),
            )
            .into_any_element()
    }

    /// The visible rows: the tree flattened through expanded nodes.
    fn sidebar_nodes(&self, cx: &App) -> Vec<Node> {
        let mut nodes = Vec::new();
        let boxes = |account: &Account| self.mailboxes.get(&account.id).map(Vec::as_slice).unwrap_or_default();

        for role in UNIFIED {
            // Archive falls back to each account's All Mail, as `unifiedMailboxes` does.
            let members: Vec<(usize, &Account, &Mailbox)> = self
                .accounts
                .iter()
                .enumerate()
                .filter_map(|(index, account)| {
                    let all = boxes(account);
                    all.iter()
                        .find(|b| b.role == role)
                        .or_else(|| if role == MailboxRole::Archive { all.iter().find(|b| b.role == MailboxRole::All) } else { None })
                        .map(|b| (index, account, b))
                })
                .collect();
            if members.is_empty() {
                continue;
            }
            let key: SharedString = format!("unified:{}", role.as_str()).into();
            let children = role == MailboxRole::Inbox && members.len() > 1;
            let expanded = children && self.expanded.contains(&key);
            nodes.push(Node {
                key,
                depth: 0,
                title: role.label().into(),
                leading: Leading::Icon(Icon::for_role(role)),
                badge: members.iter().map(|(_, _, b)| b.badge()).sum(),
                expandable: children,
                source: Some(Pick::Mail(Source::Unified(role))),
                tooltip: None,
                problem: None,
            });
            if expanded {
                for (index, account, inbox) in members {
                    nodes.push(Node {
                        key: format!("mailbox:{}\u{1}{}", account.id, inbox.id).into(),
                        depth: 1,
                        title: account.title(),
                        leading: Leading::Dot(design::account_color(index)),
                        badge: inbox.unread.and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
                        expandable: false,
                        source: Some(Pick::Mail(mailbox_source(account, inbox))),
                        tooltip: Some(account.id.clone().into()),
                        problem: self.account_problems.get(&account.id).cloned(),
                    });
                }
            }
        }

        // Google Chat, for accounts signed in to it: one node, expanding into accounts when there are several.
        let chat = self.chat.read(cx);
        if chat.shown() {
            let key: SharedString = "chat".into();
            let several = chat.accounts().len() > 1;
            let open = several && self.expanded.contains(&key);
            nodes.push(Node {
                key,
                depth: 0,
                title: "Chat".into(),
                leading: Leading::Icon(Icon::Chat),
                badge: chat.unread(None) as u32,
                expandable: several,
                source: Some(Pick::Chat(None)),
                tooltip: None,
                problem: None,
            });
            if open {
                for (position, account) in chat.accounts().iter().enumerate() {
                    // Chat accounts take their mail account's color, as natively.
                    let index = self.accounts.iter().position(|a| a.id == account.id).unwrap_or(position);
                    nodes.push(Node {
                        key: format!("chat:{}", account.id).into(),
                        depth: 1,
                        title: account.label.clone(),
                        leading: Leading::Dot(design::account_color(index)),
                        badge: chat.unread(Some(&account.id)) as u32,
                        expandable: false,
                        source: Some(Pick::Chat(Some(account.id.clone()))),
                        tooltip: Some(account.id.clone().into()),
                        problem: None,
                    });
                }
            }
        }

        if self.accounts.is_empty() {
            return nodes;
        }
        nodes.push(Node {
            key: "calendar".into(),
            depth: 0,
            title: "Calendar".into(),
            leading: Leading::Icon(Icon::Calendar),
            badge: 0,
            expandable: false,
            source: Some(Pick::Calendar),
            tooltip: None,
            problem: None,
        });
        // While Calendar is in view, its own "Calendars" section takes the place of the mail
        // accounts below (`render_sidebar`).
        if self.calendar_view {
            return nodes;
        }
        let section: SharedString = "section:accounts".into();
        let open = self.expanded.contains(&section);
        nodes.push(Node {
            key: section,
            depth: 0,
            title: "Accounts".into(),
            leading: Leading::Icon(Icon::Person),
            badge: 0,
            expandable: true,
            source: None,
            tooltip: None,
            problem: None,
        });
        if !open {
            return nodes;
        }
        for (index, account) in self.accounts.iter().enumerate() {
            let all = boxes(account);
            let key: SharedString = format!("account:{}", account.id).into();
            let expanded = self.expanded.contains(&key);
            nodes.push(Node {
                key,
                depth: 0,
                title: account.title(),
                leading: Leading::Dot(design::account_color(index)),
                badge: 0,
                expandable: !all.is_empty(),
                source: None,
                tooltip: Some(account.id.clone().into()),
                problem: self.account_problems.get(&account.id).cloned(),
            });
            if !expanded {
                continue;
            }
            for mailbox in all.iter().filter(|b| b.role != MailboxRole::Other) {
                nodes.push(mailbox_node(account, mailbox, 1));
            }
            let others: Vec<&Mailbox> = all.iter().filter(|b| b.role == MailboxRole::Other).collect();
            if others.is_empty() {
                continue;
            }
            let key: SharedString = format!("folders:{}", account.id).into();
            let folders_open = self.expanded.contains(&key);
            nodes.push(Node {
                key,
                depth: 1,
                title: account.folders_title().into(),
                leading: Leading::Icon(Icon::Label),
                badge: 0,
                expandable: true,
                source: None,
                tooltip: None,
                problem: None,
            });
            if folders_open {
                nodes.extend(others.into_iter().map(|mailbox| mailbox_node(account, mailbox, 2)));
            }
        }
        nodes
    }

    fn sidebar_row(&self, node: Node, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let selected = node.source.as_ref().is_some_and(|pick| match pick {
            Pick::Mail(source) => self.mail_in_view() && self.source == *source && self.query.is_none(),
            Pick::Chat(account) => self.chat_view.as_ref() == Some(account),
            Pick::Calendar => self.calendar_view,
        });
        let expanded = self.expanded.contains(&node.key);
        let key = node.key.clone();
        let toggle_key = node.key.clone();
        let source = node.source.clone();

        if node.key.starts_with("section:") {
            // A group's title; it folds the accounts.
            return div()
                .pt(px(space::S6))
                .child(
                    expander(key.clone(), expanded, node.title, p)
                        .title_color(p.muted)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_expanded(&key, cx))),
                )
                .into_any_element();
        }

        let leading: AnyElement = match node.leading {
            Leading::Icon(glyph) => icon(glyph, icon_size::TOOLBAR, p.accent).into_any_element(),
            Leading::Dot(color) => div()
                .flex_shrink_0()
                .w(px(icon_size::TOOLBAR))
                .flex()
                .justify_center()
                .child(mark_dot(color))
                .into_any_element(),
        };
        let disclosure = node.expandable.then(|| {
            div()
                .id(ElementId::Name(format!("disclose:{}", node.key).into()))
                .flex_shrink_0()
                .w(px(22.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_expanded(&toggle_key, cx);
                }))
                .child(disclosure_chevron(expanded, p.muted))
        });

        let tip = match (node.tooltip, &node.problem) {
            (Some(tip), Some(problem)) => Some(format!("{tip} can't connect: {problem}").into()),
            (tip, _) => tip,
        };
        destination(node.key.clone(), node.depth, selected, p)
            .when_some(tip, |d, tip: SharedString| d.tooltip(tooltip(tip)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| match &source {
                Some(Pick::Mail(source)) => this.select(source.clone(), window, cx),
                Some(Pick::Chat(account)) => this.select_chat(account.clone(), window, cx),
                Some(Pick::Calendar) => this.select_calendar(window, cx),
                // Rows that show nothing themselves fold, as clicking them does natively.
                None => this.toggle_expanded(&key, cx),
            }))
            .child(leading)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(node.title),
            )
            .when(node.badge > 0, |d| {
                d.child(div().flex_shrink_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(count(node.badge)))
            })
            .when(node.problem.is_some(), |d| d.child(icon(Icon::Spam, icon_size::ROW, p.danger)))
            .children(disclosure)
            .into_any_element()
    }
}

/// A sidebar row: the glyph and the name, the count at its end, a rounded gray highlight when
/// selected, the wash under the pointer; `depth` steps it in.
fn destination(key: SharedString, depth: usize, selected: bool, p: &'static Palette) -> Stateful<Div> {
    div()
        .id(ElementId::Name(key))
        .h(px(layout::SIDEBAR_ROW))
        .mb(px(1.))
        .ml(px(depth as f32 * layout::SIDEBAR_INDENT))
        .pl(px(space::S8))
        .pr(px(space::S6))
        .flex()
        .items_center()
        .gap(px(space::S8))
        .rounded(px(radius::CONTROL))
        .cursor_pointer()
        .when(selected, |d| d.bg(p.selection))
        .when(!selected, |d| d.hover(|s| s.bg(p.hover)).active(|s| s.bg(p.pressed)))
}

/// A sidebar group's title: small, bold, secondary, as the Mac's sidebars head their groups.
fn group_title(title: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .pt(px(14.))
        .pb(px(space::S4))
        .pl(px(space::S20))
        .text_size(px(text::CAPTION))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .child(title.into())
}

/// A rail destination: the glyph on a rounded highlight when selected, its name below.
fn rail_item(id: &'static str, glyph: Icon, label: &'static str, count: u32, selected: bool, p: &'static Palette) -> Stateful<Div> {
    div()
        .id(id)
        .group("rail-item")
        .mb(px(space::S6))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.))
        .cursor_pointer()
        .child(
            div()
                .relative()
                .w(px(48.))
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::CONTROL))
                .when(selected, |d| d.bg(p.selection))
                .when(!selected, |d| d.group_hover("rail-item", |s| s.bg(p.hover)))
                .child(icon(glyph, 19., if selected { p.accent } else { p.muted }))
                .when(count > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(-2.))
                            .right(px(2.))
                            .h(px(15.))
                            .min_w(px(15.))
                            .px(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(p.capsule)
                            .border(px(stroke::HAIRLINE))
                            .border_color(p.capsule_rim)
                            .text_size(px(text::TAG))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.muted)
                            .child(count_label(count)),
                    )
                }),
        )
        .child(div().text_size(px(text::CAPTION)).text_color(if selected { p.text } else { p.muted }).child(label))
}

/// One destination inside the floating glass bottom dock (MS-417): active tab raises on a warm
/// pill echoing desktop's `segmented` and `rail_item` highlights, with refined unread capsule badge.
fn bottom_nav_item(
    id: &'static str,
    glyph: Icon,
    label: &'static str,
    badge: u32,
    selected: bool,
    p: &'static Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .h_full()
        .rounded_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(1.))
        .cursor_pointer()
        .when(selected, |d| {
            d.bg(if p.dark { p.selection } else { p.raised_card })
                .border(px(stroke::HAIRLINE))
                .border_color(p.card_border)
                .shadow(p.card_shadow.clone())
        })
        .when(!selected, |d| d.hover(|s| s.bg(p.hover)).active(|s| s.bg(p.pressed)))
        .child(
            div()
                .relative()
                .w(px(36.))
                .h(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, 18., if selected { p.accent } else { p.glyph }))
                .when(badge > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(-3.))
                            .right(px(-5.))
                            .h(px(14.))
                            .min_w(px(14.))
                            .px(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(if selected { p.accent } else { p.capsule })
                            .when(!selected, |d| d.border(px(stroke::HAIRLINE)).border_color(p.capsule_rim))
                            .text_size(px(9.5))
                            .font_weight(FontWeight::BOLD)
                            .text_color(if selected { p.on_accent } else { p.glyph })
                            .child(count_label(badge)),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .text_color(if selected { p.text } else { p.muted })
                .child(label),
        )
}

/// A count as the sidebar shows it: thousands shortened.
fn count(n: u32) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", f64::from(n) / 1000.)
    } else {
        n.to_string()
    }
}

/// A rail badge's count: past 99 it says so.
fn count_label(n: u32) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

fn mailbox_source(account: &Account, mailbox: &Mailbox) -> Source {
    Source::Mailbox {
        account: account.id.clone(),
        id: mailbox.id.clone(),
        name: if mailbox.name.is_empty() { mailbox.role.label().into() } else { mailbox.name.clone().into() },
        role: mailbox.role,
    }
}

fn mailbox_node(account: &Account, mailbox: &Mailbox, depth: usize) -> Node {
    let source = mailbox_source(account, mailbox);
    Node {
        key: format!("mailbox:{}\u{1}{}", account.id, mailbox.id).into(),
        depth,
        title: source.title(),
        leading: Leading::Icon(Icon::for_role(mailbox.role)),
        badge: mailbox.badge(),
        expandable: false,
        source: Some(Pick::Mail(source)),
        tooltip: None,
        problem: None,
    }
}
