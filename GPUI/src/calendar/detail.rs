//! The open event's island (Flutter's `CalendarEventPane`): its calendar, title and when; a bar
//! answering the invitation (Going, Maybe, Decline) beside Join for its call; where; the guests
//! two to a row with their answers; its notes. Its toolbar closes it and opens the event in the
//! provider's web calendar.

use chrono::Local;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, px, relative,
};
use mailstream::CalendarPerson;

use super::CalendarPane;
use super::agenda::{dot_color, guest_address};
use super::model;
use crate::avatars;
use crate::controls::{
    ButtonKind, ControlSize, capsule, hairline, icon_button, icon_label_button, island, mark_dot, pill, title_bar,
    tooltip,
};
use crate::design::{Palette, calendar as metrics, icon as icon_size, layout, radius, space, text};
use crate::format;
use crate::icons::{Icon, icon};

impl CalendarPane {
    pub(super) fn render_detail(
        &self,
        index: usize,
        two_pane: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let event = &self.events[index];
        island(p.background, p)
            .id(ElementId::Name(format!("calendar-detail:{}:{}", event.key.account, event.key.id).into()))
            .size_full()
            // The event keeps the keyboard; clicks never reach the list beneath.
            .occlude()
            .child(self.render_event(index, p, cx))
            .child(self.render_toolbar(index, two_pane, p, window, cx))
            .into_any_element()
    }

    /// Floats over the top of the event: ✕ closes it beside the list, ‹ goes back to the list it
    /// covers; the provider's page for it at the end.
    fn render_toolbar(
        &self,
        index: usize,
        two_pane: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let event = &self.events[index];
        let microsoft = self.account_of(event).is_some_and(|account| account.provider == "microsoft");
        let (close_icon, close_tip) = if two_pane { (Icon::Close, "Close (Esc)") } else { (Icon::Back, "Back (Esc)") };
        title_bar("calendar-detail-toolbar", window, cx)
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(layout::HEADER_HEIGHT))
            .px(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(capsule(p).child(
                icon_button("calendar-close", close_icon, close_tip, true, p.muted)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_event(cx))),
            ))
            .child(div().flex_1())
            .when_some(event.item.link.clone(), |d, link| {
                let tip = if microsoft { "Open in Outlook" } else { "Open in Google Calendar" };
                d.child(capsule(p).child(
                    icon_button("calendar-link", Icon::OpenInWindow, tip, true, p.muted)
                        .on_click(move |_, _, cx| cx.open_url(&link)),
                ))
            })
    }

    /// The bar under the title: the answers (or the account's part, when it cannot answer from
    /// here) and Join; none when there is neither.
    fn action_bar(&self, index: usize, p: &'static Palette, cx: &mut Context<Self>) -> Option<Div> {
        let event = &self.events[index];
        let reply = self.reply(event);
        let url = event.item.meeting_url.clone();
        let part = match event.item.response.as_deref() {
            Some("organizer") => "You organize this",
            Some(response) if !reply.answerable => model::response_label(Some(response)),
            _ => "",
        };
        if !reply.answerable && part.is_empty() && url.is_none() {
            return None;
        }
        Some(
            div()
                .p(px(space::S6))
                .flex()
                .items_center()
                .rounded(px(metrics::CARD_RADIUS + 2.))
                .bg(p.fill)
                .map(|d| {
                    if reply.answerable {
                        d.child(self.reply_switch(index, reply, "detail", p, cx))
                    } else if !part.is_empty() {
                        d.child(div().px(px(space::S6)).text_color(p.muted).font_weight(FontWeight::MEDIUM).child(part))
                    } else {
                        d
                    }
                })
                .child(div().flex_1())
                .when_some(url, |d, url| {
                    d.child(
                        icon_label_button("calendar-join", Some(Icon::VideoFill), "Join", ButtonKind::Primary, ControlSize::Regular, true, p)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }),
        )
    }

    /// The event, scrolling under the toolbar.
    fn render_event(&self, index: usize, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let event = &self.events[index];
        let item = &event.item;
        let line = |glyph: Icon, child: AnyElement| {
            div()
                .pb(px(space::S10))
                .flex()
                .items_start()
                .gap(px(space::S10))
                .child(div().pt(px(1.)).child(icon(glyph, icon_size::ROW, p.muted)))
                .child(div().flex_1().min_w_0().child(child))
        };
        let calendar_label = {
            let name = item.calendar_name.clone().unwrap_or_else(|| item.calendar.clone());
            if item.calendar_name.as_deref() == Some(item.account.as_str()) { name } else { format!("{name} · {}", item.account) }
        };
        let when = format::meeting_when(
            item.all_day,
            (item.start_day.as_deref(), item.end_day.as_deref()),
            &item.start,
            &item.end,
            Local::now(),
        );
        let guests = model::event_guests(item);
        let actions = self.action_bar(index, p, cx);
        div()
            .id("calendar-event")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .pt(px(layout::HEADER_HEIGHT + space::S8))
            .px(px(space::S24))
            .pb(px(space::S24))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(space::S6))
                    .child(pill(calendar_label, Some(mark_dot(dot_color(self.event_color(event), p)).into_any_element()), p))
                    .when(item.series.is_some(), |d| d.child(pill("Repeats", None, p))),
            )
            .child(
                div()
                    .pt(px(space::S10))
                    .pb(px(space::S6))
                    .text_size(px(text::DISPLAY))
                    .line_height(px(text::DISPLAY * 1.25))
                    .font_weight(FontWeight::BOLD)
                    .when(event.declined(), |d| d.line_through())
                    .child(item.title.clone()),
            )
            .child(line(Icon::Waiting, div().text_color(p.muted).child(when).into_any_element()))
            .when_some(actions, |d, actions| d.child(div().pt(px(space::S4)).pb(px(14.)).child(actions)))
            .when_some(item.location.clone(), |d, location| {
                d.child(line(Icon::Location, div().child(location).into_any_element()))
            })
            .when(!guests.is_empty(), |d| {
                let count = if guests.len() == 1 { "1 guest".to_owned() } else { format!("{} guests", guests.len()) };
                d.child(
                    div()
                        .pt(px(space::S4))
                        .pb(px(space::S8))
                        .flex()
                        .items_center()
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(count))
                        .child(
                            div()
                                .text_size(px(text::FOOTNOTE))
                                .text_color(p.muted)
                                .child(model::guest_summary(&item.attendees)),
                        ),
                )
                .child(
                    // Two to a row, `S8` apart: each card takes half less half the gap.
                    div().flex().flex_wrap().mx(px(-space::S4)).pb(px(14.)).children(guests.iter().map(|(person, organizer)| {
                        div().w(relative(0.5)).px(px(space::S4)).pb(px(space::S8)).child(guest_card(person, *organizer, p, cx))
                    })),
                )
            })
            .when_some(item.notes.clone(), |d, notes| {
                d.child(hairline(p))
                    .child(div().pt(px(14.)).pb(px(space::S6)).font_weight(FontWeight::SEMIBOLD).child("Notes"))
                    .child(div().line_height(px(text::BODY * 1.45)).child(notes))
            })
    }
}

/// A guest on the open event: their avatar, name, and part (Organizer) and answer; the answers
/// still owed (Maybe, Invited) in the waiting color. The address shows on hover.
fn guest_card(person: &CalendarPerson, organizer: bool, p: &'static Palette, cx: &mut gpui::App) -> impl IntoElement {
    let address = guest_address(person);
    let name = person.name.clone().or_else(|| person.email.clone()).unwrap_or_else(|| "Someone".into());
    let answer = model::response_label(person.response.as_deref());
    let status: Vec<&str> =
        [organizer.then_some("Organizer"), (!answer.is_empty() && answer != "Organizer").then_some(answer)].into_iter().flatten().collect();
    let owed = matches!(person.response.as_deref(), Some("tentative" | "needsAction"));
    let email = person.email.clone().filter(|email| *email != name);
    div()
        .id(ElementId::Name(format!("guest:{}", address.email).into()))
        .px(px(space::S8))
        .py(px(space::S6))
        .flex()
        .items_center()
        .gap(px(space::S8))
        .rounded(px(radius::CONTROL))
        .bg(p.fill)
        .child(avatars::avatar(&address, metrics::GUEST_AVATAR, p, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().font_weight(FontWeight::SEMIBOLD).child(name))
                .when(!status.is_empty(), |d| {
                    d.child(
                        div()
                            .truncate()
                            .text_size(px(text::FOOTNOTE))
                            .text_color(if owed { p.waiting } else { p.muted })
                            .child(status.join(" · ")),
                    )
                }),
        )
        .when_some(email, |d, email| {
            d.tooltip(tooltip(email))
        })
}
