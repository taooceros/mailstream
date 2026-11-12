//! The day and week views (Flutter's `CalendarGrid`, MS-212): days side by side on a 24-hour
//! timeline under their dates (today's raised, the others over a bar of the hours they book) and
//! the all-day row; the timed events as cards placed side by side where they overlap, today's
//! column lit, and the time now as a red line with its time in the gutter. Days the engine does
//! not fetch are shaded and named over the dates.
//!
//! The days page as one: a two-finger sideways swipe drags them 1:1, the gutter's hours staying
//! put, and past a third of the width or flicked they go on to the next page, else back; ← and →
//! slide the next page in. The pages either side are drawn only while they move.

use std::time::Instant;

use chrono::{DateTime, Days, Local, NaiveDate, Timelike};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, ClickEvent, Context, DispatchPhase, Div, ElementId, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    IntoElement, ParentElement, ScrollWheelEvent, StatefulInteractiveElement, Styled, StyledText, Window, canvas, div,
    linear_color_stop, linear_gradient, point, px,
};

use super::agenda::{dot_color, guest_avatars};
use super::model::{self, AllDayPlacement, Event, Look, Placement};
use super::{CalendarPane, Pan, View};
use crate::controls::{Tone, mark_dot, notice, tag};
use crate::design::{self, Palette, calendar as metrics, motion, space, stroke, text};
use crate::icons::{Icon, icon};
use crate::swipe::{self, Step};

/// Minutes in a day by the clock.
const DAY_MINUTES: f32 = 24.0 * 60.0;

/// The timeline's height: the hours and the room over the first line and under the last.
const TIMELINE: f32 = 24.0 * metrics::HOUR_HEIGHT + 2.0 * metrics::INSET;

/// Points per minute on the timeline.
const PER_MINUTE: f32 = metrics::HOUR_HEIGHT / 60.0;

/// How much of a card's content its stretch of time has room for: one line (title, start and
/// length), the title and time, or those over the call and the guests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fit {
    Line,
    Standard,
    Full,
}

fn fit(minutes: i64) -> Fit {
    if minutes < 45 {
        Fit::Line
    } else if minutes < 75 {
        Fit::Standard
    } else {
        Fit::Full
    }
}

/// One page's days laid out.
struct Page {
    days: Vec<NaiveDate>,
    timed: Vec<Vec<Placement>>,
    conflicts: Vec<Vec<bool>>,
    booked: Vec<i64>,
    all_day: Vec<AllDayPlacement>,
    rows: usize,
}

impl Page {
    fn new(events: &[Event], days: Vec<NaiveDate>) -> Page {
        let start = model::midnight(days[0]);
        let end = model::midnight(model::next_day(days[days.len() - 1]));
        let candidates: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                if event.end > event.start { event.end > start && event.start < end } else { event.start >= start && event.start < end }
            })
            .map(|(index, _)| index)
            .collect();
        let timed: Vec<Vec<Placement>> =
            days.iter().map(|&day| model::layout_day(events, candidates.iter().copied(), day)).collect();
        let conflicts = timed.iter().map(|placed| model::day_conflicts(events, placed)).collect();
        let booked = timed.iter().map(|placed| model::booked_minutes(events, placed)).collect();
        let (all_day, rows) = model::layout_all_day(events, candidates.iter().copied(), &days);
        Page { days, timed, conflicts, booked, all_day, rows }
    }

    fn shown_rows(&self) -> usize {
        self.rows.min(metrics::ALL_DAY_ROWS)
    }
}

/// A day's weekday and date (`CalendarDayHeader`): today's raised on a small card in the accent,
/// the other days flat over a bar as long as the hours they book.
pub(super) fn day_header(day: NaiveDate, today: bool, booked: i64, p: &'static Palette) -> Div {
    let booked = booked as f32;
    let load = if today || booked <= 0. { 0. } else { (booked / metrics::FULL_DAY_MINUTES).clamp(0.15, 1.) };
    let date = div()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .text_size(px(text::CAPTION))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if today { p.accent } else { p.muted })
                .child(day.format("%a").to_string()),
        )
        .child(
            div()
                .text_size(px(text::BODY))
                .line_height(px(16.))
                .font_weight(if today { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
                .text_color(if today { p.accent } else { p.text })
                .child(day.format("%-d").to_string()),
        )
        .child(
            div()
                .mt(px(3.))
                .h(px(metrics::LOAD_BAR))
                .w(px(metrics::LOAD_WIDTH * load))
                .rounded_full()
                .bg(design::tint(p.accent, metrics::LOAD_OPACITY)),
        );
    let cell = div().h_full().flex().items_center().justify_center().rounded(px(metrics::CARD_RADIUS));
    if !today {
        return cell.child(date);
    }
    cell.child(
        div()
            .px(px(space::S10))
            .py(px(space::S2))
            .rounded(px(metrics::CARD_RADIUS))
            .bg(p.raised_card)
            .border(px(stroke::HAIRLINE))
            .border_color(p.card_border)
            .shadow(p.card_shadow.clone())
            .child(date),
    )
}

impl CalendarPane {
    /// The days the page `offset` pages from the one shown covers.
    fn page_days(&self, offset: i64) -> Vec<NaiveDate> {
        let shift = |day: NaiveDate, days: i64| {
            if days >= 0 { day + Days::new(days as u64) } else { day - Days::new(days.unsigned_abs()) }
        };
        match self.view {
            View::Week => model::week_days(shift(model::week_of(self.focus_day), 7 * offset)).to_vec(),
            _ => vec![shift(self.focus_day, offset)],
        }
    }

    /// The day view's next day or the week view's next week (`step` 1), or the one before (-1);
    /// the new page slides in from that side.
    pub(super) fn page(&mut self, step: i64, cx: &mut Context<Self>) {
        if self.view == View::Agenda {
            return;
        }
        self.move_focus(step);
        let width = self.page_width.get();
        self.pan = Pan::Settle { from: step as f32 * width, start: Instant::now() };
        cx.notify();
    }

    fn move_focus(&mut self, step: i64) {
        let days = if self.view == View::Week { 7 * step } else { step };
        self.focus_day = if days >= 0 { self.focus_day + Days::new(days as u64) } else { self.focus_day - Days::new(days.unsigned_abs()) };
    }

    /// Where the pages sit now, called while rendering: advances a slide and asks for the next
    /// frame while it moves.
    fn current_pan(&mut self, window: &mut Window) -> f32 {
        match self.pan {
            Pan::Rest => 0.,
            Pan::Drag(offset) => offset,
            Pan::Settle { from, start } => {
                let progress = (start.elapsed().as_secs_f32() / motion::PAGE.as_secs_f32()).min(1.);
                if progress < 1. {
                    window.request_animation_frame();
                    from * (1. - design::house_curve(progress))
                } else {
                    self.pan = Pan::Rest;
                    0.
                }
            }
        }
    }

    /// A scroll event over the grid, in the capture phase: true when the page swipe took it.
    fn feed_swipe(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        let width = self.page_width.get().max(1.);
        match self.swipe.feed(event, self.view != View::Agenda) {
            Step::Pass => false,
            Step::Consume => true,
            Step::Drag(offset) => {
                self.pan = Pan::Drag(offset.clamp(-width, width));
                cx.notify();
                true
            }
            Step::Release { offset, velocity } => {
                let (onward, _) = swipe::settle(offset, velocity, width);
                let from = if onward {
                    // Dragged left: the next page; right: the one before. It goes on from where
                    // the fingers left it.
                    let step = if offset < 0. { 1 } else { -1 };
                    self.move_focus(step);
                    offset + step as f32 * width
                } else {
                    offset
                };
                self.pan = Pan::Settle { from, start: Instant::now() };
                cx.notify();
                true
            }
        }
    }

    /// The timeline scrolled to put `minutes` (from midnight) an hour and a half from its top.
    pub(super) fn scroll_timeline_to(&self, minutes: f32) {
        let viewport = f32::from(self.timeline.bounds().size.height);
        let top = ((minutes / 60. - 1.5) * metrics::HOUR_HEIGHT).clamp(0., (TIMELINE - viewport).max(0.));
        self.timeline.set_offset(point(px(0.), px(-top)));
    }

    /// Brings the timed card of `event` on `day` into view when it is off the visible hours.
    pub(super) fn reveal_on_timeline(&self, event: &Event, day: NaiveDate) {
        if event.in_all_day_row() {
            return;
        }
        let midnight = model::midnight(day);
        let start = if event.start < midnight { 0. } else { (event.start.hour() * 60 + event.start.minute()) as f32 };
        let end = if event.end >= model::midnight(model::next_day(day)) {
            DAY_MINUTES
        } else {
            (event.end.hour() * 60 + event.end.minute()) as f32
        };
        let (top, bottom) = (metrics::INSET + start * PER_MINUTE, metrics::INSET + end.max(start + 20.) * PER_MINUTE);
        let viewport = f32::from(self.timeline.bounds().size.height);
        let scrolled = -f32::from(self.timeline.offset().y);
        let margin = metrics::HOUR_HEIGHT / 2.;
        let max = (TIMELINE - viewport).max(0.);
        if top < scrolled {
            self.timeline.set_offset(point(px(0.), px(-(top - margin).clamp(0., max))));
        } else if bottom > scrolled + viewport {
            let to = (top - margin).max(bottom + margin - viewport).clamp(0., max);
            self.timeline.set_offset(point(px(0.), px(-to)));
        }
    }

    /// The day or week view: the synced note, the dates, the all-day row and the timeline, its
    /// pages under the fingers or sliding.
    pub(super) fn render_grid(
        &mut self,
        today: NaiveDate,
        now: DateTime<Local>,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let pan = self.current_pan(window);
        let width = self.page_width.get();
        let offsets: &[i64] = if pan == 0. { &[0] } else { &[-1, 0, 1] };
        let pages: Vec<(i64, Page)> =
            offsets.iter().map(|&offset| (offset, Page::new(&self.events, self.page_days(offset)))).collect();
        let current = &pages.iter().find(|(offset, _)| *offset == 0).expect("the page shown").1;
        let shows_today = current.days.contains(&today);
        if std::mem::take(&mut self.scroll_to_now) {
            self.scroll_timeline_to((now.hour() * 60 + now.minute()) as f32);
        }
        // Each page `width` wide, `offset` pages from the shown one; before the first layout,
        // the shown one fills the band.
        let place = move |offset: i64, d: Div| {
            let d = d.absolute().top_0().bottom_0().left(px(offset as f32 * width + pan));
            if width > 0. { d.w(px(width)) } else { d.w_full() }
        };
        let unsynced = self.synced.filter(|(from, until)| current.days.iter().any(|day| day < from || day >= until));

        let header = div().flex_shrink_0().h(px(metrics::HEADER_HEIGHT)).flex().child(div().flex_shrink_0().w(px(metrics::GUTTER))).child(
            div().flex_1().h_full().relative().overflow_hidden().children(pages.iter().map(|(offset, page)| {
                place(
                    *offset,
                    div().flex().children(page.days.iter().zip(&page.booked).map(|(&day, &booked)| {
                        let week = self.view == View::Week;
                        div()
                            .id(ElementId::Name(format!("grid-day:{day}").into()))
                            .flex_1()
                            .h_full()
                            .when(week, |d| {
                                d.cursor_pointer()
                                    .rounded(px(metrics::CARD_RADIUS))
                                    .hover(|s| s.bg(p.hover))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.show_day_view(day, cx)))
                            })
                            .child(day_header(day, day == today, booked, p))
                    })),
                )
            })),
        );

        let rows = current.shown_rows();
        let all_day = (rows > 0).then(|| {
            let height = rows as f32 * metrics::ALL_DAY_HEIGHT + 4.;
            div()
                .flex_shrink_0()
                .h(px(height))
                .flex()
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(metrics::GUTTER - space::S6))
                        .mr(px(space::S6))
                        .pt(px(space::S4))
                        .text_right()
                        .text_size(px(text::CAPTION))
                        .text_color(p.muted)
                        .child("all-day"),
                )
                .child(
                    div().flex_1().h_full().relative().overflow_hidden().children(pages.iter().map(|(offset, page)| {
                        let bars = self.render_all_day_bars(page, width, now, p, cx);
                        place(
                            *offset,
                            div().child(
                                div()
                                    .id(ElementId::Name(format!("all-day:{}", page.days[0]).into()))
                                    .size_full()
                                    .overflow_y_scroll()
                                    .child(
                                        div()
                                            .relative()
                                            .w_full()
                                            .h(px(page.rows as f32 * metrics::ALL_DAY_HEIGHT + 4.))
                                            .children(bars),
                                    ),
                            ),
                        )
                    })),
                )
        });

        // The current time in the gutter: a pill over the hour label it would collide with.
        let minutes = (now.hour() * 60 + now.minute()) as f32;
        let now_y = metrics::INSET + minutes * PER_MINUTE;
        let covered_hour = shows_today.then(|| (minutes / 60.).round() as u32).filter(|&hour| {
            (metrics::INSET + hour as f32 * metrics::HOUR_HEIGHT - now_y).abs() < metrics::NOW_PILL
        });
        let gutter = div()
            .absolute()
            .left_0()
            .top_0()
            .w(px(metrics::GUTTER))
            .h_full()
            .children((0..=24u32).map(|hour| {
                let y = metrics::INSET + hour as f32 * metrics::HOUR_HEIGHT;
                div()
                    .absolute()
                    .left(px(metrics::GUTTER - 4.))
                    .top(px(y))
                    .w(px(4.))
                    .h(px(1.))
                    .bg(p.line)
            }))
            .children((1..24u32).filter(|&hour| covered_hour != Some(hour)).map(|hour| {
                let y = metrics::INSET + hour as f32 * metrics::HOUR_HEIGHT;
                let label = match hour {
                    12 => "12 PM".to_owned(),
                    hour if hour < 12 => format!("{hour} AM"),
                    hour => format!("{} PM", hour - 12),
                };
                div()
                    .absolute()
                    .right(px(space::S8))
                    .top(px(y - 7.))
                    .h(px(14.))
                    .text_size(px(text::CAPTION))
                    .line_height(px(14.))
                    .text_color(p.muted)
                    .child(label)
            }))
            .when(shows_today, |d| {
                d.child(
                    div()
                        .absolute()
                        .right(px(3.))
                        .top(px(now_y - metrics::NOW_PILL / 2.))
                        .h(px(metrics::NOW_PILL))
                        .px(px(5.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(p.danger)
                        .text_size(px(text::CAPTION))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(gpui::white())
                        .child(now.format("%-I:%M").to_string()),
                )
            });

        // The pages' width, measured as laid out: a new width (the window, the open event
        // narrowing the list) draws the pane again at once.
        let measure = {
            let cell = self.page_width.clone();
            let pane = cx.entity().downgrade();
            canvas(
                move |bounds, _, cx| {
                    let measured = f32::from(bounds.size.width);
                    if (cell.get() - measured).abs() > 0.5 {
                        cell.set(measured);
                        pane.update(cx, |_, cx| cx.notify()).ok();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };
        let timeline = div()
            .id("calendar-timeline")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.timeline)
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(TIMELINE))
                    .child(gutter)
                    .child(
                        div()
                            .absolute()
                            .left(px(metrics::GUTTER))
                            .right_0()
                            .top_0()
                            .bottom_0()
                            .overflow_hidden()
                            .child(measure)
                            .children(pages.iter().map(|(offset, page)| {
                                place(*offset, self.render_timeline(page, today, now, width, p, cx))
                            })),
                    ),
            );

        // Two-finger paging: scroll events are seen in the capture phase, before the timeline
        // scrolls with them, so a sideways swipe is never eaten by it.
        let entity = cx.entity();
        let swipe = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx: &mut App| {
                    if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                        return;
                    }
                    if entity.update(cx, |this, cx| this.feed_swipe(event, cx)) {
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .size_full();

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(swipe)
            .when_some(unsynced, |d, (from, until)| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(18.))
                        .pb(px(space::S6))
                        .text_size(px(text::FOOTNOTE))
                        .text_color(p.muted)
                        .child(model::synced_note(from, until)),
                )
            })
            .child(header)
            .children(all_day)
            .child(div().flex_shrink_0().h(px(stroke::HAIRLINE)).bg(p.line))
            .child(timeline)
    }

    /// One page of the timeline: today's column lit, unfetched days shaded, the hour lines and
    /// the lines between days, the cards, and the time now.
    fn render_timeline(
        &self,
        page: &Page,
        today: NaiveDate,
        now: DateTime<Local>,
        width: f32,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let count = page.days.len();
        let column = width / count as f32;
        let today_at = page.days.iter().position(|&day| day == today);
        let shade = design::tint(p.text, if p.dark { 0.05 } else { 0.035 });
        let unfetched: Vec<usize> = match self.synced {
            Some((from, until)) => (0..count).filter(|&i| page.days[i] < from || page.days[i] >= until).collect(),
            None => Vec::new(),
        };
        let mut d = div()
            .when_some(today_at, |d, at| {
                d.child(div().absolute().top_0().bottom_0().left(px(at as f32 * column)).w(px(column)).bg(p.spotlight))
            })
            .children(
                unfetched
                    .into_iter()
                    .map(|i| div().absolute().top_0().bottom_0().left(px(i as f32 * column)).w(px(column)).bg(shade)),
            )
            .children((0..=24).map(|hour| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(metrics::INSET + hour as f32 * metrics::HOUR_HEIGHT))
                    .h(px(1.))
                    .bg(p.line)
            }))
            .children((0..count).map(|i| div().absolute().top_0().bottom_0().left(px(i as f32 * column)).w(px(1.)).bg(p.line)));
        if width > 0. {
            let gap = metrics::CONFLICT_GAP;
            for (i, (placed, conflicts)) in page.timed.iter().zip(&page.conflicts).enumerate() {
                // Half a gap from the day's line on the left, a whole one before the next day's.
                let room = column - 1.5 * gap;
                for (placement, &conflict) in placed.iter().zip(conflicts) {
                    let share = room / placement.columns as f32;
                    let trailing = if placement.column + placement.span < placement.columns { gap } else { 0. };
                    let card_width = share * placement.span as f32 - trailing;
                    let card_height = (placement.bottom - placement.top) as f32 * PER_MINUTE - gap / 2.;
                    let left = i as f32 * column + gap / 2. + share * placement.column as f32;
                    let top = metrics::INSET + placement.top as f32 * PER_MINUTE;
                    d = d.child(self.render_card(placement, conflict, (left, top, card_width, card_height), now, p, cx));
                }
            }
        }
        // The time now: a line with a dot across today's column, dashed across the other days.
        if let Some(at) = today_at {
            let y = metrics::INSET + (now.hour() * 60 + now.minute()) as f32 * PER_MINUTE;
            let left = at as f32 * column;
            let trail = design::tint(p.danger, metrics::NOW_TRAIL_OPACITY);
            // Dashes `DASH` long, as far apart, from `from` to `to`.
            let dashed = |from: f32, to: f32| {
                let count = ((to - from).max(0.) / (2. * metrics::DASH)).ceil() as usize;
                (0..count).map(move |i| {
                    let x = from + i as f32 * 2. * metrics::DASH;
                    div().absolute().top(px(y - 0.5)).left(px(x)).w(px(metrics::DASH.min(to - x))).h(px(1.)).bg(trail)
                })
            };
            d = d
                .children(dashed(0., left))
                .children(dashed(left + column, width))
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .w(px(column))
                        .top(px(y - metrics::NOW_LINE / 2.))
                        .h(px(metrics::NOW_LINE))
                        .bg(p.danger),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left - 3.))
                        .top(px(y - 3.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(p.danger),
                );
        }
        d
    }

    /// An event on the timeline as a card on the tray (MS-163): its calendar's color only in a
    /// dot before the title, the call as a quiet tag and the guests at its foot when there is
    /// room. Open, it lifts as the lists' rows do; under the pointer half as far; clashing with
    /// another the account goes to, a red dot; waiting for an answer, a tint of its calendar in
    /// a dashed border; over or declined, flat on the tray and faded.
    fn render_card(
        &self,
        placement: &Placement,
        conflict: bool,
        (left, top, width, height): (f32, f32, f32, f32),
        now: DateTime<Local>,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let event = &self.events[placement.event];
        let item = &event.item;
        let look = model::look(event, now);
        let flat = matches!(look, Look::Past | Look::Declined);
        let fade = match look {
            Look::Past => metrics::PAST_OPACITY,
            Look::Declined => metrics::DECLINED_OPACITY,
            _ => 1.,
        };
        let selected = self.opened.as_ref() == Some(&event.key);
        let color = self.event_color(event);
        let fit = fit(placement.bottom - placement.top);
        let ink = design::tint(p.text, fade);
        let soft = design::tint(p.muted, fade);
        let meeting = if flat { None } else { item.meeting_url.clone() };
        let dot = || mark_dot(design::tint(dot_color(color, p), fade));
        let answer = match item.response.as_deref() {
            Some("accepted") => Some("Going"),
            Some("tentative") => Some("Maybe"),
            Some("needsAction") => Some("Invited"),
            _ => None,
        }
        .filter(|_| fit != Fit::Line && width >= 150. && !flat);
        let trailing = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::S4))
            .when_some(answer, |d, answer| d.child(tag(answer, None, p).h(px(16.))))
            .when(fit == Fit::Line && meeting.is_some() && width >= 90., |d| d.child(icon(Icon::Video, 13., p.muted)))
            .when(conflict && look == Look::Upcoming, |d| d.child(mark_dot(p.danger)));
        let title = |lines: usize| {
            div()
                .min_w_0()
                .text_size(px(text::FOOTNOTE))
                .line_height(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ink)
                .when(look == Look::Declined, |d| d.line_through())
                .when(lines <= 1, |d| d.truncate())
                .when(lines > 1, |d| d.line_clamp(lines))
                .child(item.title.clone())
        };
        let content = if width < 36. {
            // A sliver beside its neighbors: the dot alone says something is there.
            div().pl(px(5.)).pt(px(5.)).child(dot())
        } else if fit == Fit::Line {
            // One line cut at its end: the title first, then when and how long, quieter.
            let line = format!(
                "{}  {} · {}",
                item.title,
                model::clock(event.start),
                model::short_duration((event.end - event.start).num_minutes())
            );
            let quiet = item.title.len()..line.len();
            let detail = HighlightStyle { color: Some(soft), font_weight: Some(FontWeight::NORMAL), ..HighlightStyle::default() };
            div()
                .size_full()
                .px(px(space::S6))
                .flex()
                .items_center()
                .gap(px(5.))
                .child(dot())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(text::FOOTNOTE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ink)
                        .when(look == Look::Declined, |d| d.line_through())
                        .child(StyledText::new(line).with_highlights([(quiet, detail)])),
                )
                .child(trailing)
        } else {
            let when = if width < 100. { model::clock(event.start) } else { model::time_range(event.start, event.end) };
            let detail = match &item.location {
                Some(location) => format!("{when} · {location}"),
                None => when,
            };
            let guests = if flat { Vec::new() } else { model::others(item) };
            let foot = fit == Fit::Full && width >= 50. && (meeting.is_some() || !guests.is_empty());
            let room = height - 7. - 14. - if foot { metrics::AVATAR + 2. } else { 0. };
            let lines = ((room / 15.).floor() as usize).clamp(1, 3);
            let face = if selected { p.lifted_card[1] } else { p.background };
            div()
                .relative()
                .size_full()
                .pl(px(space::S6))
                .pr(px(5.))
                .pt(px(3.))
                .pb(px(space::S4))
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(5.))
                        .child(div().pt(px((15. - design::marks::DOT) / 2.)).child(dot()))
                        .child(div().flex_1().min_w_0().child(title(lines)))
                        .child(trailing),
                )
                .child(div().truncate().text_size(px(text::CAPTION)).line_height(px(14.)).text_color(soft).child(detail))
                .when(foot, |d| {
                    let max = if width >= 130. {
                        3
                    } else if width >= 96. {
                        2
                    } else {
                        1
                    };
                    d.child(
                        div()
                            .absolute()
                            .left(px(space::S6))
                            .right(px(5.))
                            .bottom(px(space::S4))
                            .h(px(metrics::AVATAR))
                            .flex()
                            .items_center()
                            .when_some(meeting.clone().filter(|_| guests.is_empty() || width >= 70.), |d, url| {
                                d.child(
                                    div()
                                        .flex_shrink()
                                        .min_w_0()
                                        .h(px(16.))
                                        .px(px(space::S6))
                                        .flex()
                                        .items_center()
                                        .gap(px(3.))
                                        .rounded_full()
                                        .bg(p.chip)
                                        .text_size(px(text::TAG))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.muted)
                                        .child(icon(Icon::Video, 11., p.muted))
                                        .when(width >= 110., |d| d.child(model::meeting_label(&url))),
                                )
                            })
                            .child(div().flex_1())
                            .when(!guests.is_empty(), |d| d.child(guest_avatars(&guests, max, face, p, cx))),
                    )
                })
        };
        let key = event.key.clone();
        let raised = p.raised_shadow.clone();
        let card = div()
            .id(ElementId::Name(format!("card:{}:{}:{}:{}", key.account, key.calendar, key.id, placement.top).into()))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width.max(0.)))
            .h(px(height.max(0.)))
            .overflow_hidden()
            .rounded(px(metrics::CARD_RADIUS))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open(key.clone(), cx)));
        let card = if selected {
            card.bg(linear_gradient(180., linear_color_stop(p.lifted_card[0], 0.), linear_color_stop(p.lifted_card[1], 1.)))
                .border(px(stroke::RIM))
                .border_color(p.lifted_rim)
                .shadow(p.lifted_shadow.clone())
        } else {
            let card = card.hover(move |s| s.shadow(raised.clone()));
            match look {
                Look::Invited => card
                    .bg(p.background.blend(design::tint(color, metrics::INVITED_TINT)))
                    .border_1()
                    .border_dashed()
                    .border_color(dot_color(color, p)),
                Look::Past => card
                    .bg(p.tray.blend(design::tint(p.background, fade)))
                    .border(px(stroke::HAIRLINE))
                    .border_color(p.card_border),
                Look::Declined => card.bg(p.tray.blend(p.hover)),
                Look::Upcoming => card
                    .bg(p.background)
                    .border(px(stroke::HAIRLINE))
                    .border_color(p.card_border)
                    .shadow(p.card_shadow.clone()),
            }
        };
        card.child(content)
    }

    /// The all-day row's bars of `page`: each tinted with its calendar's color, lifted when open,
    /// faded when over; ‹ and › where it goes on before or after the days shown.
    fn render_all_day_bars(
        &self,
        page: &Page,
        width: f32,
        now: DateTime<Local>,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        if width <= 0. {
            return Vec::new();
        }
        let column = width / page.days.len() as f32;
        let (first_day, last_day) = (page.days[0], page.days[page.days.len() - 1]);
        page.all_day
            .iter()
            .map(|placed| {
                let event = &self.events[placed.event];
                let past = event.end <= now;
                let fade = if past { metrics::PAST_OPACITY } else { 1. };
                let color = self.event_color(event);
                let selected = self.opened.as_ref() == Some(&event.key);
                let base = if past { p.tray } else { p.background };
                let face: Hsla = base.blend(design::tint(color, metrics::ALL_DAY_TINT * fade));
                let before = event.start.date_naive() < first_day;
                let after = placed.last == page.days.len() - 1 && event.last_day() > last_day;
                let title = format!(
                    "{}{}{}",
                    if before { "‹ " } else { "" },
                    event.item.title,
                    if after { " ›" } else { "" }
                );
                let key = event.key.clone();
                let raised = p.raised_shadow.clone();
                div()
                    .id(ElementId::Name(format!("bar:{}:{}:{}:{first_day}", key.account, key.calendar, key.id).into()))
                    .absolute()
                    .left(px(placed.first as f32 * column + 1.))
                    .w(px((placed.last - placed.first + 1) as f32 * column - 2.))
                    .top(px(placed.row as f32 * metrics::ALL_DAY_HEIGHT + 2.))
                    .h(px(metrics::ALL_DAY_HEIGHT - 2.))
                    .px(px(space::S6))
                    .flex()
                    .items_center()
                    .rounded(px(metrics::CARD_RADIUS))
                    .cursor_pointer()
                    .map(|d| {
                        if selected {
                            d.bg(p.lifted_card[0]).border(px(stroke::RIM)).border_color(p.lifted_rim).shadow(p.lifted_shadow.clone())
                        } else {
                            d.bg(face).hover(move |s| s.shadow(raised.clone()))
                        }
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open(key.clone(), cx)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(text::FOOTNOTE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(design::tint(p.text, fade))
                            .child(title),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The day and week views' one line for the accounts whose calendar cannot show; a click
    /// opens the agenda, which has each one's notice.
    pub(super) fn render_notice_line(&self, p: &'static Palette, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let notices: Vec<&mailstream::CalendarAccount> =
            self.accounts.iter().filter(|account| !account.granted || account.problem.is_some()).collect();
        let first = notices.first()?;
        let failed = notices.iter().any(|account| account.granted && account.problem.is_some());
        let message = if notices.len() == 1 {
            let label = if first.label.is_empty() { &first.id } else { &first.label };
            if failed { format!("{label}: could not update") } else { format!("{label}: sign in again to show its calendar") }
        } else {
            format!("{} accounts cannot show their calendars", notices.len())
        };
        Some(
            div().flex_shrink_0().px(px(space::S12)).pb(px(space::S6)).child(
                notice(if failed { Tone::Error } else { Tone::Action }, None, message, true, p)
                    .id("calendar-notice")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_view(View::Agenda, cx)))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(text::CAPTION))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.accent)
                            .child("Show in Agenda"),
                    ),
            )
            .into_any_element(),
        )
    }
}
