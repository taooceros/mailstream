//! The sidebar tree, as in `SidebarViewController`: unified mailboxes first (Inbox expands into
//! each account's inbox), then an "Accounts" section with each account's mailboxes and its
//! labels or folders. Expansion is remembered by node key.

use super::*;
use gpui::{AppContext, Transformation, radians};

use crate::controls::Tooltip;

use crate::model::{Account, Mailbox};
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
}

/// What a selectable row shows.
#[derive(Clone, PartialEq)]
enum Pick {
    Mail(Source),
    /// Google Chat: every account's spaces, or one account's.
    Chat(Option<SharedString>),
}

impl MailApp {
    /// The sidebar, on the window: a top band that moves the window and hides the sidebar, then
    /// the tree under "Mailboxes", and Add Account… at its end.
    pub(super) fn render_sidebar(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for node in self.sidebar_nodes(cx) {
            rows.push(self.sidebar_row(node, p, cx));
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
                title_bar("sidebar-title")
                    .h(px(layout::SIDEBAR_TOP))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(space::S8))
                    .child(
                        icon_button("hide-sidebar", Icon::Sidebar, "Hide Sidebar", true, p.muted, p)
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
                    .children(rows),
            )
    }

    /// The rail standing in for the hidden sidebar: a button that brings it back, then the unified
    /// mailboxes and Chat, each a glyph over its name.
    pub(super) fn render_rail(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let chat = self.chat.read(cx);
        let mut items: Vec<AnyElement> = Vec::new();
        for role in Role::UNIFIED {
            let members: Vec<&Mailbox> = self
                .accounts
                .iter()
                .filter_map(|account| {
                    // Archive falls back to each account's All Mail, as in the sidebar.
                    let all = self.mailboxes.get(&account.id)?;
                    all.iter()
                        .find(|b| b.role == role)
                        .or_else(|| if role == Role::Archive { all.iter().find(|b| b.role == Role::All) } else { None })
                })
                .collect();
            if members.is_empty() {
                continue;
            }
            let source = Source::Unified(role);
            let selected = self.chat_view.is_none() && self.source == source && self.query.is_none();
            let count = members.iter().map(|b| b.badge()).sum::<u32>();
            items.push(
                rail_item(role.arg(), Icon::for_role(role), role.title(), count, selected, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select(source.clone(), window, cx)))
                    .into_any_element(),
            );
        }
        if !chat.accounts().is_empty() {
            let count = chat.unread(None) as u32;
            items.push(
                rail_item("chat", Icon::Chat, "Chat", count, self.chat_view.is_some(), p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.select_chat(None, window, cx)))
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
                title_bar("rail-title").w_full().pt(px(layout::RAIL_TOP)).pb(px(space::S8)).flex().justify_center().child(
                    icon_button("show-sidebar", Icon::Sidebar, "Show Sidebar", true, p.muted, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_sidebar(&ToggleSidebar, window, cx))),
                ),
            )
            .child(div().id("rail").flex_1().min_h_0().w_full().overflow_y_scroll().px(px(space::S8)).children(items))
            .into_any_element()
    }

    /// The visible rows: the tree flattened through expanded nodes.
    fn sidebar_nodes(&self, cx: &App) -> Vec<Node> {
        let mut nodes = Vec::new();
        let boxes = |account: &Account| self.mailboxes.get(&account.id).map(Vec::as_slice).unwrap_or_default();

        for role in Role::UNIFIED {
            // Archive falls back to each account's All Mail, as `unifiedMailboxes` does.
            let members: Vec<(usize, &Account, &Mailbox)> = self
                .accounts
                .iter()
                .enumerate()
                .filter_map(|(index, account)| {
                    let all = boxes(account);
                    all.iter()
                        .find(|b| b.role == role)
                        .or_else(|| if role == Role::Archive { all.iter().find(|b| b.role == Role::All) } else { None })
                        .map(|b| (index, account, b))
                })
                .collect();
            if members.is_empty() {
                continue;
            }
            let key: SharedString = format!("unified:{}", role.arg()).into();
            let children = role == Role::Inbox && members.len() > 1;
            let expanded = children && self.expanded.contains(&key);
            nodes.push(Node {
                key,
                depth: 0,
                title: role.title().into(),
                leading: Leading::Icon(Icon::for_role(role)),
                badge: members.iter().map(|(_, _, b)| b.badge()).sum(),
                expandable: children,
                source: Some(Pick::Mail(Source::Unified(role))),
                tooltip: None,
            });
            if expanded {
                for (index, account, inbox) in members {
                    nodes.push(Node {
                        key: format!("mailbox:{}\u{1}{}", account.id, inbox.id).into(),
                        depth: 1,
                        title: account.title(),
                        leading: Leading::Dot(design::account_color(index)),
                        badge: inbox.unread.unwrap_or(0),
                        expandable: false,
                        source: Some(Pick::Mail(mailbox_source(account, inbox))),
                        tooltip: Some(account.id.clone()),
                    });
                }
            }
        }

        // Google Chat, for accounts signed in to it: one node, expanding into accounts when there are several.
        let chat = self.chat.read(cx);
        if !chat.accounts().is_empty() {
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
                        tooltip: Some(account.id.clone()),
                    });
                }
            }
        }

        if self.accounts.is_empty() {
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
                tooltip: Some(account.id.clone()),
            });
            if !expanded {
                continue;
            }
            for mailbox in all.iter().filter(|b| b.role != Role::Other) {
                nodes.push(mailbox_node(account, mailbox, 1));
            }
            let others: Vec<&Mailbox> = all.iter().filter(|b| b.role == Role::Other).collect();
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
            });
            if folders_open {
                nodes.extend(others.into_iter().map(|mailbox| mailbox_node(account, mailbox, 2)));
            }
        }
        nodes
    }

    fn sidebar_row(&self, node: Node, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let selected = node.source.as_ref().is_some_and(|pick| match pick {
            Pick::Mail(source) => self.chat_view.is_none() && self.source == *source && self.query.is_none(),
            Pick::Chat(account) => self.chat_view.as_ref() == Some(account),
        });
        let expanded = self.expanded.contains(&node.key);
        let key = node.key.clone();
        let toggle_key = node.key.clone();
        let source = node.source.clone();

        if node.key.starts_with("section:") {
            // A group's title; it folds the accounts.
            return group_title(node.title, p)
                .id(ElementId::Name(key.clone()))
                .mx(px(-space::S10))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_expanded(&key, cx)))
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
                .child(icon(Icon::Disclosure, 13., p.muted).with_transformation(Transformation::rotate(radians(
                    if expanded { std::f32::consts::FRAC_PI_2 } else { 0. },
                ))))
        });

        destination(node.key.clone(), node.depth, selected, p)
            .when_some(node.tooltip, |d, tip| {
                d.tooltip(move |_, cx| cx.new(|_| Tooltip { text: tip.clone(), p }).into())
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| match &source {
                Some(Pick::Mail(source)) => this.select(source.clone(), window, cx),
                Some(Pick::Chat(account)) => this.select_chat(account.clone(), window, cx),
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
        name: if mailbox.name.is_empty() { mailbox.role.title().into() } else { mailbox.name.clone() },
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
    }
}
