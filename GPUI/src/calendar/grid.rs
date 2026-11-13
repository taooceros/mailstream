//! The day and week views (Flutter's `CalendarGrid`, MS-212): days side by side on a 24-hour
//! timeline under their dates (today's raised, the others over a bar of the hours they book) and
//! the all-day row; the timed events as cards placed side by side where they overlap, today's
//! column lit, and the time now as a red line with its time in the gutter. Days the engine does
//! not fetch are shaded and named over the dates.
//!
//! The days page as one: a two-finger sideways swipe drags them 1:1, the gutter's hours staying
//! put, and past a third of the width or flicked they go on to the next page, else back; ← and →
//! slide the next page in. The pages either side are drawn only while they move.
//!
//! The week view opens an event in gpui-kit's popover beside its card (MS-184), not in the pane
//! beside the calendar: on the side of the card with more of the week, under an all-day bar. A
//! press outside it, Esc or another week closes it; the day view keeps the pane. A double click
//! on a card or bar opens the event's page over the whole window, in either view.

use chrono::{DateTime, Days, Local, NaiveDate, Timelike};
use gpui::prelude::FluentBuilder;
use gpui::base::Selectable;
use gpui::component::popover::Popover;
use gpui::{
    Anchor, AnyElement, App, ClickEvent, Context, Div, ElementId, FontWeight, HighlightStyle, Hsla,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, RenderOnce, ScrollEvent, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, StyledText, Window, canvas, div, linear_color_stop,
    linear_gradient, point, px, relative,
};

use super::agenda::{dot_color, guest_avatars};
use super::model::{self, AllDayPlacement, Event, EventKey, Look, Placement};
use super::{CalendarPane, Pan, View};
use crate::controls::{Tone, mark_dot, notice, tag};
use crate::design::{self, Palette, calendar as metrics, motion, space, stroke, text};
use crate::icons::{Icon, icon};
use crate::frame_clock;
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

/// The pages side by side in a band, shifted `pan` points to the right. They are placed in
/// proportion to the band, not by a width measured in an earlier frame, so they fill it at
/// whatever width it has now (an open event or a docked draft narrowing the list).
fn strip(pan: f32, pages: impl IntoIterator<Item = Div>) -> Div {
    div().absolute().top_0().bottom_0().left(px(pan)).w_full().children(pages)
}

/// A page `offset` pages from the shown one, as wide as the band.
fn place(offset: i64, page: Div) -> Div {
    page.absolute().top_0().bottom_0().left(relative(offset as f32)).w_full()
}

/// Day `index`'s column of `count`, the full height of its page.
fn day_column(index: usize, count: usize) -> Div {
    div().absolute().top_0().bottom_0().left(relative(index as f32 / count as f32)).w(relative(1. / count as f32))
}

/// `count` equal shares of a flex row's width, alongside others.
fn shares(count: usize) -> Div {
    let mut share = div().flex_basis(px(0.)).min_w_0();
    share.style().flex_grow = Some(count as f32);
    share
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
        // The event's page covers the days: they stay as they were until it goes back.
        if self.view == View::Agenda || self.page {
            return;
        }
        self.close_week_popover(cx);
        self.move_focus(step);
        let width = self.page_width.get();
        self.pan = Pan::Settle { from: step as f32 * width, velocity: 0., start: frame_clock::now() };
        cx.notify();
    }

    /// Another week takes the open event's popover away with the one it was in.
    fn close_week_popover(&mut self, cx: &mut Context<Self>) {
        if self.view == View::Week {
            self.close_event(cx);
        }
    }

    fn move_focus(&mut self, step: i64) {
        let days = if self.view == View::Week { 7 * step } else { step };
        self.focus_day = if days >= 0 { self.focus_day + Days::new(days as u64) } else { self.focus_day - Days::new(days.unsigned_abs()) };
    }

    /// Where the pages sit now, called while rendering: advances a slide and asks for the next
    /// frame while it moves.
    fn current_pan(&mut self, window: &mut Window, cx: &App) -> f32 {
        match &mut self.pan {
            Pan::Rest => 0.,
            Pan::Drag(offset) => *offset,
            Pan::Settle { from, velocity, start } => {
                let elapsed = frame_clock::elapsed(start, window, cx);
                let (offset, arrived) = motion::PAGING.at(*from, 0., *velocity, elapsed);
                if arrived {
                    self.pan = Pan::Rest;
                } else {
                    window.request_animation_frame_at_display_rate();
                }
                offset
            }
        }
    }

    /// A scroll event over the grid, in the capture phase: true when the page swipe took it.
    fn feed_swipe(&mut self, event: &ScrollEvent, _: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) -> bool {
        // A swipe over the event's page, or the pane over the days, is theirs (`feed_close_swipe`).
        if self.page || self.pane_covers {
            return false;
        }
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
                    self.close_week_popover(cx);
                    self.move_focus(step);
                    offset + step as f32 * width
                } else {
                    offset
                };
                // At the speed the fingers left it (MS-289, MS-290).
                self.pan = Pan::Settle { from, velocity, start: frame_clock::now() };
                cx.notify();
                true
            }
            Step::Cancel(offset) => {
                self.pan = Pan::Settle { from: offset, velocity: 0., start: frame_clock::now() };
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
        let pan = self.current_pan(window, cx);
        // The pages' width as last measured, for what follows a card's width (its content) and
        // not its place; before the first layout, the window's less the gutter.
        let measured = self.page_width.get();
        let width = if measured > 0. { measured } else { (f32::from(window.viewport_size().width) - metrics::GUTTER).max(1.) };
        // How far a line across a page may need to reach, whatever the page's width now.
        let reach = f32::from(window.viewport_size().width);
        let offsets: &[i64] = if pan == 0. { &[0] } else { &[-1, 0, 1] };
        let pages: Vec<(i64, Page)> =
            offsets.iter().map(|&offset| (offset, Page::new(&self.events, self.page_days(offset)))).collect();
        let current = &pages.iter().find(|(offset, _)| *offset == 0).expect("the page shown").1;
        let shows_today = current.days.contains(&today);
        if std::mem::take(&mut self.scroll_to_now) {
            self.scroll_timeline_to((now.hour() * 60 + now.minute()) as f32);
        }
        let unsynced = self.synced.filter(|(from, until)| current.days.iter().any(|day| day < from || day >= until));
        // The week's open event, for the popover of the first of its cards drawn (none while its
        // page covers the week); the window's height less the popovers' margin is all it may take.
        let room = f32::from(window.viewport_size().height) - 2. * space::S8;
        let mut floating = (self.view == View::Week && !self.page)
            .then(|| self.opened.as_ref().and_then(|key| self.event_index(key)))
            .flatten()
            .map(|index| self.render_popover(index, room, p, cx));

        let header = div().flex_shrink_0().h(px(metrics::HEADER_HEIGHT)).flex().child(div().flex_shrink_0().w(px(metrics::GUTTER))).child(
            div().flex_1().h_full().relative().overflow_hidden().child(strip(pan, pages.iter().map(|(offset, page)| {
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
            }))),
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
                    div().flex_1().h_full().relative().overflow_hidden().child(strip(pan, pages.iter().map(|(offset, page)| {
                        let bars = self.render_all_day_bars(page, now, &mut floating, p, cx);
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
                    }))),
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

        // The pages' width, measured as laid out, for the cards' content. A notify while the
        // frame is drawn is dropped (gpui forgets the views marked dirty as the frame ends), so
        // a new width draws the pane again on the next frame.
        let measure = {
            let cell = self.page_width.clone();
            let top = self.band_top.clone();
            let pane = cx.entity().downgrade();
            canvas(
                move |bounds, window, _| {
                    top.set(f32::from(bounds.origin.y));
                    let measured = f32::from(bounds.size.width);
                    if (cell.get() - measured).abs() > 0.5 {
                        cell.set(measured);
                        let pane = pane.clone();
                        window.on_next_frame(move |_, cx| {
                            pane.update(cx, |_, cx| cx.notify()).ok();
                        });
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
                            .debug_selector(|| "timeline-band".into())
                            .child(measure)
                            .child(strip(
                                pan,
                                pages
                                    .iter()
                                    .map(|(offset, page)| {
                                        place(*offset, self.render_timeline(page, today, now, (width, reach), &mut floating, p, cx))
                                    }),
                            )),
                    ),
            );

        // Two-finger paging: scroll events are seen in the capture phase, before the timeline
        // scrolls with them, so a sideways swipe is never eaten by it.
        let swipe = swipe::scroll_capture(cx.entity(), Self::feed_swipe).absolute().size_full();

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
    /// the lines between days, the cards, and the time now. Everything sits in proportion to the
    /// page; `width`, the page's as last measured, only decides what a card has room to show, and
    /// `reach` how far a line may have to run.
    fn render_timeline(
        &self,
        page: &Page,
        today: NaiveDate,
        now: DateTime<Local>,
        (width, reach): (f32, f32),
        floating: &mut Option<AnyElement>,
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
            .when_some(today_at, |d, at| d.child(day_column(at, count).bg(p.spotlight)))
            .children(unfetched.into_iter().map(|i| day_column(i, count).bg(shade)))
            .children((0..=24).map(|hour| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(metrics::INSET + hour as f32 * metrics::HOUR_HEIGHT))
                    .h(px(1.))
                    .bg(p.line)
            }))
            .children((0..count).map(|i| day_column(i, count).w(px(1.)).bg(p.line)));
        let gap = metrics::CONFLICT_GAP;
        // Half a gap from the day's line on the left, a whole one before the next day's.
        let room = column - 1.5 * gap;
        for (i, (placed, conflicts)) in page.timed.iter().zip(&page.conflicts).enumerate() {
            // The popover on the side with more of the week (the middle day's on its left).
            let side = if 2 * i + 1 < count { Anchor::LeftCenter } else { Anchor::RightCenter };
            let mut cards = div().absolute().top_0().bottom_0().left(px(gap / 2.)).right(px(gap));
            for (placement, &conflict) in placed.iter().zip(conflicts) {
                let columns = placement.columns as f32;
                let trailing = if placement.column + placement.span < placement.columns { gap } else { 0. };
                let card_width = room / columns * placement.span as f32 - trailing;
                let card_height = ((placement.bottom - placement.top) as f32 * PER_MINUTE - gap / 2.).max(0.);
                // A row across the day's room: the card's slot takes as many shares of it as the
                // columns it spans, after as many as it is in from the left.
                cards = cards.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(metrics::INSET + placement.top as f32 * PER_MINUTE))
                        .h(px(card_height))
                        .flex()
                        .child(shares(placement.column))
                        .child(
                            shares(placement.span)
                                .flex()
                                .child(self.render_card(placement, conflict, (card_width, card_height), now, (side, &mut *floating), p, cx))
                                .child(div().flex_shrink_0().w(px(trailing))),
                        )
                        .child(shares(placement.columns - placement.column - placement.span)),
                );
            }
            let day = page.days[i];
            let column = day_column(i, count).id(ElementId::Name(format!("timeline-day:{day}").into()));
            let column = if self.can_create() {
                // A click on free time there: a new event from the quarter hour under it.
                let top = self.band_top.clone();
                column.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    // The press closed the open event's popover: that is all it does.
                    let down = match event {
                        ClickEvent::Mouse(click) => Some(click.down.position),
                        _ => None,
                    };
                    if this.dismissed_at.take().is_some_and(|at| Some(at) == down) {
                        return;
                    }
                    let y = f32::from(event.position().y) - top.get() - metrics::INSET;
                    let minutes = ((y / PER_MINUTE).floor() as i64).div_euclid(15) * 15;
                    let minutes = minutes.clamp(0, DAY_MINUTES as i64 - 15);
                    let start = day.and_hms_opt(0, 0, 0).expect("midnight") + chrono::Duration::minutes(minutes);
                    this.create_event(Some(start), window, cx);
                }))
            } else {
                column
            };
            d = d.child(column.child(cards));
        }
        // The time now: a line with a dot across today's column, dashed across the other days.
        if let Some(at) = today_at {
            let y = metrics::INSET + (now.hour() * 60 + now.minute()) as f32 * PER_MINUTE;
            let trail = design::tint(p.danger, metrics::NOW_TRAIL_OPACITY);
            // Dashes `DASH` long, as far apart, across `band`, which cuts them off at its end.
            let dashed = |band: Div| {
                let dashes = (reach / (2. * metrics::DASH)).ceil() as usize;
                band.absolute().top(px(y - 0.5)).h(px(1.)).overflow_hidden().children((0..dashes).map(|i| {
                    div().absolute().top_0().left(px(i as f32 * 2. * metrics::DASH)).w(px(metrics::DASH)).h(px(1.)).bg(trail)
                }))
            };
            d = d
                .child(dashed(div().left_0().w(relative(at as f32 / count as f32))))
                .child(dashed(div().left(relative((at + 1) as f32 / count as f32)).right_0()))
                .child(
                    day_column(at, count)
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(y - metrics::NOW_LINE / 2.))
                                .h(px(metrics::NOW_LINE))
                                .bg(p.danger),
                        )
                        .child(div().absolute().left(px(-3.)).top(px(y - 3.)).size(px(6.)).rounded_full().bg(p.danger)),
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
        (width, height): (f32, f32),
        now: DateTime<Local>,
        (side, floating): (Anchor, &mut Option<AnyElement>),
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        // A past card's title stays readable; its details and dot fade further.
        let ink = design::tint(p.text, if look == Look::Past { metrics::PAST_EMPHASIS } else { fade });
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
            let face = if selected { p.lifted_card[1] } else { p.card };
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
                                        .flex_shrink_1()
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
        let id = format!("card:{}:{}:{}:{}", key.account, key.calendar, key.id, placement.top);
        let card = div()
            .id(ElementId::Name(id.clone().into()))
            .debug_selector(|| format!("card:{}", key.id))
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .rounded(px(metrics::CARD_RADIUS))
            .cursor_pointer();
        let card = if selected {
            card.bg(linear_gradient(180., linear_color_stop(p.lifted_card[0], 0.), linear_color_stop(p.lifted_card[1], 1.)))
                .border(px(stroke::RIM))
                .border_color(p.lifted_rim)
                .shadow(p.lifted_shadow.clone())
        } else {
            let card = card.hover(move |s| s.shadow(raised.clone()));
            match look {
                Look::Invited => card
                    .relative()
                    .bg(p.card)
                    .border_1()
                    .border_dashed()
                    .border_color(dot_color(color, p))
                    .child(hatch(design::tint(color, metrics::HATCH_OPACITY))),
                Look::Past => card
                    .bg(p.tray.blend(design::tint(p.card, metrics::PAST_EMPHASIS)))
                    .border(px(stroke::HAIRLINE))
                    .border_color(p.card_border),
                Look::Declined => card.bg(p.tray.blend(p.hover)),
                Look::Upcoming => card
                    .bg(p.card)
                    .border(px(stroke::HAIRLINE))
                    .border_color(p.card_border)
                    .shadow(p.card_shadow.clone()),
            }
        };
        // The content as wide as the card was last measured: its lines are cut to fit (…) only
        // in a box of known width. The card itself always fills its place; a frame after the
        // width changes, the content follows.
        let card = card.child(content.w(px(width.max(0.))));
        self.opener(card, id, &event.key, side, floating, cx)
    }

    /// `trigger` (an event's card or all-day bar) opening its event: in the week view under
    /// gpui-kit's popover (MS-184), which a press on it opens and which shows `floating` (the
    /// open event's panel, taken by the first of its triggers drawn) with its `anchor` corner
    /// against it, `POPOVER_GAP` off; a press outside it or Esc closes it. Elsewhere a click opens
    /// the event in the pane. Either way a double click opens its page, and the press never
    /// reaches the free time under it.
    fn opener(
        &self,
        trigger: Stateful<Div>,
        id: String,
        key: &EventKey,
        anchor: Anchor,
        floating: &mut Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = key.clone();
        let clicked = key.clone();
        let trigger = trigger.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            cx.stop_propagation();
            // The week's popover opens on the press; its second (a double click's) closed it.
            if this.view != View::Week || event.click_count() >= 2 {
                this.open_clicked(clicked.clone(), event, cx);
            }
        }));
        if self.view != View::Week {
            return trigger.into_any_element();
        }
        let content = if self.opened.as_ref() == Some(&key) { floating.take() } else { None };
        let pane = cx.entity().downgrade();
        Popover::new(ElementId::Name(format!("popover:{id}").into()))
            .appearance(false)
            .anchor(anchor)
            .offset(px(metrics::POPOVER_GAP))
            .trigger_style(StyleRefinement::default().flex_1().min_w_0().h_full())
            .trigger(Trigger(trigger.into_any_element()))
            .open(content.is_some())
            .on_open_change(move |&open, window, cx| {
                // Esc closes it with the pointer anywhere; only a press is where it went down.
                let at = (!window.last_input_was_keyboard()).then(|| window.mouse_position());
                pane.update(cx, |this, cx| if open { this.open(key.clone(), cx) } else { this.popover_closed(&key, at, cx) })
                    .ok();
            })
            .children(content)
            .into_any_element()
    }

    /// The all-day row's bars of `page`: each tinted with its calendar's color, lifted when open,
    /// faded when over; ‹ and › where it goes on before or after the days shown.
    fn render_all_day_bars(
        &self,
        page: &Page,
        now: DateTime<Local>,
        floating: &mut Option<AnyElement>,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let (first_day, last_day) = (page.days[0], page.days[page.days.len() - 1]);
        page.all_day
            .iter()
            .map(|placed| {
                let event = &self.events[placed.event];
                let past = event.end <= now;
                let fade = if past { metrics::PAST_OPACITY } else { 1. };
                let color = self.event_color(event);
                let selected = self.opened.as_ref() == Some(&event.key);
                let base = if past { p.tray } else { p.card };
                let face: Hsla = base.blend(design::tint(color, metrics::ALL_DAY_TINT * fade));
                let before = event.start.date_naive() < first_day;
                let after = placed.last == page.days.len() - 1 && event.last_day() > last_day;
                let title = format!(
                    "{}{}{}",
                    if before { "‹ " } else { "" },
                    event.item.title,
                    if after { " ›" } else { "" }
                );
                let key = &event.key;
                let raised = p.raised_shadow.clone();
                let id = format!("bar:{}:{}:{}:{first_day}", key.account, key.calendar, key.id);
                let bar = div()
                    .id(ElementId::Name(id.clone().into()))
                    .flex_1()
                    .min_w_0()
                    .h_full()
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
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(text::FOOTNOTE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(design::tint(p.text, if past { metrics::PAST_EMPHASIS } else { 1. }))
                            .child(title),
                    );
                // Over its days in the row, a point in from their lines.
                let days = page.days.len();
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(placed.row as f32 * metrics::ALL_DAY_HEIGHT + 2.))
                    .h(px(metrics::ALL_DAY_HEIGHT - 2.))
                    .flex()
                    .child(shares(placed.first))
                    .child(
                        shares(placed.last - placed.first + 1)
                            .flex()
                            .child(div().flex_shrink_0().w(px(1.)))
                            .child(self.opener(bar, id, key, Anchor::TopCenter, floating, cx))
                            .child(div().flex_shrink_0().w(px(1.))),
                    )
                    .child(shares(days - 1 - placed.last))
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

#[cfg(test)]
mod tests {
    use chrono::{Days, Local, NaiveDate};
    use gpui::{
        AppContext as _, Bounds, Context, Entity, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
        ParentElement, Pixels, Point, Render, ScrollDelta, ScrollEvent, ScrollSource, Styled, TestAppContext, TouchPhase,
        VisualTestContext, Window, div, point, px, size,
    };

    use super::super::model::{self, tests::timed};
    use super::super::{CalendarPane, Pan, View};

    /// The window as the app's draws the calendar: the pane, and its open event's page over it.
    struct Host(Entity<CalendarPane>);

    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let page = self.0.update(cx, |pane, cx| pane.render_page(window, cx));
            div().relative().size_full().child(self.0.clone()).children(page)
        }
    }

    /// The calendar's week view of this week in a window 1400 wide, with an hour on its Saturday
    /// (`saturday`) and today; the calendar has the keyboard.
    fn week_view(cx: &mut TestAppContext) -> (Entity<CalendarPane>, &mut VisualTestContext) {
        // The pane draws gpui-kit's buttons, which read the kit's theme.
        cx.update(|cx| {
            gpui::init(cx);
            crate::design::sync_kit_theme(gpui::WindowAppearance::Light, cx);
            super::super::bind_keys(cx);
        });
        let (host, cx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| CalendarPane::new(window, cx))));
        let pane = host.read_with(cx, |host, _| host.0.clone());
        cx.simulate_resize(size(px(1400.), px(900.)));
        let today = Local::now().date_naive();
        let saturday = model::week_of(today) + Days::new(6);
        pane.update_in(cx, |pane, window, cx| {
            pane.events = vec![timed("today", today, (10, 0), (11, 0)), timed("saturday", saturday, (10, 0), (11, 0))];
            pane.events.sort_by_key(|event| event.start);
            pane.listed = true;
            pane.view = View::Week;
            pane.rebuild(today);
            window.focus(&pane.focus, cx);
            cx.notify();
        });
        cx.run_until_parked();
        (pane, cx)
    }

    fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    }

    /// A scroll over the timeline as gpui's Wayland client sends a touchpad's: `Started` with no
    /// movement as the fingers land, `Moved` in points as they move, `Ended` with none as they
    /// lift, then the fling as `Moved` again.
    fn touchpad(at: gpui::Point<Pixels>, phase: TouchPhase, dx: f32, dy: f32) -> ScrollEvent {
        ScrollEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
            source: ScrollSource::Pointer,
        }
    }

    fn shown(pane: &Entity<CalendarPane>, cx: &mut VisualTestContext) -> (NaiveDate, Pan) {
        pane.read_with(cx, |pane, _| (pane.focus_day, pane.pan))
    }

    fn opened(pane: &Entity<CalendarPane>, cx: &mut VisualTestContext) -> Option<String> {
        pane.read_with(cx, |pane, _| pane.opened.as_ref().map(|key| key.id.to_string()))
    }

    /// A click at `at`, then the frames it brings.
    fn click(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_click(at, Modifiers::default());
        cx.run_until_parked();
    }

    /// Two presses at `at` as a double click makes them, then the frames they bring.
    fn double_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
        for click_count in 1..=2 {
            let (button, modifiers) = (MouseButton::Left, Modifiers::default());
            cx.simulate_event(MouseDownEvent { button, position: at, modifiers, click_count, first_mouse: false });
            cx.simulate_event(MouseUpEvent { button, position: at, modifiers, click_count });
        }
        cx.run_until_parked();
    }

    fn click_on(cx: &mut VisualTestContext, selector: &'static str) {
        let at = bounds(cx, selector).center();
        click(cx, at);
    }

    fn double_click_on(cx: &mut VisualTestContext, selector: &'static str) {
        let at = bounds(cx, selector).center();
        double_click(cx, at);
    }

    fn key(cx: &mut VisualTestContext, keys: &str) {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_narrower_band_keeps_the_whole_week_in_it(cx: &mut TestAppContext) {
        let (_, cx) = week_view(cx);
        let wide = bounds(cx, "timeline-band");
        assert!(wide.contains(&bounds(cx, "card:saturday").center()));
        // One frame at the new width: what the pane last measured is the old one.
        cx.simulate_resize(size(px(800.), px(900.)));
        let band = bounds(cx, "timeline-band");
        assert!(band.size.width < wide.size.width * 0.6, "the band narrowed with the window");
        let saturday = bounds(cx, "card:saturday");
        assert!(
            saturday.left() > band.left() + band.size.width * 6. / 7. - px(1.) && saturday.right() <= band.right(),
            "Saturday's card sits in the last seventh of the band: {saturday:?} in {band:?}"
        );
    }

    #[gpui::test]
    fn a_click_on_an_event_floats_it_beside_its_card_and_esc_closes_it(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        let band = bounds(cx, "timeline-band");
        let card = bounds(cx, "card:saturday");
        click(cx, card.center());
        assert_eq!(opened(&pane, cx).as_deref(), Some("saturday"));
        let popover = bounds(cx, "calendar-popover");
        assert_eq!(bounds(cx, "timeline-band"), band, "no pane beside the week narrows it");
        assert!(
            popover.right() <= card.left() && popover.right() >= card.left() - px(16.),
            "the last day's event floats just left of its card: {popover:?} beside {card:?}"
        );
        assert!(popover.top() < card.bottom() && popover.bottom() > card.top(), "level with it");

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(opened(&pane, cx), None);
        assert!(cx.debug_bounds("calendar-popover").is_none(), "Esc closed it");
        // The calendar has the keyboard back.
        cx.simulate_keystrokes("t");
        assert!(pane.update_in(cx, |pane, window, _| pane.focus.is_focused(window)));
    }

    #[gpui::test]
    fn a_press_outside_or_another_week_closes_the_popover(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        let today = bounds(cx, "card:today");
        click(cx, today.center());
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"));
        let popover = bounds(cx, "calendar-popover");
        // Free time on the other side of the week from the popover.
        let band = bounds(cx, "timeline-band");
        let free = if popover.left() > today.left() {
            point(band.left() + px(10.), today.top() - px(30.))
        } else {
            point(band.right() - px(10.), today.top() - px(30.))
        };
        click(cx, free);
        assert_eq!(opened(&pane, cx), None, "a press outside closes it");
        assert!(cx.debug_bounds("calendar-popover").is_none());

        click(cx, today.center());
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"));
        let (week, _) = shown(&pane, cx);
        cx.simulate_keystrokes("right");
        cx.run_until_parked();
        assert_eq!(shown(&pane, cx).0, week + Days::new(7));
        assert_eq!(opened(&pane, cx), None, "the next week closes it");
        assert!(cx.debug_bounds("calendar-popover").is_none());
    }

    #[gpui::test]
    fn a_two_finger_swipe_to_the_left_pages_to_the_next_week(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        let at = bounds(cx, "timeline-band").center();
        let (week, _) = shown(&pane, cx);
        cx.simulate_event(touchpad(at, TouchPhase::Started, 0., 0.));
        for _ in 0..10 {
            cx.simulate_event(touchpad(at, TouchPhase::Moved, -50., 0.5));
        }
        assert!(matches!(shown(&pane, cx), (day, Pan::Drag(offset)) if day == week && offset == -500.), "the week follows the fingers");
        cx.simulate_event(touchpad(at, TouchPhase::Ended, 0., 0.));
        let (day, pan) = shown(&pane, cx);
        assert_eq!(day, week + Days::new(7));
        assert!(matches!(pan, Pan::Settle { .. }), "the next week slides on from where the fingers left it");
        // The fling after the lift is the swipe's: the timeline doesn't scroll with it.
        let scrolled = pane.read_with(cx, |pane, _| pane.timeline.offset());
        cx.simulate_event(touchpad(at, TouchPhase::Moved, -30., -30.));
        assert_eq!(pane.read_with(cx, |pane, _| pane.timeline.offset()), scrolled);
        assert_eq!(shown(&pane, cx).0, week + Days::new(7));
    }

    #[gpui::test]
    fn a_short_swipe_springs_back_and_a_vertical_one_scrolls_the_hours(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        let at = bounds(cx, "timeline-band").center();
        let (week, _) = shown(&pane, cx);
        cx.simulate_event(touchpad(at, TouchPhase::Started, 0., 0.));
        cx.simulate_event(touchpad(at, TouchPhase::Moved, 8., 0.));
        cx.simulate_event(touchpad(at, TouchPhase::Moved, 7., 0.));
        cx.simulate_event(touchpad(at, TouchPhase::Ended, 0., 0.));
        assert!(matches!(shown(&pane, cx), (day, Pan::Settle { from, .. }) if day == week && from == 15.), "back to this week");

        // From midnight, so the hours can scroll on whatever the time now.
        pane.update(cx, |pane, cx| {
            pane.scroll_timeline_to(0.);
            cx.notify();
        });
        let scrolled = pane.read_with(cx, |pane, _| pane.timeline.offset().y);
        cx.simulate_event(touchpad(at, TouchPhase::Started, 0., 0.));
        for _ in 0..3 {
            cx.simulate_event(touchpad(at, TouchPhase::Moved, 1., -40.));
        }
        cx.simulate_event(touchpad(at, TouchPhase::Ended, 0., 0.));
        assert_eq!(shown(&pane, cx).0, week);
        assert!(pane.read_with(cx, |pane, _| pane.timeline.offset().y) < scrolled, "the hours scrolled");

        // A mouse wheel's sideways step is no swipe.
        cx.simulate_event(ScrollEvent {
            position: at,
            delta: ScrollDelta::Lines(point(-3., 0.)),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
            source: ScrollSource::Pointer,
        });
        assert_eq!(shown(&pane, cx).0, week);
    }

    #[gpui::test]
    fn a_double_click_opens_the_page_over_the_window_and_esc_goes_back_to_the_popover(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        let band = bounds(cx, "timeline-band");
        double_click_on(cx, "card:saturday");
        assert_eq!(opened(&pane, cx).as_deref(), Some("saturday"));
        let page = bounds(cx, "calendar-page");
        assert_eq!(page.size, size(px(1400.), px(900.)), "the page covers the whole window: {page:?}");
        assert!(cx.debug_bounds("calendar-popover").is_none(), "the popover gives way to it");
        // The week's keys don't page the week beneath.
        let (week, _) = shown(&pane, cx);
        key(cx, "right");
        assert_eq!(shown(&pane, cx).0, week);
        // A sideways swipe over it doesn't either.
        let at = page.center();
        cx.simulate_event(touchpad(at, TouchPhase::Started, 0., 0.));
        for _ in 0..10 {
            cx.simulate_event(touchpad(at, TouchPhase::Moved, -50., 0.));
        }
        cx.simulate_event(touchpad(at, TouchPhase::Ended, 0., 0.));
        assert!(matches!(shown(&pane, cx), (day, Pan::Rest) if day == week));

        key(cx, "escape");
        slide_done(&pane, cx);
        assert!(cx.debug_bounds("calendar-page").is_none(), "Esc went back");
        assert_eq!(opened(&pane, cx).as_deref(), Some("saturday"), "to the event's popover, as it was");
        bounds(cx, "calendar-popover");
        assert_eq!(bounds(cx, "timeline-band"), band, "the week as it was");
        key(cx, "escape");
        assert_eq!(opened(&pane, cx), None);
        assert!(cx.debug_bounds("calendar-popover").is_none());
    }

    #[gpui::test]
    fn return_or_the_expand_button_opens_the_page_and_back_returns(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        click_on(cx, "card:today");
        bounds(cx, "calendar-popover");
        key(cx, "enter");
        slide_done(&pane, cx);
        bounds(cx, "calendar-page");
        click_on(cx, "calendar-page-back");
        slide_done(&pane, cx);
        assert!(cx.debug_bounds("calendar-page").is_none(), "Back went back");
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"));

        click_on(cx, "calendar-expand");
        slide_done(&pane, cx);
        bounds(cx, "calendar-page");
        assert!(cx.debug_bounds("calendar-popover").is_none());
        // ↓ takes the page to the week's next event.
        key(cx, "down");
        assert_eq!(opened(&pane, cx).as_deref(), Some("saturday"));
        bounds(cx, "calendar-page");
        key(cx, "escape");
        slide_done(&pane, cx);
        assert!(cx.debug_bounds("calendar-page").is_none());
        assert_eq!(opened(&pane, cx).as_deref(), Some("saturday"));
    }

    #[gpui::test]
    fn the_day_view_opens_the_page_from_its_pane(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        key(cx, "d");
        click_on(cx, "card:today");
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"));
        assert!(cx.debug_bounds("calendar-page").is_none(), "a click opens the pane");
        key(cx, "enter");
        bounds(cx, "calendar-page");
        key(cx, "escape");
        slide_done(&pane, cx);
        assert!(cx.debug_bounds("calendar-page").is_none());
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"), "back to the pane");
        double_click_on(cx, "card:today");
        bounds(cx, "calendar-page");
    }

    /// A left press at `from`, the pointer held through `steps` moves to `to`; released there
    /// unless `hold`.
    fn mouse_drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>, steps: usize, hold: bool) {
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let at = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::default());
        }
        if !hold {
            cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        }
        cx.run_until_parked();
    }

    /// The frames once a slide has had its time.
    fn slide_done(pane: &Entity<CalendarPane>, cx: &mut VisualTestContext) {
        std::thread::sleep(crate::design::motion::PAGE.longest() + std::time::Duration::from_millis(50));
        pane.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }

    fn close_offset(pane: &Entity<CalendarPane>, cx: &mut VisualTestContext) -> f32 {
        pane.read_with(cx, |pane, _| pane.close.offset(pane.close_width.get()))
    }

    #[gpui::test]
    fn a_sideways_mouse_drag_closes_the_pane_and_a_short_one_springs_back(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        key(cx, "d");
        click_on(cx, "card:today");
        slide_done(&pane, cx);
        let detail = bounds(cx, "calendar-detail");
        // On the event's head, under its toolbar.
        let at = point(detail.center().x, detail.top() + px(120.));

        // Mid-drag the pane is under the pointer; let go short of a third, at rest, it springs back.
        mouse_drag(cx, at, point(at.x + px(80.), at.y + px(2.)), 8, true);
        assert_eq!(close_offset(&pane, cx), 80., "the pane follows the pointer");
        std::thread::sleep(std::time::Duration::from_millis(150));
        cx.simulate_mouse_up(point(at.x + px(80.), at.y), MouseButton::Left, Modifiers::default());
        slide_done(&pane, cx);
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"), "a short drag springs back");
        assert_eq!(bounds(cx, "calendar-detail"), detail, "back in its place");

        // A drag that sets off downward leaves it alone.
        mouse_drag(cx, at, point(at.x + px(60.), at.y + px(60.)), 6, false);
        slide_done(&pane, cx);
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"));
        assert_eq!(bounds(cx, "calendar-detail"), detail);

        // Past a third of its width it slides off and closes.
        mouse_drag(cx, at, point(at.x + detail.size.width * 0.6, at.y), 12, false);
        slide_done(&pane, cx);
        assert_eq!(opened(&pane, cx), None, "dragged off, the pane closed");
        assert!(cx.debug_bounds("calendar-detail").is_none());

        // Two fingers do the same.
        click_on(cx, "card:today");
        slide_done(&pane, cx);
        let at = bounds(cx, "calendar-detail").center();
        cx.simulate_event(touchpad(at, TouchPhase::Started, 0., 0.));
        for _ in 0..12 {
            cx.simulate_event(touchpad(at, TouchPhase::Moved, 50., 0.));
        }
        cx.simulate_event(touchpad(at, TouchPhase::Ended, 0., 0.));
        slide_done(&pane, cx);
        assert_eq!(opened(&pane, cx), None, "swiped off, the pane closed");
    }

    #[gpui::test]
    fn the_page_drags_off_from_its_title_row_back_to_the_pane_and_the_popover_stays(cx: &mut TestAppContext) {
        let (pane, cx) = week_view(cx);
        key(cx, "d");
        click_on(cx, "card:today");
        key(cx, "enter");
        slide_done(&pane, cx);
        let page = bounds(cx, "calendar-page");
        // The title row, between its Back and its buttons.
        let at = point(page.center().x, page.top() + px(30.));
        mouse_drag(cx, at, point(at.x + px(700.), at.y), 14, false);
        slide_done(&pane, cx);
        assert!(cx.debug_bounds("calendar-page").is_none(), "dragged off, the page went back");
        assert_eq!(opened(&pane, cx).as_deref(), Some("today"), "to the pane, as it was");
        bounds(cx, "calendar-detail");

        // The week's popover closes with a press outside it, not a drag.
        key(cx, "w");
        click_on(cx, "card:saturday");
        let popover = bounds(cx, "calendar-popover");
        let at = point(popover.center().x, popover.top() + px(80.));
        mouse_drag(cx, at, point(at.x + px(300.), at.y), 10, false);
        slide_done(&pane, cx);
        assert_eq!(opened(&pane, cx).as_deref(), Some("saturday"));
        assert_eq!(bounds(cx, "calendar-popover"), popover);
    }
}

/// An unanswered invitation's hatch (Flutter's `_InvitedPainter`): 1 pt lines at 45° in `color`,
/// `HATCH_SPACING` apart, over the whole card under it (which clips them to itself).
fn hatch(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let mut lines = PathBuilder::stroke(px(1.));
            let mut x = -height;
            while x < width {
                lines.move_to(bounds.origin + point(px(x), px(height)));
                lines.line_to(bounds.origin + point(px(x + height), px(0.)));
                x += metrics::HATCH_SPACING;
            }
            if let Ok(path) = lines.build() {
                window.paint_path(path, color);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// An event's card or bar as the trigger gpui-kit's popover asks for: the popover's open state
/// shows on it through `CalendarPane::opened`, so it takes no part in that.
#[derive(IntoElement)]
struct Trigger(AnyElement);

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.0
    }
}

impl Selectable for Trigger {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}
