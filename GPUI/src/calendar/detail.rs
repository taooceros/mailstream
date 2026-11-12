//! The open event's island (Flutter's `CalendarEventPane`): its calendar, title and when; a bar
//! answering the invitation (Going, Maybe, Decline) beside Join for its call; where; the guests
//! two to a row with their answers; its notes, their links opening. Its toolbar closes it, opens
//! its page and opens the event in the provider's web calendar. The week view shows the same
//! event in a popover beside its card instead (MS-184, `render_popover`).
//!
//! The event's page (`render_page`, MS-184) covers the whole window, sidebar and all: the same
//! event at full width, its call's address and every guest down the side, under Back to the view
//! it came from (Esc), Edit, Delete and the provider's page. The expand button of the popover or
//! the pane, a double click on the event or Return opens it.

use std::ops::Range;

use chrono::Local;
use gpui::prelude::FluentBuilder;
use gpui::component::button::Button;
use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, HighlightStyle, InteractiveElement, InteractiveText,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledText, Window, div, px, relative,
};
use linkify::{LinkFinder, LinkKind};
use mailstream::CalendarPerson;

use super::agenda::{dot_color, guest_address};
use super::model;
use super::{CalendarPane, View};
use crate::avatars;
use crate::controls::{
    ButtonKind, ControlSize, capsule, hairline, icon_button, icon_label_button, island, mark_dot, pill, popover,
    title_bar, tooltip,
};
use crate::design::{self, Palette, calendar as metrics, icon as icon_size, layout, radius, space, text};
use crate::format;
use crate::icons::{Icon, icon};

/// Where the open event is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// Beside the agenda or the day view (over it in a narrow window).
    Pane,
    /// Beside its card in the week view.
    Popover,
    /// Over the whole window.
    Page,
}

impl Surface {
    /// Keeps the controls of the surfaces drawn at once (the pane under the page) apart.
    fn name(self) -> &'static str {
        match self {
            Surface::Pane => "detail",
            Surface::Popover => "popover",
            Surface::Page => "page",
        }
    }
}

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
            .child(self.render_event(index, Surface::Pane, p, cx))
            .child(self.render_toolbar(index, two_pane, p, window, cx))
            .into_any_element()
    }

    /// The week view's open event, floating beside its card (MS-184): the pane's event under a
    /// row of its actions, on the popovers' panel, as tall as it needs up to `POPOVER_HEIGHT` or
    /// `room` (its notes and guests scrolling past that). It is drawn after the window, so it
    /// sets the pane's text style again.
    pub(super) fn render_popover(&self, index: usize, room: f32, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        popover(p)
            .debug_selector(|| "calendar-popover".into())
            .w(px(metrics::POPOVER_WIDTH))
            .max_h(px(metrics::POPOVER_HEIGHT.min(room)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .font_family(design::ui_font())
            .text_size(px(text::BODY))
            .text_color(p.text)
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(space::S8))
                    .pt(px(space::S8))
                    .flex()
                    .items_center()
                    .gap(px(space::S2))
                    .child(div().flex_1())
                    .children(self.edit_buttons(index, p, cx).into_iter().flatten())
                    .children(self.link_button(index, p))
                    .child(self.expand_button(index, p, cx))
                    .child(
                        icon_button("calendar-popover-close", Icon::Close, "Close (Esc)", true, p.glyph)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_event(cx))),
                    ),
            )
            .child(self.render_event(index, Surface::Popover, p, cx))
            .into_any_element()
    }

    /// The open event's page over the whole window (MS-184), for the window to draw over
    /// everything it holds: none unless it is open. The window's own keys reach the pane, which
    /// keeps the keyboard under it (Esc goes back).
    pub fn render_page(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.page {
            return None;
        }
        let index = self.opened.as_ref().and_then(|key| self.event_index(key))?;
        let p = design::palette(window.appearance());
        let back = match self.view {
            View::Agenda => "Agenda",
            View::Day => "Day",
            View::Week => "Week",
        };
        let toolbar = title_bar("calendar-page-toolbar", window, cx)
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(layout::HEADER_HEIGHT))
            .pl(px(space::S10 + layout::TRAFFIC_LIGHTS))
            .pr(px(space::S10))
            .flex()
            .items_center()
            .gap(px(space::S8))
            .child(
                capsule(p).child(
                    icon_label_button("calendar-page-back", Some(Icon::Back), back, ButtonKind::Plain(p.text), ControlSize::Regular, true, p)
                        .debug_selector(|| "calendar-page-back".into())
                        .tooltip(format!("Back to {back} (Esc)"))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_page(cx))),
                ),
            )
            .child(div().flex_1())
            .when_some(self.edit_buttons(index, p, cx), |d, buttons| d.child(capsule(p).children(buttons)))
            .when_some(self.link_button(index, p), |d, link| d.child(capsule(p).child(link)));
        Some(
            div()
                .id("calendar-page")
                .debug_selector(|| "calendar-page".into())
                .absolute()
                .inset_0()
                // Nothing under it takes the pointer.
                .occlude()
                .p(px(layout::GAP))
                .bg(design::window_fill(p))
                .font_family(design::ui_font())
                .text_size(px(text::BODY))
                .text_color(p.text)
                .child(
                    island(p.background, p)
                        .size_full()
                        .child(self.render_page_event(index, f32::from(window.viewport_size().width), p, cx))
                        .child(toolbar),
                )
                .into_any_element(),
        )
    }

    /// The page's event in a column `PAGE_WIDTH` at most, centered: the head across it, then
    /// where and the call beside every guest, one to a row, in a window `PAGE_SIDE_BY_SIDE` wide
    /// (under them in a narrower one), then the notes.
    fn render_page_event(&self, index: usize, width: f32, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let item = &self.events[index].item;
        let side_by_side = width >= metrics::PAGE_SIDE_BY_SIDE;
        let details = div()
            .flex()
            .flex_col()
            .when_some(item.location.clone(), |d, location| {
                d.child(detail_line(Icon::Location, linked_text("calendar-page-location", &location, p), p))
            })
            .when_some(item.meeting_url.clone(), |d, url| {
                let call = div()
                    .flex()
                    .flex_col()
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("{} call", model::meeting_label(&url))))
                    .child(div().text_size(px(text::FOOTNOTE)).child(linked_text("calendar-page-call", &url, p)));
                d.child(detail_line(Icon::Video, call.into_any_element(), p))
            });
        let guests = self.guests_section(index, if side_by_side { 1 } else { 2 }, true, p, cx);
        let notes = self.notes_section(index, Surface::Page, p);
        let body = if side_by_side {
            div()
                .flex()
                .items_start()
                .gap(px(space::S32))
                .child(div().flex_1().min_w_0().flex().flex_col().child(details).children(notes))
                .when_some(guests, |d, guests| d.child(div().flex_shrink_0().w(px(metrics::PAGE_SIDE)).child(guests)))
        } else {
            div().flex().flex_col().child(details).children(guests).children(notes)
        };
        div()
            .id("calendar-page-event")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.page_scroll)
            .pt(px(layout::HEADER_HEIGHT + space::S16))
            .px(px(space::S32))
            .pb(px(space::S32))
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(metrics::PAGE_WIDTH))
                    .flex()
                    .flex_col()
                    .child(self.event_head(index, Surface::Page, p, cx))
                    .child(body),
            )
    }

    /// Edit and Delete, for an event the account organizes on a calendar it can write.
    fn edit_buttons(&self, index: usize, p: &'static Palette, cx: &mut Context<Self>) -> Option<[Button; 2]> {
        let event = &self.events[index];
        if !event.item.editable {
            return None;
        }
        let (edit, delete) = (event.key.clone(), event.key.clone());
        Some([
            icon_button("calendar-edit", Icon::Compose, "Edit", true, p.glyph)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.edit_event(&edit, window, cx))),
            icon_button("calendar-delete", Icon::Delete, "Delete", true, p.glyph)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.confirm_delete(&delete, window, cx))),
        ])
    }

    /// Opens the event in the provider's web calendar.
    fn link_button(&self, index: usize, p: &'static Palette) -> Option<Button> {
        let event = &self.events[index];
        let link = event.item.link.clone()?;
        Some(
            icon_button("calendar-link", Icon::OpenInWindow, self.provider_page(index), true, p.glyph)
                .on_click(move |_, _, cx| cx.open_url(&link)),
        )
    }

    /// What the provider's web calendar is called.
    fn provider_page(&self, index: usize) -> &'static str {
        let microsoft = self.account_of(&self.events[index]).is_some_and(|account| account.provider == mailstream::Provider::Microsoft);
        if microsoft { "Open in Outlook" } else { "Open in Google Calendar" }
    }

    /// Opens the event's page over the window.
    fn expand_button(&self, index: usize, p: &'static Palette, cx: &mut Context<Self>) -> Button {
        let key = self.events[index].key.clone();
        icon_button("calendar-expand", Icon::Expand, "Open in Full Window (Return)", true, p.glyph)
            .debug_selector(|| "calendar-expand".into())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_page(key.clone(), cx)))
    }

    /// Floats over the top of the event: ✕ closes it beside the list, ‹ goes back to the list it
    /// covers; its page and the provider's page for it at the end.
    fn render_toolbar(
        &self,
        index: usize,
        two_pane: bool,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                icon_button("calendar-close", close_icon, close_tip, true, p.glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_event(cx))),
            ))
            .child(div().flex_1())
            .when_some(self.edit_buttons(index, p, cx), |d, buttons| d.child(capsule(p).children(buttons)))
            .child(capsule(p).children(self.link_button(index, p)).child(self.expand_button(index, p, cx)))
    }

    /// The bar under the title: the answers (or the account's part, when it cannot answer from
    /// here) and Join; none when there is neither.
    fn action_bar(&self, index: usize, surface: Surface, p: &'static Palette, cx: &mut Context<Self>) -> Option<Div> {
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
                        d.child(self.reply_switch(index, reply, surface.name(), p, cx))
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

    /// The event's calendar (and whether it repeats), title, when, and the bar answering it.
    fn event_head(&self, index: usize, surface: Surface, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let event = &self.events[index];
        let item = &event.item;
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
        let title = if surface == Surface::Page { metrics::PAGE_TITLE } else { text::DISPLAY };
        let actions = self.action_bar(index, surface, p, cx);
        div()
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
                    .text_size(px(title))
                    .line_height(px(title * 1.25))
                    .font_weight(FontWeight::BOLD)
                    .when(event.declined(), |d| d.line_through())
                    .child(item.title.clone()),
            )
            .child(detail_line(Icon::Waiting, div().text_color(p.muted).child(when).into_any_element(), p))
            .when_some(actions, |d, actions| d.child(div().pt(px(space::S4)).pb(px(14.)).child(actions)))
    }

    /// "N guests" over their answers counted, then each guest's card, `per_row` to a row, `S8`
    /// apart; the cards name each one's address under it when `addresses`. None without guests.
    fn guests_section(&self, index: usize, per_row: usize, addresses: bool, p: &'static Palette, cx: &mut Context<Self>) -> Option<Div> {
        let item = &self.events[index].item;
        let guests = model::event_guests(item);
        if guests.is_empty() {
            return None;
        }
        let count = if guests.len() == 1 { "1 guest".to_owned() } else { format!("{} guests", guests.len()) };
        Some(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .pt(px(space::S4))
                        .pb(px(space::S8))
                        .flex()
                        .items_center()
                        .gap(px(space::S8))
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(count))
                        .child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(model::guest_summary(&item.attendees))),
                )
                .child(
                    // Each card takes its share less half the gap.
                    div().flex().flex_wrap().mx(px(-space::S4)).pb(px(14.)).children(guests.iter().map(|(person, organizer)| {
                        div()
                            .w(relative(1. / per_row as f32))
                            .px(px(space::S4))
                            .pb(px(space::S8))
                            .child(guest_card(person, *organizer, addresses, p, cx))
                    })),
                ),
        )
    }

    /// The notes under a rule, their links opening; on the page, a word that they were cut
    /// short of the provider's.
    fn notes_section(&self, index: usize, surface: Surface, p: &'static Palette) -> Option<Div> {
        let item = &self.events[index].item;
        let notes = item.notes.as_deref()?;
        let cut = (surface == Surface::Page && item.notes_cut && item.link.is_some()).then(|| {
            let (_, place) = self.provider_page(index).split_at("Open in ".len());
            format!("Shortened here; the whole description is in {place}.")
        });
        Some(
            div()
                .flex()
                .flex_col()
                .child(hairline(p))
                .child(div().pt(px(14.)).pb(px(space::S6)).font_weight(FontWeight::SEMIBOLD).child("Notes"))
                .child(
                    div()
                        .line_height(px(text::BODY * 1.45))
                        .child(linked_text(format!("calendar-notes:{}", surface.name()), notes, p)),
                )
                .when_some(cut, |d, cut| {
                    d.child(div().pt(px(space::S8)).text_size(px(text::FOOTNOTE)).text_color(p.muted).child(cut))
                }),
        )
    }

    /// The event, scrolling under the pane's toolbar, or under the popover's actions and no taller
    /// than the room the popover leaves it.
    fn render_event(&self, index: usize, surface: Surface, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let item = &self.events[index].item;
        let floating = surface == Surface::Popover;
        div()
            .id(if floating { "calendar-popover-event" } else { "calendar-event" })
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .map(|d| {
                if floating {
                    d.min_h_0().flex_shrink_1().pt(px(space::S2)).px(px(space::S16)).pb(px(space::S16))
                } else {
                    d.size_full().pt(px(layout::HEADER_HEIGHT + space::S8)).px(px(space::S24)).pb(px(space::S24))
                }
            })
            .flex()
            .flex_col()
            .child(self.event_head(index, surface, p, cx))
            .when_some(item.location.clone(), |d, location| {
                d.child(detail_line(Icon::Location, linked_text(format!("calendar-location:{}", surface.name()), &location, p), p))
            })
            .children(self.guests_section(index, 2, false, p, cx))
            .children(self.notes_section(index, surface, p))
    }
}

/// A line of the event's details: `glyph` in the margin, `child` beside it.
fn detail_line(glyph: Icon, child: AnyElement, p: &Palette) -> Div {
    div()
        .pb(px(space::S10))
        .flex()
        .items_start()
        .gap(px(space::S10))
        .child(div().pt(px(1.)).child(icon(glyph, icon_size::ROW, p.muted)))
        .child(div().flex_1().min_w_0().child(child))
}

/// `text` with its web addresses and mail addresses in the links' color (as Chat's), a click on
/// one opening it in the browser or the mail app.
fn linked_text(id: impl Into<ElementId>, text: &str, p: &Palette) -> AnyElement {
    let (ranges, targets) = links(text);
    let link = HighlightStyle { color: Some(p.accent), ..HighlightStyle::default() };
    let styled = StyledText::new(SharedString::from(text.to_owned())).with_highlights(ranges.iter().map(|range| (range.clone(), link)));
    if ranges.is_empty() {
        return styled.into_any_element();
    }
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, _, cx| {
            if let Some(target) = targets.get(index) {
                cx.open_url(target);
            }
        })
        .into_any_element()
}

/// Where `text`'s web and mail addresses are, and what each opens (`mailto:` for a mail address).
fn links(text: &str) -> (Vec<Range<usize>>, Vec<String>) {
    LinkFinder::new()
        .links(text)
        .map(|link| {
            let target = match link.kind() {
                LinkKind::Email => format!("mailto:{}", link.as_str()),
                _ => link.as_str().to_owned(),
            };
            (link.start()..link.end(), target)
        })
        .unzip()
}

/// A guest on the open event: their avatar, name, and part (Organizer) and answer; the answers
/// still owed (Maybe, Invited) in the waiting color. With `address_shown` (the page), the address
/// sits under the name and the answer at the end; otherwise the answer is under the name and the
/// address shows on hover.
fn guest_card(person: &CalendarPerson, organizer: bool, address_shown: bool, p: &'static Palette, cx: &mut gpui::App) -> impl IntoElement {
    let address = guest_address(person);
    let name = person.name.clone().or_else(|| person.email.clone()).unwrap_or_else(|| "Someone".into());
    let answer = model::response_label(person.response.as_deref());
    let status: Vec<&str> =
        [organizer.then_some("Organizer"), (!answer.is_empty() && answer != "Organizer").then_some(answer)].into_iter().flatten().collect();
    let status = (!status.is_empty()).then(|| {
        let owed = matches!(person.response.as_deref(), Some("tentative" | "needsAction"));
        div()
            .truncate()
            .text_size(px(text::FOOTNOTE))
            .text_color(if owed { p.waiting } else { p.muted })
            .child(status.join(" · "))
    });
    let email = person.email.clone().filter(|email| *email != name);
    let (under, trailing, hovered) = if address_shown {
        let email = email.map(|email| div().truncate().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(email));
        (email, status.map(|status| status.flex_shrink_0()), None)
    } else {
        (status, None, email)
    };
    div()
        .id(ElementId::Name(format!("guest:{}", address.email).into()))
        .px(px(space::S8))
        .py(px(space::S6))
        .flex()
        .items_center()
        .gap(px(space::S8))
        .rounded(px(radius::CONTROL))
        .bg(p.fill)
        .child(avatars::avatar(&address.email, &mailstream::Avatar::of(&address), metrics::GUEST_AVATAR, p, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().font_weight(FontWeight::SEMIBOLD).child(name))
                .children(under),
        )
        .children(trailing)
        .when_some(hovered, |d, email| d.tooltip(tooltip(email)))
}

#[cfg(test)]
mod tests {
    use super::links;

    #[test]
    fn notes_link_their_web_and_mail_addresses() {
        let notes = "Agenda: https://docs.example.com/plan?x=1. Questions to ana@example.com, not example.com";
        let (ranges, targets) = links(notes);
        assert_eq!(targets, ["https://docs.example.com/plan?x=1", "mailto:ana@example.com"]);
        assert_eq!(&notes[ranges[0].clone()], "https://docs.example.com/plan?x=1");
        assert_eq!(&notes[ranges[1].clone()], "ana@example.com");
    }
}
