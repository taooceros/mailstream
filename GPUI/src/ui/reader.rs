//! The reader: a conversation's messages, folded except the newest and the unread, or, between
//! people, a chat log (Flutter's `Reader` and `_ChatLog`, MS-73). Quoted history and signatures
//! fold behind a "more" toggle in each body. The reader opens at its end and holds there while
//! bodies load, until it is scrolled (MS-70).

use chrono::{DateTime, Local};
use gpui::ScrollDelta;

use super::*;
use crate::controls::tooltip;
use crate::design::reader as metrics_reader;

impl MailApp {
    pub(super) fn render_reader(&self, conversation: &Conversation, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match self.threads.get(&conversation.id).map(|t| &t.state) {
            None | Some(ThreadState::Loading) => {
                div().py(px(24.)).text_color(p.muted).child("Loading messages…").into_any_element()
            }
            Some(ThreadState::Failed(error)) => notice(Tone::Error, None, error.clone(), false, p).into_any_element(),
            Some(ThreadState::Loaded(messages)) if conversation.chat_log() => self.render_chat_log(messages, p, cx),
            Some(ThreadState::Loaded(messages)) => {
                let last = messages.len().saturating_sub(1);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::S12))
                    .children(messages.iter().enumerate().map(|(index, message)| {
                        let open = index == last || message.unread || self.unfolded.contains(&message.id);
                        self.render_message(message, open, index == last, p, cx)
                    }))
                    .into_any_element()
            }
        };
        let account = self.accounts.iter().position(|a| a.id == conversation.account).map(|index| {
            let title = self.accounts[index].title();
            pill(title, Some(mark_dot(design::account_color(index)).into_any_element()), p)
        });
        if self.reader_pinned {
            // Taken up as the reader lays out: the end of what it holds now.
            self.reader_scroll.scroll_to_bottom();
        }
        div()
            .id("reader")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.reader_scroll)
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, _| {
                let vertical = match event.delta {
                    ScrollDelta::Pixels(delta) => delta.y != px(0.),
                    ScrollDelta::Lines(delta) => delta.y != 0.,
                };
                if vertical {
                    this.reader_pinned = false;
                }
            }))
            .child(
                div().w_full().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(layout::READING_WIDTH))
                        .px(px(space::S32))
                        .pt(px(layout::HEADER_HEIGHT + space::S16))
                        .pb(px(64.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(text::DISPLAY))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight::BOLD)
                                .child(conversation.title.clone()),
                        )
                        .child(
                            div()
                                .mt(px(space::S12))
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(space::S8))
                                .when(conversation.waiting, |d| {
                                    d.child(
                                        pill("Waiting on you", Some(icon(Icon::Reply, icon_size::INLINE, p.on_waiting_fill).into_any_element()), p)
                                            .id("reader-resolve")
                                            .bg(p.waiting_fill)
                                            .text_color(p.on_waiting_fill)
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                                this.toggle_waiting(&ToggleWaiting, window, cx)
                                            })),
                                    )
                                })
                                .when(!conversation.tag.is_empty(), |d| d.child(pill(conversation.tag.clone(), None, p)))
                                .children(conversation.labels.iter().map(|label| {
                                    pill(label.clone(), Some(mark_dot(design::topic_color(label, p.dark)).into_any_element()), p)
                                }))
                                .children(account)
                                .when(conversation.message_count > 1, |d| {
                                    d.child(pill(format!("{} messages", conversation.message_count), None, p))
                                })
                                .children(self.render_renderer_switch(p, cx)),
                        )
                        .child(div().mt(px(space::S24)).flex().flex_col().gap(px(space::S12)).child(body))
                        .when_some(self.reply.as_ref().filter(|r| r.conversation == conversation.id), |d, reply| {
                            d.child(div().mt(px(space::S12)).child(reply.view.clone()))
                        }),
                ),
            )
            .into_any_element()
    }

    /// A message as Mail shows one: the page's surface inside a hairline, rounded; its header
    /// (the sender, who it went to or a preview while folded, the date) folds it.
    fn render_message(
        &self,
        message: &Message,
        open: bool,
        last: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sender = message.sender();
        let id = message.id.clone();
        let photo = avatars::avatar(&sender, metrics::AVATAR, p, cx);
        let header = div()
            .id(ElementId::Name(format!("message:{id}").into()))
            .pl(px(space::S16))
            .pr(px(space::S8))
            .py(px(space::S12))
            .flex()
            .items_center()
            .gap(px(space::S16))
            .when(!last, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_fold(&id, cx)))
            })
            .child(photo)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::S2))
                    .child(
                        div()
                            .flex()
                            .gap(px(space::S8))
                            .items_baseline()
                            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(sender.display_name()))
                            .when(open && sender.name.is_some(), |d| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(text::CAPTION))
                                        .text_color(p.muted)
                                        .child(format!("<{}>", sender.email)),
                                )
                            })
                            .when(!open, |d| {
                                d.child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(message.preview.clone()))
                            }),
                    )
                    .when(open, |d| {
                        d.child(
                            div()
                                .truncate()
                                .text_size(px(text::CAPTION))
                                .text_color(p.muted)
                                .child(message.recipients.clone()),
                        )
                    }),
            )
            .child(div().flex_shrink_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(message.when.clone()));
        let card = div()
            .flex()
            .flex_col()
            .rounded(px(radius::PANE))
            .bg(p.background)
            .border(px(stroke::HAIRLINE))
            .border_color(p.line)
            .overflow_hidden()
            .child(header);
        if !open {
            return card.into_any_element();
        }
        let inset = || div().px(px(space::S16)).pb(px(space::S16));
        let invitation = message.invitation.as_ref().map(|invitation| {
            div().px(px(space::S16)).pb(px(space::S12)).child(self.render_invitation(message, invitation, p, cx))
        });
        card.children(invitation)
            .children(self.render_body(message, false, p, cx).map(|body| inset().child(body)))
            .when(message.truncated && message.html.is_none(), |d| {
                d.child(
                    inset()
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child("The message continues; open it in Mailstream to read the rest."),
                )
            })
            .when_some(message.error.as_ref(), |d, error| {
                d.child(inset().child(notice(Tone::Error, None, error.clone(), false, p)))
            })
            .when(!message.attachments.is_empty(), |d| d.child(inset().child(attachments(message, p))))
            .into_any_element()
    }

    /// A message's body: its HTML (in a chat log, only when it has no text), else its text; either
    /// way with its quoted history folded.
    fn render_body(&self, message: &Message, chat: bool, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let html = message.html.as_ref().filter(|html| !html.trim().is_empty());
        let has_text = message.text.as_ref().is_some_and(|text| !text.trim().is_empty());
        if let Some(html) = html.filter(|_| !(chat && has_text)) {
            return Some(self.render_html(message, html.clone(), p, cx));
        }
        message.text.as_ref()?;
        let open = self.quotes_open.contains(&message.id);
        Some(
            div()
                .flex()
                .flex_col()
                .text_size(px(text::READING))
                .line_height(relative(text::READING_LEADING))
                .when(!message.shown.is_empty(), |d| d.child(message.shown.clone()))
                .when_some(message.quoted.clone(), |d, quoted| {
                    d.child(self.render_fold(&message.id, open, div().text_color(p.muted).child(quoted), p, cx))
                })
                .into_any_element(),
        )
    }

    /// An HTML body. Blitz draws what the mail wrote, and its quoted history (a page of its own)
    /// once the toggle opens it; the Mac's web view, when chosen, shows the mail whole. Only the
    /// chosen renderer holds pages: the other's close at the next frame (`begin_frame`).
    fn render_html(&self, message: &Message, html: SharedString, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = message.id.clone();
        #[cfg(target_os = "macos")]
        if self.renderer == Renderer::WebKit {
            return web_body(id, html);
        }
        let main = blitz_html(id.clone(), html.clone(), blitz_body::HtmlPart::Main);
        if blitz_body::quoted(&id) != Some(true) {
            return main;
        }
        let open = self.quotes_open.contains(&id);
        let quoted = blitz_html(format!("{id}#quoted").into(), html, blitz_body::HtmlPart::Quoted);
        div().flex().flex_col().child(main).child(self.render_fold(&id, open, quoted, p, cx)).into_any_element()
    }

    /// The switch beside the pills, labelled with the renderer in use (Flutter's
    /// `ReaderRendererSwitch`): the Mac only. The open messages and quotes stay as they were.
    #[cfg(target_os = "macos")]
    fn render_renderer_switch(&self, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let segments = Renderer::ALL.map(|renderer| {
            segment(renderer.title(), None, None)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if this.renderer != renderer {
                        this.renderer = renderer;
                        cx.notify();
                    }
                }))
        });
        let switch = div()
            .flex()
            .items_center()
            .gap(px(space::S6))
            .text_size(px(text::FOOTNOTE))
            .text_color(p.muted)
            .child("Renderer:")
            .child(segmented("renderer", Renderer::ALL.iter().position(|renderer| *renderer == self.renderer)).children(segments));
        Some(switch.into_any_element())
    }

    #[cfg(not(target_os = "macos"))]
    fn render_renderer_switch(&self, _: &'static Palette, _: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    /// Folded content behind a small "more" toggle (Flutter's `_Fold`), with a rule at its left
    /// while open.
    fn render_fold(
        &self,
        message: &SharedString,
        open: bool,
        content: impl IntoElement,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = message.clone();
        let tip: SharedString = if open { "Hide quoted text" } else { "Show quoted text" }.into();
        let toggle = div()
            .id(ElementId::Name(format!("fold:{message}").into()))
            .my(px(space::S4))
            .w(px(metrics_reader::FOLD_WIDTH))
            .h(px(metrics_reader::FOLD_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(radius::SMALL))
            .bg(p.fill)
            .cursor_pointer()
            .hover(|s| s.bg(p.pressed))
            .text_size(px(text::CAPTION))
            .line_height(relative(1.))
            .font_weight(FontWeight::BOLD)
            .text_color(p.muted)
            .child("•••")
            .tooltip(tooltip(tip))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_quotes(&id, cx)));
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(toggle)
            .when(open, |d| {
                d.child(
                    div()
                        .w_full()
                        .border_l(px(metrics_reader::QUOTE_RULE))
                        .border_color(p.line)
                        .pl(px(metrics_reader::QUOTE_INDENT))
                        .child(content),
                )
            })
            .into_any_element()
    }

    /// A conversation between people as a chat log: a divider per day, each message under its
    /// sender's avatar, name and time, standing on its own with its sign-off (MS-73). The message
    /// under the pointer takes a faint wash, its reply buttons floating on its top edge.
    fn render_chat_log(&self, messages: &[Message], p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let now = Local::now();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(messages.len() * 2);
        let mut previous: Option<Option<DateTime<Local>>> = None;
        for message in messages {
            let date = message.date.map(|date| date.with_timezone(&Local));
            if let Some(date) = date
                && previous.is_none_or(|before| before.is_none_or(|before| before.date_naive() != date.date_naive()))
            {
                rows.push(day_divider(format::day_divider(date, now), p).into_any_element());
            }
            previous = Some(date);
            rows.push(self.render_log_message(message, p, cx));
        }
        div().flex().flex_col().children(rows).into_any_element()
    }

    fn render_log_message(&self, message: &Message, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = message.id.clone();
        let hovered = self.hovered_message.as_ref() == Some(&id);
        let sender = message.sender();
        let photo = avatars::avatar(&sender, metrics::AVATAR, p, cx);
        let column = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .pb(px(space::S4))
                    .flex()
                    .items_baseline()
                    .gap(px(space::S8))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(sender.display_name()))
                    .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(message.time.clone())),
            )
            .children(message.error.as_ref().map(|error| notice(Tone::Error, None, error.clone(), false, p)))
            .children(message.invitation.as_ref().map(|invitation| {
                div().pb(px(space::S8)).child(self.render_invitation(message, invitation, p, cx))
            }))
            .children(self.render_body(message, true, p, cx))
            .when(!message.attachments.is_empty(), |d| d.child(div().pt(px(space::S8)).child(attachments(message, p))));
        let actions = raised_capsule(p)
            .flex()
            .child(icon_button("log-reply", Icon::Reply, "Reply", true, p.muted).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.reply(&Reply, window, cx)),
            ))
            .child(icon_button("log-reply-all", Icon::ReplyAll, "Reply All", true, p.muted).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.reply_all(&ReplyAll, window, cx)),
            ));
        // The gap above belongs to the message, so the buttons floating over it keep it hovered.
        div()
            .id(ElementId::Name(format!("log:{id}").into()))
            .relative()
            .pt(px(space::S12))
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if *hovering {
                    this.hovered_message = Some(id.clone());
                } else if this.hovered_message.as_ref() == Some(&id) {
                    this.hovered_message = None;
                } else {
                    return;
                }
                cx.notify();
            }))
            .child(
                div()
                    .px(px(space::S12))
                    .py(px(space::S6))
                    .rounded(px(radius::CONTROL))
                    .when(hovered, |d| d.bg(p.hover))
                    .flex()
                    .items_start()
                    .child(div().w(px(metrics_reader::LOG_GUTTER)).flex_shrink_0().child(photo))
                    .child(column),
            )
            .when(hovered, |d| d.child(div().absolute().top_0().right(px(space::S12)).child(actions)))
            .into_any_element()
    }
}

/// A message's attachments, each a button that opens it in the system's app.
fn attachments(message: &Message, p: &'static Palette) -> Div {
    div().flex().flex_wrap().gap(px(space::S8)).children(message.attachments.iter().enumerate().map(|(index, attachment)| {
        let path = attachment.path.clone();
        icon_label_button(
            ElementId::Name(format!("attachment:{}:{index}", message.id).into()),
            Some(Icon::Attachment),
            format!("{}  ·  {}", attachment.filename, format::file_size(attachment.size)),
            ButtonKind::Secondary,
            ControlSize::Regular,
            true,
            p,
        )
        .on_click(move |_, _, cx| {
            if !path.is_empty() {
                cx.open_with_system(std::path::Path::new(path.as_ref()));
            }
        })
    }))
}

/// A line across the log with the day in it.
fn day_divider(label: String, p: &Palette) -> Div {
    // A half-point hairline vanishes at some offsets on a 1x screen.
    let rule = || div().flex_1().h(px(stroke::RIM)).bg(p.line);
    div()
        .py(px(space::S12))
        .flex()
        .items_center()
        .gap(px(space::S12))
        .text_size(px(text::FOOTNOTE))
        .text_color(p.muted)
        .child(rule())
        .child(label)
        .child(rule())
}

/// Blitz's tiles of `part` of an HTML body, drawn in a placeholder as tall as its page once that
/// is laid out.
fn blitz_html(key: SharedString, html: SharedString, part: blitz_body::HtmlPart) -> AnyElement {
    let height = blitz_body::height(&key).unwrap_or(40.);
    let body = canvas(
        |bounds, window, _| blitz_body::prepaint(bounds, window),
        move |bounds, hitbox, window, cx| blitz_body::paint(&key, &html, part, bounds, hitbox, window, cx),
    );
    body.w_full().h(px(height)).into_any_element()
}

/// The Mac's WebView renderer: a placeholder the native web view of the whole body is laid over,
/// as tall as its page once that is measured.
#[cfg(target_os = "macos")]
fn web_body(id: SharedString, html: SharedString) -> AnyElement {
    let height = webview::height(&id).unwrap_or(40.);
    let body = canvas(|_, _, _| {}, move |bounds, _, window, _| webview::paint(&id, &html, bounds, window));
    body.w_full().h(px(height)).into_any_element()
}
