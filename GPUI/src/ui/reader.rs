//! The reader: a conversation's messages, folded except the newest and the unread, or, between
//! people, a chat log (Flutter's `Reader` and `_ChatLog`, MS-73). Quoted history and signatures
//! fold behind a "more" toggle in each body. A single mail opens at its top; a thread at the head
//! of its first unread message, else its last, held there while bodies load until it is scrolled
//! (MS-70, MS-255). The toolbar above shows how far it is scrolled (`ReaderTop`).

use gpui::{ObjectFit, ScrollDelta, StyledImage, img, point};

use super::*;
use crate::controls::tooltip;
use crate::design::{AttachmentKind, marks, reader as metrics_reader};
use crate::state::{ReaderPin, ReaderTop};

impl MailApp {
    /// The conversation scrolling under its toolbar. The scroll container's children are its rows
    /// (the title and its pills, each message, or the log's days and messages, then the reply
    /// box), so the row a thread opens at can be found in its layout.
    pub(super) fn render_reader(
        &self,
        conversation: &Conversation,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let column = || div().w_full().max_w(px(layout::READING_WIDTH)).px(px(space::S32));
        let account = self.accounts.iter().position(|a| a.id == conversation.account).map(|index| {
            let title = self.accounts[index].title();
            pill(title, Some(mark_dot(design::account_color(index)).into_any_element()), p)
        });
        let head = column()
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
            );
        let mut rows: Vec<AnyElement> = vec![head.into_any_element()];
        let pinned = match &self.reader_pin {
            ReaderPin::Message(id) => Some(id),
            _ => None,
        };
        let mut target = None;
        match self.threads.get(&conversation.id).map(|t| &t.state) {
            None | Some(ThreadState::Loading) => rows.push(
                column().mt(px(space::S24)).py(px(24.)).text_color(p.muted).child("Loading messages…").into_any_element(),
            ),
            Some(ThreadState::Failed(error)) => rows.push(
                column().mt(px(space::S24)).child(notice(Tone::Error, None, error.clone(), false, p)).into_any_element(),
            ),
            Some(ThreadState::Loaded(messages)) if conversation.chat_log() => {
                for (index, (id, row)) in self.render_chat_log(messages, p, cx).into_iter().enumerate() {
                    if pinned.is_some() && id.as_ref() == pinned {
                        target = Some(rows.len());
                    }
                    rows.push(column().when(index == 0, |d| d.mt(px(space::S24))).child(row).into_any_element());
                }
            }
            Some(ThreadState::Loaded(messages)) => {
                let last = messages.len().saturating_sub(1);
                for (index, message) in messages.iter().enumerate() {
                    if pinned == Some(&message.id) {
                        target = Some(rows.len());
                    }
                    let open = index == last || message.unread || self.unfolded.contains(&message.id);
                    let gap = if index == 0 { space::S24 } else { space::S12 };
                    rows.push(column().mt(px(gap)).child(self.render_message(message, open, index == last, p, cx)).into_any_element());
                }
            }
        }
        if let Some(reply) = self.reply.as_ref().filter(|r| r.conversation == conversation.id) {
            rows.push(column().mt(px(space::S12)).child(reply.view.clone()).into_any_element());
        }
        match self.reader_pin {
            // Taken up as the reader lays out: the end of what it holds now.
            ReaderPin::End => {
                self.reader_scroll.scroll_to_bottom();
                cx.on_next_frame(window, |this, _, cx| this.note_reader_top(cx));
            }
            ReaderPin::Message(_) => {
                if let Some(target) = target {
                    self.hold_row(target, rows.len(), window, cx);
                }
            }
            ReaderPin::Free | ReaderPin::Opening => {}
        }
        div()
            .id("reader")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.reader_scroll)
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let vertical = match event.delta {
                    ScrollDelta::Pixels(delta) => delta.y != px(0.),
                    ScrollDelta::Lines(delta) => delta.y != 0.,
                };
                if vertical {
                    this.reader_pin = ReaderPin::Free;
                    // Once the wheel's step is taken and clamped.
                    cx.on_next_frame(window, |this, _, cx| this.note_reader_top(cx));
                }
            }))
            .flex()
            .flex_col()
            .items_center()
            .pt(px(layout::HEADER_HEIGHT + space::S16))
            .pb(px(64.))
            .children(rows)
            .into_any_element()
    }

    /// Scrolls the reader so that row `row`'s top sits just under the toolbar, as the last frame
    /// laid it out. A frame that laid out other rows (the thread was loading) is followed by
    /// another, which finds them; either way the toolbar then learns where the reader is.
    fn hold_row(&self, row: usize, count: usize, window: &mut Window, cx: &mut Context<Self>) {
        let scroll = &self.reader_scroll;
        let laid_out = scroll.bounds_for_item(row).filter(|_| scroll.children_count() == count);
        if let Some(bounds) = laid_out {
            let top = bounds.top() - scroll.bounds().top() - px(layout::HEADER_HEIGHT + metrics_reader::OPEN_GAP);
            scroll.set_offset(point(px(0.), -top.max(px(0.))));
        }
        let again = laid_out.is_none();
        cx.on_next_frame(window, move |this, _, cx| {
            this.note_reader_top(cx);
            if again {
                cx.notify();
            }
        });
    }

    /// Notes how far the reader is scrolled, for the toolbar: past its top, and past its title
    /// and pills (the first row).
    fn note_reader_top(&mut self, cx: &mut Context<Self>) {
        let scroll = &self.reader_scroll;
        let scrolled = -scroll.offset().y;
        let top = if scrolled <= px(0.) {
            ReaderTop::Top
        } else if scroll
            .bounds_for_item(0)
            .is_some_and(|head| head.bottom() - scroll.bounds().top() - scrolled < px(layout::HEADER_HEIGHT))
        {
            ReaderTop::PastTitle
        } else {
            ReaderTop::Scrolled
        };
        if top != self.reader_top {
            self.reader_top = top;
            cx.notify();
        }
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
        let photo = avatars::avatar(&sender.email, &message.avatar, metrics::AVATAR, p, cx);
        let header = div()
            .id(ElementId::Name(format!("message:{id}").into()))
            .pl(px(space::S16))
            .pr(px(space::S8))
            .py(px(space::S12))
            .flex()
            // Full headers open run down from the avatar.
            .map(|d| if open && self.headers_open.contains(&message.id) { d.items_start() } else { d.items_center() })
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
                            .children(message.unread.then(|| unread_mark(p)))
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
                    .when(open, |d| d.children(self.render_recipients(message, p, cx))),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(space::S6))
                    .children(message.starred.then(|| icon(Icon::Starred, icon_size::INLINE, p.star)))
                    .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(message.when.clone())),
            );
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
            .children(self.render_body(message, p, cx).map(|body| inset().child(body)))
            .when(message.truncated && message.html_body().is_none(), |d| {
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
            .when(!message.attachments.is_empty(), |d| d.child(inset().child(self.render_attachments(message, p, cx))))
            .into_any_element()
    }

    /// Who a message went to, the user as "me", on one line; a click opens its full headers
    /// (From, To, Cc with their addresses, and the date) in its place, and another closes them.
    fn render_recipients(&self, message: &Message, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.recipients.is_empty() {
            return None;
        }
        let id = message.id.clone();
        let open = self.headers_open.contains(&id);
        let line = div()
            .id(ElementId::Name(format!("recipients:{id}").into()))
            .min_w_0()
            .text_size(px(text::CAPTION))
            .text_color(p.muted)
            .cursor_pointer()
            .tooltip(tooltip(if open { "Hide details" } else { "Show details" }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_headers(&id, cx)));
        if !open {
            return Some(line.truncate().hover(|s| s.text_color(p.text)).child(message.recipients.clone()).into_any_element());
        }
        let rows = message.headers.iter().map(|(label, value)| {
            div()
                .flex()
                .gap(px(space::S8))
                .child(div().flex_shrink_0().w(px(metrics_reader::HEADER_LABEL)).text_right().text_color(p.faint).child(*label))
                .child(div().flex_1().min_w_0().child(value.clone()))
        });
        Some(line.py(px(space::S2)).flex().flex_col().gap(px(space::S2)).children(rows).into_any_element())
    }

    /// A message's attachments as tiles of two lines (Mail's): a mark in the file kind's color, or
    /// a picture's thumbnail, beside its name over its size and type, and a button that saves a
    /// copy. A click on the tile opens the file in the system's app.
    fn render_attachments(&self, message: &Message, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        div().flex().flex_wrap().gap(px(space::S8)).children(message.attachments.iter().enumerate().map(|(index, attachment)| {
            let here = !attachment.path.is_empty();
            let color = design::attachment_color(attachment.kind, p.dark);
            let thumbnail = (here && attachment.kind == AttachmentKind::Image)
                .then(|| img(std::path::PathBuf::from(attachment.path.as_ref())).size_full().object_fit(ObjectFit::Cover));
            let mark = div()
                .flex_shrink_0()
                .size(px(metrics_reader::ATTACHMENT_MARK))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::CONTROL))
                .overflow_hidden()
                .bg(color.opacity(design::avatar::TINT))
                .text_size(px(text::TAG))
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .map(|d| match thumbnail {
                    Some(thumbnail) => d.child(thumbnail),
                    None => d.child(attachment.mark.clone()),
                });
            let (open, save, name) = (attachment.path.clone(), attachment.path.clone(), attachment.filename.clone());
            div()
                .id(ElementId::Name(format!("attachment:{}:{index}", message.id).into()))
                .min_w(px(metrics_reader::ATTACHMENT_MIN_WIDTH))
                .max_w(px(metrics_reader::ATTACHMENT_MAX_WIDTH))
                .pl(px(space::S6))
                .pr(px(space::S4))
                .py(px(space::S6))
                .flex()
                .items_center()
                .gap(px(space::S10))
                .rounded(px(radius::CARD))
                .border(px(stroke::HAIRLINE))
                .border_color(p.line)
                .when(!here, |d| d.opacity(design::chat::DIMMED))
                .when(here, |d| {
                    d.cursor_pointer().hover(|s| s.bg(p.hover)).on_click(move |_, _, cx| {
                        cx.open_with_system(std::path::Path::new(open.as_ref()));
                    })
                })
                .tooltip(tooltip(attachment.filename.clone()))
                .child(mark)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_size(px(text::FOOTNOTE)).font_weight(FontWeight::SEMIBOLD).child(attachment.filename.clone()))
                        .child(div().truncate().text_size(px(text::CAPTION)).text_color(p.muted).child(attachment.detail.clone())),
                )
                .child(
                    icon_button(ElementId::Name(format!("attachment-save:{}:{index}", message.id).into()), Icon::Download, "Save…", here, p.glyph)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.save_attachment(save.clone(), name.clone(), cx))),
                )
        }))
    }

    /// A message's body: its HTML when it has one (`Message::html_body`), else its text with its
    /// `>` quotes in place; either way with its quoted history folded.
    fn render_body(&self, message: &Message, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(html) = message.html_body() {
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
                .when(!message.shown.is_empty(), |d| d.child(text_runs(&message.shown, p)))
                .when_some(message.quoted.clone(), |d, quoted| {
                    d.child(self.render_fold(&message.id, open, div().text_color(p.muted).child(quoted), p, cx))
                })
                .into_any_element(),
        )
    }

    /// An HTML body. Blitz draws what the mail wrote, and its quoted history (a page of its own)
    /// once the toggle opens it; the Mac's web view, when chosen, folds the quotes inside its
    /// page behind the same toggle. Only the chosen renderer holds pages: the other's close at
    /// the next frame (`begin_frame`).
    fn render_html(&self, message: &Message, html: SharedString, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = message.id.clone();
        let open = self.quotes_open.contains(&id);
        #[cfg(target_os = "macos")]
        if self.renderer == Renderer::WebKit {
            let body = web_body(id.clone(), html, open);
            if webview::quoted(&id) != Some(true) {
                return body;
            }
            return div().flex().flex_col().child(body).child(self.render_fold(&id, open, div(), p, cx)).into_any_element();
        }
        let main = sheet(blitz_html(id.clone(), html.clone(), blitz_body::HtmlPart::Main), p);
        if blitz_body::quoted(&id) != Some(true) {
            return main;
        }
        let quoted = sheet(blitz_html(format!("{id}#quoted").into(), html, blitz_body::HtmlPart::Quoted), p);
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
            .child(segmented("renderer", Renderer::ALL.iter().position(|renderer| *renderer == self.renderer), p).children(segments));
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
    /// sender's avatar, name, time and recipients, standing on its own with its sign-off (MS-73);
    /// a sender's next message within the hour joins theirs, its time in the avatar's column on
    /// hover (`Message::group`). The message under the pointer takes a faint wash, its reply and
    /// forward buttons floating on its top edge. Returns its rows, each message's with its id, for
    /// the reader to scroll.
    fn render_chat_log(
        &self,
        messages: &[Message],
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Vec<(Option<SharedString>, AnyElement)> {
        let mut rows = Vec::with_capacity(messages.len() * 2);
        let mut previous: Option<&Message> = None;
        for message in messages {
            if !message.divider.is_empty() {
                rows.push((None, day_divider(message.divider.clone(), p).into_any_element()));
            }
            // A message joining a run names its recipients only when they changed.
            let recipients = !message.continued || previous.is_none_or(|before| before.recipients != message.recipients);
            previous = Some(message);
            rows.push((Some(message.id.clone()), self.render_log_message(message, recipients, p, cx)));
        }
        rows
    }

    fn render_log_message(&self, message: &Message, recipients: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = message.id.clone();
        let hovered = self.hovered_message.as_ref() == Some(&id);
        let continued = message.continued;
        let sender = message.sender();
        let gutter = if continued {
            // The time shows on hover, and while the message is unread, in the accent.
            let color = if message.unread { p.accent } else if hovered { p.muted } else { gpui::transparent_black() };
            div()
                .pt(px(space::S2))
                .text_size(px(text::TAG))
                .text_color(color)
                .when(message.unread, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(message.time.clone())
                .into_any_element()
        } else {
            avatars::avatar(&sender.email, &message.avatar, metrics::AVATAR, p, cx).into_any_element()
        };
        let head = (!continued).then(|| {
            div()
                .pb(px(space::S4))
                .flex()
                .items_baseline()
                .gap(px(space::S8))
                .min_w_0()
                .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(sender.display_name()))
                .children(message.unread.then(|| unread_mark(p)))
                .child(div().flex_shrink_0().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(message.time.clone()))
                .children(message.starred.then(|| icon(Icon::Starred, icon_size::INLINE, p.star).self_center()))
        });
        let column = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .children(head)
            .when(recipients, |d| d.children(self.render_recipients(message, p, cx).map(|line| div().pb(px(space::S4)).child(line))))
            .children(message.error.as_ref().map(|error| notice(Tone::Error, None, error.clone(), false, p)))
            .children(message.invitation.as_ref().map(|invitation| {
                div().pb(px(space::S8)).child(self.render_invitation(message, invitation, p, cx))
            }))
            .children(self.render_body(message, p, cx))
            .when(!message.attachments.is_empty(), |d| {
                d.child(div().pt(px(space::S8)).child(self.render_attachments(message, p, cx)))
            });
        // The buttons answer the message under the pointer, not the conversation's latest.
        let (reply, reply_all, forward) = (id.clone(), id.clone(), id.clone());
        let actions = raised_capsule(p)
            .flex()
            .child(icon_button("log-reply", Icon::Reply, "Reply", true, p.glyph).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.reply_to_message(reply.clone(), false, window, cx),
            )))
            .child(icon_button("log-reply-all", Icon::ReplyAll, "Reply All", true, p.glyph).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.reply_to_message(reply_all.clone(), true, window, cx),
            )))
            .child(icon_button("log-forward", Icon::Forward, "Forward", true, p.glyph).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| this.forward_message(forward.clone(), cx),
            )));
        // The gap above belongs to the message, so the buttons floating over it keep it hovered.
        div()
            .id(ElementId::Name(format!("log:{id}").into()))
            .relative()
            .pt(px(if continued { space::S2 } else { space::S12 }))
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
                    .relative()
                    .px(px(space::S12))
                    .py(px(space::S6))
                    .flex()
                    .items_start()
                    .child(div().w(px(metrics_reader::LOG_GUTTER)).flex_shrink_0().child(gutter))
                    .child(column)
                    // The wash lies over the message, so its HTML (white, as Blitz paints it)
                    // takes it too.
                    .when(hovered, |d| d.child(div().absolute().inset_0().rounded(px(radius::CONTROL)).bg(p.hover))),
            )
            .when(hovered, |d| d.child(div().absolute().top_0().right(px(space::S12)).child(actions)))
            .into_any_element()
    }
}

/// The accent dot beside an unread message's sender.
fn unread_mark(p: &Palette) -> Div {
    div().flex_shrink_0().self_center().size(px(marks::DOT)).rounded_full().bg(p.accent)
}

/// A line across the log with the day in it.
fn day_divider(label: SharedString, p: &Palette) -> Div {
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

/// A text body's runs (`format::runs`): what was written, and its quotes in place behind a rule,
/// muted.
fn text_runs(runs: &[format::Run], p: &'static Palette) -> Div {
    div().flex().flex_col().gap(px(space::S8)).children(runs.iter().map(|run| match run {
        format::Run::Text(text) => div().child(text.clone()),
        format::Run::Quote(inner) => text_runs(inner, p)
            .border_l(px(metrics_reader::QUOTE_RULE))
            .border_color(p.line)
            .pl(px(metrics_reader::QUOTE_INDENT))
            .text_color(p.muted),
    }))
}

/// An HTML page as the reader shows it: on the white Blitz paints, which dark mode makes a sheet
/// with room around the mail and rounded corners, so mail written for white never meets the dark
/// surface edge to edge (MS-256).
fn sheet(page: AnyElement, p: &Palette) -> AnyElement {
    if !p.dark {
        return page;
    }
    div().p(px(metrics_reader::SHEET_PADDING)).rounded(px(radius::CARD)).bg(p.page).child(page).into_any_element()
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
/// as tall as its page once that is measured; its quoted history shows while `open`.
#[cfg(target_os = "macos")]
fn web_body(id: SharedString, html: SharedString, open: bool) -> AnyElement {
    let height = webview::height(&id).unwrap_or(40.);
    let body = canvas(|_, _, _| {}, move |bounds, _, window, _| webview::paint(&id, &html, open, bounds, window));
    body.w_full().h(px(height)).into_any_element()
}
