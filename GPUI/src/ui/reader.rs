//! The reader: a conversation's messages, folded except the newest and the unread.

use super::*;

impl MailApp {
    pub(super) fn render_reader(&self, conversation: &Conversation, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match self.threads.get(&conversation.id).map(|t| &t.state) {
            None | Some(ThreadState::Loading) => {
                div().py(px(24.)).text_color(p.muted).child("Loading messages…").into_any_element()
            }
            Some(ThreadState::Failed(error)) => {
                div().py(px(24.)).text_color(p.danger).child(error.clone()).into_any_element()
            }
            Some(ThreadState::Loaded(messages)) => {
                let last = messages.len().saturating_sub(1);
                div()
                    .flex()
                    .flex_col()
                    .children(messages.iter().enumerate().map(|(index, message)| {
                        let open = index == last || message.unread || self.unfolded.contains(&message.id);
                        self.render_message(message, open, index == last, p, cx)
                    }))
                    .into_any_element()
            }
        };
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
                        .px(px(32.))
                        .pt(px(28.))
                        .pb(px(64.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            div()
                                .text_size(px(text::DISPLAY))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(conversation.title.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(text::FOOTNOTE))
                                .text_color(p.muted)
                                .when(conversation.waiting, |d| d.child(chip("Waiting on you".into(), p.waiting)))
                                .children(
                                    conversation
                                        .labels
                                        .iter()
                                        .map(|label| chip(label.clone(), design::topic_color(label, p.dark))),
                                )
                                .when(!conversation.tag.is_empty(), |d| d.child(conversation.tag.clone()))
                                .child(conversation.account.clone()),
                        )
                        .child(div().mt(px(6.)).child(body))
                        .when_some(self.reply.as_ref().filter(|r| r.conversation == conversation.id), |d, reply| {
                            d.child(div().mt(px(10.)).child(reply.view.clone()))
                        }),
                ),
            )
            .into_any_element()
    }

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
        let photo = avatars::avatar(&sender, 32., p, cx);
        let header = div()
            .id(ElementId::Name(format!("message:{id}").into()))
            .flex()
            .items_center()
            .gap(px(10.))
            .when(!last, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_fold(&id, cx)))
            })
            .child(photo)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .items_baseline()
                            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(sender.display_name()))
                            .when(sender.name.is_some(), |d| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(text::FOOTNOTE))
                                        .text_color(p.muted)
                                        .child(sender.email.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::FOOTNOTE))
                            .text_color(p.muted)
                            .child(if open { message.recipients.clone() } else { message.preview.clone() }),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(text::FOOTNOTE))
                    .text_color(p.muted)
                    .child(message.when.clone()),
            );
        let card = div().py(px(14.)).border_t_1().border_color(p.line).flex().flex_col().gap(px(12.)).child(header);
        if !open {
            return card.into_any_element();
        }
        card.when_some(message.text.as_ref(), |d, text| {
            d.child(
                div()
                    .pl(px(42.))
                    .text_size(px(text::READING))
                    .line_height(relative(1.55))
                    .child(text.clone()),
            )
        })
        .when(message.truncated, |d| {
            d.child(
                div()
                    .pl(px(42.))
                    .text_size(px(text::FOOTNOTE))
                    .text_color(p.muted)
                    .child("The message continues; open it in Mailstream to read the rest."),
            )
        })
        .when_some(message.error.as_ref(), |d, error| {
            d.child(div().pl(px(42.)).text_color(p.danger).child(error.clone()))
        })
        .when(!message.attachments.is_empty(), |d| {
            d.child(div().pl(px(42.)).flex().flex_wrap().gap(px(8.)).children(message.attachments.iter().enumerate().map(
                |(index, attachment)| {
                    let path = attachment.path.clone();
                    div()
                        .id(ElementId::Name(format!("attachment:{}:{index}", message.id).into()))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(radius::CONTROL))
                        .border_1()
                        .border_color(p.line)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(text::FOOTNOTE))
                        .child(icon(Icon::Attachment, icon_size::SMALL, p.muted))
                        .child(attachment.filename.clone())
                        .child(div().text_color(p.muted).child(format::file_size(attachment.size)))
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
