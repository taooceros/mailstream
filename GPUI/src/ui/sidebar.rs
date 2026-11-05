//! The sidebar tree, as in `SidebarViewController`: unified mailboxes first (Inbox expands into
//! each account's inbox), then an "Accounts" section with each account's mailboxes and its
//! labels or folders. Expansion is remembered by node key.

use super::*;
use gpui::{Transformation, radians};

use crate::model::{Account, Mailbox};

/// What a row draws before its title.
enum Leading {
    Icon(Icon),
    Dot(Hsla),
    Avatar(Address, Hsla),
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
    /// Account inboxes under the unified Inbox: secondary text.
    secondary: bool,
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
    pub(super) fn render_sidebar(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for node in self.sidebar_nodes(cx) {
            rows.push(self.sidebar_row(node, p, cx));
        }
        div()
            .w(px(layout::SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.line)
            .child(
                title_bar("sidebar-title")
                    .h(px(layout::TOOLBAR_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(10.))
                    .child(
                        icon_button("hide-sidebar", Icon::Sidebar, "Hide Sidebar", true, p.muted, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_sidebar(&ToggleSidebar, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("sidebar")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(10.))
                    .pb(px(16.))
                    .children(rows),
            )
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
                secondary: false,
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
                        secondary: true,
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
                secondary: false,
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
                        secondary: true,
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
            secondary: false,
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
                leading: Leading::Avatar(account.address(), design::account_color(index)),
                badge: 0,
                expandable: !all.is_empty(),
                source: None,
                secondary: false,
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
                secondary: false,
                tooltip: None,
            });
            if folders_open {
                nodes.extend(others.into_iter().map(|mailbox| mailbox_node(account, mailbox, 2)));
            }
        }
        nodes
    }

    fn sidebar_row(&self, node: Node, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let is_section = node.key.starts_with("section:");
        let selected = node.source.as_ref().is_some_and(|pick| match pick {
            Pick::Mail(source) => self.chat_view.is_none() && self.source == *source && self.query.is_none(),
            Pick::Chat(account) => self.chat_view.as_ref() == Some(account),
        });
        let expanded = self.expanded.contains(&node.key);
        let key = node.key.clone();
        let toggle_key = node.key.clone();
        let source = node.source.clone();

        let disclosure = node.expandable.then(|| {
            div()
                .id(ElementId::Name(format!("disclose:{}", node.key).into()))
                .flex_shrink_0()
                .size(px(icon_size::SMALL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::SMALL))
                .hover(|s| s.bg(p.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_expanded(&toggle_key, cx);
                }))
                .child(icon(Icon::Disclosure, icon_size::SMALL, p.muted).with_transformation(Transformation::rotate(
                    radians(if expanded { std::f32::consts::FRAC_PI_2 } else { 0. }),
                )))
        });

        if is_section {
            // A group row: small caps, folds the accounts.
            return div()
                .id(ElementId::Name(key.clone()))
                .group("section")
                .mt(px(14.))
                .h(px(layout::SIDEBAR_ROW))
                .px(px(8.))
                .flex()
                .items_center()
                .text_size(px(text::CAPTION))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_expanded(&key, cx)))
                .child(div().flex_1().child(node.title))
                .child(
                    div()
                        .invisible()
                        .group_hover("section", |s| s.visible())
                        .text_color(p.faint)
                        .child(if expanded { "Hide" } else { "Show" }),
                )
                .into_any_element();
        }

        let leading: AnyElement = match node.leading {
            Leading::Icon(glyph) => {
                icon(glyph, icon_size::SMALL, if selected { p.accent } else { p.muted }).into_any_element()
            }
            Leading::Dot(color) => div()
                .flex_shrink_0()
                .size(px(icon_size::SMALL))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(8.)).rounded_full().bg(color))
                .into_any_element(),
            Leading::Avatar(address, color) => div()
                .flex_shrink_0()
                .size(px(icon_size::SMALL + 2.))
                .rounded_full()
                .bg(color)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(text::MICRO))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.on_accent)
                .child(address.initial())
                .into_any_element(),
        };

        div()
            .id(ElementId::Name(node.key.clone()))
            .h(px(layout::SIDEBAR_ROW))
            .pl(px(4. + node.depth as f32 * layout::SIDEBAR_INDENT))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(radius::CONTROL))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection).font_weight(FontWeight::MEDIUM))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .when_some(node.tooltip, |d, tip| {
                d.tooltip(move |_, cx| cx.new(|_| Tooltip { text: tip.clone(), p }).into())
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| match &source {
                Some(Pick::Mail(source)) => this.select(source.clone(), window, cx),
                Some(Pick::Chat(account)) => this.select_chat(account.clone(), window, cx),
                // Rows that show nothing themselves fold, as clicking them does natively.
                None => this.toggle_expanded(&key, cx),
            }))
            // The disclosure triangle, or room for one so titles line up.
            .child(match disclosure {
                Some(disclosure) => disclosure.into_any_element(),
                None => div().flex_shrink_0().w(px(icon_size::SMALL)).into_any_element(),
            })
            .child(leading)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(node.secondary, |d| d.text_color(p.muted))
                    .child(node.title),
            )
            .when(node.badge > 0, |d| {
                d.child(
                    div().flex_shrink_0().text_size(px(text::CAPTION)).text_color(p.muted).child(node.badge.to_string()),
                )
            })
            .into_any_element()
    }
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
        secondary: false,
        tooltip: None,
    }
}
