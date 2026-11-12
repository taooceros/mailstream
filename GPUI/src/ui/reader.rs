//! The reader: a conversation's messages, folded except the newest and the unread.

use super::*;

impl MailApp {
    pub(super) fn render_reader(&self, conversation: &Conversation, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match self.threads.get(&conversation.id).map(|t| &t.state) {
            None | Some(ThreadState::Loading) => {
                div().py(px(24.)).text_color(p.muted).child("Loading messages…").into_any_element()
            }
            Some(ThreadState::Failed(error)) => notice(Tone::Error, None, error.clone(), false, p).into_any_element(),
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
        div()
            .id("reader")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.reader_scroll)
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
                                }),
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
        let body = match (&message.html, &message.text) {
            (Some(html), _) => Some(inset().child(html_body(message.id.clone(), html.clone()))),
            (None, Some(text)) => Some(
                inset()
                    .text_size(px(text::READING))
                    .line_height(relative(text::READING_LEADING))
                    .child(text.clone()),
            ),
            (None, None) => None,
        };
        card.children(body)
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
            .when(!message.attachments.is_empty(), |d| {
                d.child(inset().flex().flex_wrap().gap(px(space::S8)).children(message.attachments.iter().enumerate().map(
                    |(index, attachment)| {
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
                    },
                )))
            })
            .into_any_element()
    }
}

/// An HTML body: a placeholder the native web view is laid over (the Mac) or Blitz's tiles are
/// drawn in (elsewhere), as tall as its page once that is measured.
fn html_body(id: SharedString, html: SharedString) -> AnyElement {
    let height = webview::height(&id).unwrap_or(40.);
    #[cfg(target_os = "macos")]
    let body = canvas(|_, _, _| {}, move |bounds, _, window, _| webview::paint(&id, &html, bounds, window));
    #[cfg(not(target_os = "macos"))]
    let body = canvas(
        |bounds, window, _| webview::prepaint(bounds, window),
        move |bounds, hitbox, window, cx| webview::paint(&id, &html, bounds, hitbox, window, cx),
    );
    body.w_full().h(px(height)).into_any_element()
}
