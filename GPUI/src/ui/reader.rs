//! The reader: a conversation's messages, folded except the newest and the unread, or, between
//! people, a chat log (Flutter's `Reader` and `_ChatLog`, MS-73). Quoted history and signatures
//! fold behind a "more" toggle in each body. A single mail opens at its top; a thread at the head
//! of its first unread message, else its last, held there while bodies load until it is scrolled
//! (MS-70, MS-255). The toolbar above shows how far it is scrolled (`ReaderTop`).

use gpui::{FocusHandle, Hsla, ObjectFit, Rgba, ScrollDelta, StyledImage, img, point};

use super::*;
use crate::controls::{linked_text, tooltip};
use crate::design::{AttachmentKind, log as log_metrics, marks, reader as metrics_reader};
use crate::message_log;
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
                    // Its category and topic labels open the tag editor (MS-339).
                    .child({
                        let (title, glyph, color) = match conversation.category {
                            Some(category) => (category.label(), Icon::for_category(category), design::category_color(category, p)),
                            None => ("Add Tag", Icon::Label, p.glyph),
                        };
                        let id = conversation.id.clone();
                        pill(title, Some(icon(glyph, icon_size::INLINE, color).into_any_element()), p)
                            .id("reader-category")
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.open_tag_editor(vec![id.clone()], Some(event.position()), window, cx)
                            }))
                    })
                    .children(conversation.labels.iter().enumerate().map(|(ix, label)| {
                        let id = conversation.id.clone();
                        pill(label.clone(), Some(mark_dot(design::topic_color(label, p.dark)).into_any_element()), p)
                            .id(ElementId::NamedInteger("reader-label".into(), ix as u64))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.open_tag_editor(vec![id.clone()], Some(event.position()), window, cx)
                            }))
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
            rows.push(own_presses(column().mt(px(space::S12))).child(reply.view.clone()).into_any_element());
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
        message: &ShownMessage,
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
            // The body's presses are its own (text selection, links): no drag of the reader there.
            .children(self.render_body(message, p, cx).map(|body| own_presses(inset()).child(body)))
            .when(message.truncated && message.body_html.is_none(), |d| {
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
            .when(!message.tiles.is_empty(), |d| d.child(inset().child(self.render_attachments(message, p, cx))))
            .into_any_element()
    }

    /// Who a message went to, the user as "me", on one line; a click opens its full headers
    /// (From, To, Cc with their addresses, and the date) in its place, and another closes them.
    fn render_recipients(&self, message: &ShownMessage, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
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
    fn render_attachments(&self, message: &ShownMessage, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        div().flex().flex_wrap().gap(px(space::S8)).children(message.tiles.iter().enumerate().map(|(index, tile)| {
            let attachment = &tile.attachment;
            let here = !attachment.path.is_empty();
            let color = design::attachment_color(tile.kind, p.dark);
            let thumbnail = (here && tile.kind == AttachmentKind::Image)
                .then(|| img(std::path::PathBuf::from(&attachment.path)).size_full().object_fit(ObjectFit::Cover));
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
                    None => d.child(tile.mark.clone()),
                });
            let name = SharedString::from(attachment.filename.clone());
            let (open, save, file) = (attachment.path.clone(), SharedString::from(attachment.path.clone()), name.clone());
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
                        cx.open_with_system(std::path::Path::new(&open));
                    })
                })
                .tooltip(tooltip(name.clone()))
                .child(mark)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_size(px(text::FOOTNOTE)).font_weight(FontWeight::SEMIBOLD).child(name))
                        .child(div().truncate().text_size(px(text::CAPTION)).text_color(p.muted).child(tile.detail.clone())),
                )
                .child(
                    icon_button(ElementId::Name(format!("attachment-save:{}:{index}", message.id).into()), Icon::Download, "Save…", here, p.glyph)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.save_attachment(save.clone(), file.clone(), cx))),
                )
        }))
    }

    /// A message's body: its HTML when it has one (`mailstream::Message::html_body`), else its text with its
    /// `>` quotes in place; either way with its quoted history folded.
    fn render_body(&self, message: &ShownMessage, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(html) = &message.body_html {
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
                .when(!message.shown.is_empty(), |d| d.child(text_runs(&format!("body:{}", message.id), &message.shown, p)))
                .when_some(message.quoted.clone(), |d, quoted| {
                    let content = div()
                        .text_color(p.muted)
                        .child(linked_text(ElementId::Name(format!("body-quoted:{}", message.id).into()), quoted, p));
                    d.child(self.render_fold(&message.id, open, content, p, cx))
                })
                .into_any_element(),
        )
    }

    /// An HTML body. Blitz draws what the mail wrote, and its quoted history (a page of its own)
    /// once the toggle opens it; the Mac's web view, when chosen, folds the quotes inside its
    /// page behind the same toggle. Only the chosen renderer holds pages: the other's close at
    /// the next frame (`begin_frame`).
    fn render_html(&self, message: &ShownMessage, html: SharedString, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id: SharedString = message.id.clone().into();
        let open = self.quotes_open.contains(&message.id);
        #[cfg(target_os = "macos")]
        if self.renderer == Renderer::WebKit {
            let body = web_body(id.clone(), html, open);
            if webview::quoted(&id) != Some(true) {
                return body;
            }
            return div().flex().flex_col().child(body).child(self.render_fold(&id, open, div(), p, cx)).into_any_element();
        }
        let main = blitz_html(id.clone(), html.clone(), blitz_body::HtmlPart::Main, p, &self.focus);
        if blitz_body::quoted(&id) != Some(true) {
            return main;
        }
        let quoted = blitz_html(format!("{id}#quoted").into(), html, blitz_body::HtmlPart::Quoted, p, &self.focus);
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
        message: &str,
        open: bool,
        content: impl IntoElement,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = message.to_owned();
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

    /// A conversation between people as a chat log (`message_log`, shared with Chat): a divider
    /// per day, each message under its sender's avatar, name, time and recipients, standing on its
    /// own with its sign-off (MS-73); the user's own on the right (MS-326). A sender's next message
    /// within the hour joins theirs, its time in the avatar's column on hover (`Message::group`).
    /// The message under the pointer takes a faint wash, its reply and forward buttons floating on
    /// its top edge. Returns its rows, each message's with its id, for the reader to scroll.
    fn render_chat_log(
        &self,
        messages: &[ShownMessage],
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Vec<(Option<String>, AnyElement)> {
        let mut rows = Vec::with_capacity(messages.len() * 2);
        let mut previous: Option<&ShownMessage> = None;
        for message in messages {
            if !message.divider.is_empty() {
                rows.push((None, message_log::day_divider(message.divider.clone().into(), p).into_any_element()));
            }
            // A message joining a run names its recipients only when they changed.
            let recipients = !message.continued || previous.is_none_or(|before| before.recipients != message.recipients);
            previous = Some(message);
            rows.push((Some(message.id.clone()), self.render_log_message(message, recipients, p, cx)));
        }
        rows
    }

    fn render_log_message(&self, message: &ShownMessage, recipients: bool, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = message.id.clone();
        let hovered = self.hovered_message.as_ref() == Some(&id);
        let continued = message.continued;
        let sender = message.sender();
        let lead = if continued {
            // The time shows on hover, and while the message is unread, in the accent.
            let color = if message.unread { p.accent } else if hovered { p.muted } else { gpui::transparent_black() };
            message_log::run_time(message.time.clone().into(), color, message.unread).into_any_element()
        } else {
            avatars::avatar(&sender.email, &message.avatar, log_metrics::AVATAR, p, cx)
        };
        let head = (!continued).then(|| {
            let marks = [
                message.unread.then(|| unread_mark(p).into_any_element()),
                message.starred.then(|| icon(Icon::Starred, icon_size::INLINE, p.star).self_center().into_any_element()),
            ];
            message_log::head(sender.display_name().into(), message.time.clone().into(), marks.into_iter().flatten(), message.mine, p)
        });
        let content = [
            recipients
                .then(|| self.render_recipients(message, p, cx).map(|line| div().pb(px(space::S4)).child(line).into_any_element()))
                .flatten(),
            message.error.as_ref().map(|error| notice(Tone::Error, None, error.clone(), false, p).into_any_element()),
            message.invitation.as_ref().map(|invitation| {
                div().pb(px(space::S8)).child(self.render_invitation(message, invitation, p, cx)).into_any_element()
            }),
            self.render_body(message, p, cx).map(|body| own_presses(div()).child(body).into_any_element()),
            (!message.tiles.is_empty())
                .then(|| div().pt(px(space::S8)).child(self.render_attachments(message, p, cx)).into_any_element()),
        ];
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
        let row = message_log::Row { id: ElementId::Name(format!("log:{id}").into()), mine: message.mine, continued, hovered };
        message_log::message(row, lead, head, content.into_iter().flatten(), Some(actions.into_any_element()), p)
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
            .into_any_element()
    }
}

/// The accent dot beside an unread message's sender.
fn unread_mark(p: &Palette) -> Div {
    div().flex_shrink_0().self_center().size(px(marks::DOT)).rounded_full().bg(p.accent)
}

/// A text body's runs (`format::runs`): what was written, and its quotes in place behind a rule,
/// muted; web and mail addresses in both highlight and open on click.
fn text_runs(prefix: &str, runs: &[format::Run], p: &'static Palette) -> Div {
    div().flex().flex_col().gap(px(space::S8)).children(runs.iter().enumerate().map(|(index, run)| match run {
        format::Run::Text(text) => linked_text(ElementId::Name(format!("{prefix}:{index}").into()), text.clone(), p),
        format::Run::Quote(inner) => text_runs(&format!("{prefix}:{index}"), inner, p)
            .border_l(px(metrics_reader::QUOTE_RULE))
            .border_color(p.line)
            .pl(px(metrics_reader::QUOTE_INDENT))
            .text_color(p.muted)
            .into_any_element(),
    }))
}

/// Blitz's tiles of `part` of an HTML body, drawn in a placeholder as tall as its page once that
/// is laid out. The mail sits on the reader's surface, its colours moved to it in dark mode
/// (MS-267); mail that can't move is drawn on paper, which dark mode shows as a sheet with room
/// around the mail and rounded corners, so it never meets the dark surface edge to edge (MS-256).
/// A press in it gives the window's `focus` the keys (`blitz_body::paint`).
fn blitz_html(
    key: SharedString,
    html: SharedString,
    part: blitz_body::HtmlPart,
    p: &Palette,
    focus: &FocusHandle,
) -> AnyElement {
    let height = blitz_body::height(&key).unwrap_or(40.);
    let paper = p.dark && blitz_body::paper(&key);
    let theme = html_theme(p);
    let focus = focus.clone();
    let body = canvas(
        |bounds, window, _| blitz_body::prepaint(bounds, window),
        move |bounds, hitbox, window, cx| blitz_body::paint(&key, &html, part, theme, bounds, hitbox, &focus, window, cx),
    );
    let body = body.w_full().h(px(height)).into_any_element();
    if !paper {
        return body;
    }
    div().p(px(metrics_reader::SHEET_PADDING)).rounded(px(radius::CARD)).bg(p.page).child(body).into_any_element()
}

/// The reader's surface under an HTML body and its own colours there, for Blitz.
fn html_theme(p: &Palette) -> blitz_body::HtmlTheme {
    let rgb = |color: Hsla| {
        let color = Rgba::from(color);
        let byte = |channel: f32| (channel.clamp(0., 1.) * 255.).round() as u32;
        (byte(color.r) << 16) | (byte(color.g) << 8) | byte(color.b)
    };
    blitz_body::HtmlTheme { dark: p.dark, surface: rgb(p.background), text: rgb(p.text), link: rgb(p.accent) }
}

/// The Mac's WebView renderer: a placeholder the native web view of the whole body is laid over,
/// as tall as its page once that is measured; its quoted history shows while `open`.
#[cfg(target_os = "macos")]
fn web_body(id: SharedString, html: SharedString, open: bool) -> AnyElement {
    let height = webview::height(&id).unwrap_or(40.);
    let body = canvas(|_, _, _| {}, move |bounds, _, window, _| webview::paint(&id, &html, open, bounds, window));
    body.w_full().h(px(height)).into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, InteractiveElement, Modifiers, ParentElement, Render, Styled, TestAppContext, VisualTestContext,
        Window, div, px,
    };

    use super::text_runs;
    use crate::design::{self, text};
    use crate::format;

    struct RunsHost {
        plain: Vec<format::Run>,
        body: Vec<format::Run>,
        quote: Vec<format::Run>,
        nested: Vec<format::Run>,
    }

    impl Render for RunsHost {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            let p = design::palette(window.appearance());
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(8.))
                .p(px(16.))
                .font_family(design::ui_font())
                .text_size(px(text::READING))
                .text_color(p.text)
                .child(div().debug_selector(|| "plain-run".into()).child(text_runs("plain", &self.plain, p)))
                .child(div().debug_selector(|| "body-link".into()).child(text_runs("body", &self.body, p)))
                .child(div().debug_selector(|| "quote-email".into()).child(text_runs("quote", &self.quote, p)))
                .child(div().debug_selector(|| "nested-link".into()).child(text_runs("nested", &self.nested, p)))
        }
    }

    #[gpui::test]
    fn clicking_links_in_text_and_quoted_runs_opens_allowed_targets(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::init(cx);
            design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| RunsHost {
            plain: format::runs("just plain prose, not example.com"),
            body: format::runs("https://example.com/spec"),
            quote: format::runs("> ana@example.com"),
            nested: format::runs(">> http://nested.example.com/path"),
        });
        let click_selector = |cx: &mut VisualTestContext, selector: &'static str| {
            cx.run_until_parked();
            let bounds = cx.debug_bounds(selector).expect(selector);
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
        };

        click_selector(cx, "plain-run");
        assert_eq!(cx.opened_url(), None);

        click_selector(cx, "body-link");
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/spec"));

        click_selector(cx, "quote-email");
        assert_eq!(cx.opened_url().as_deref(), Some("mailto:ana@example.com"));

        click_selector(cx, "nested-link");
        assert_eq!(cx.opened_url().as_deref(), Some("http://nested.example.com/path"));
    }
}
