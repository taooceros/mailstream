//! The calendar's list island (Flutter's `CalendarListPane`): the title row with the calendars'
//! list (each account's own and others' shared or subscribed calendars, shown or hidden with a
//! switch, MS-302), New event and Refresh over
//! the views' bar (Agenda, Day, Week; previous page, Today, next page), then the agenda (this
//! week's days on a strip, today raised and the others over a bar of the hours they book; the
//! meeting about to start on one card with Join; notices for accounts whose calendar is not
//! connected or whose fetch failed; each day from today with its events under its header), or the day or week view
//! (`grid.rs`).

use chrono::{DateTime, Local, NaiveDate};
use gpui::component::popover::Popover;
use gpui::component::tab::TabBar;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    ListOffset, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, list, px,
};
use mailstream::{CalendarInfo, CalendarPerson};

use super::grid::day_header;
use super::model::{self, ReplyState};
use super::{CalendarPane, Entry, Refresh, Today, View};
use crate::avatars;
use crate::controls::{
    ButtonKind, ControlSize, Tone, button, capsule, capsule_button, color_dots, hairline, icon_button,
    icon_label_button, island, list_row, mark_dot, notice, notice_close, popover, segment, segmented, swatch_row, tag,
    title_bar, tooltip,
};
use crate::design::{self, Palette, calendar as metrics, icon as icon_size, layout, list as rows, space, stroke, text};
use crate::icons::{Icon, icon};
use mailstream::Address;

/// A guest as the address their avatar is drawn for.
pub(super) fn guest_address(person: &CalendarPerson) -> Address {
    Address { name: person.name.clone().map(Into::into), email: person.email.clone().unwrap_or_default().into() }
}

impl CalendarPane {
    /// The list island: its title row and the views' bar over the agenda (with the week strip and
    /// the Up Next card), the day view (Up Next over the timeline) or the week view.
    pub(super) fn render_list_island(
        &mut self,
        today: NaiveDate,
        p: &'static Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let now = Local::now();
        let header = self.render_header(now, p, window, cx);
        let island = island(p.tray, p).occlude().flex().flex_col().child(header);
        if !self.listed {
            return island.child(div().flex_1().min_h_0().child(crate::controls::centered_note("Loading…", p)));
        }
        match self.view {
            View::Agenda => island
                .child(self.render_week_strip(today, p, cx))
                .children(self.render_up_next(now, p, cx))
                .child(
                    div().flex_1().min_h_0().child(
                        list(self.agenda.clone(), cx.processor(move |this, ix: usize, _, cx| this.render_entry(ix, now, p, cx)))
                            .size_full()
                            .pt(px(space::S4))
                            .pb(px(space::S24)),
                    ),
                ),
            View::Day | View::Week => {
                let notices = self.render_notice_line(p, cx);
                let up_next = if self.view == View::Day { self.render_up_next(now, p, cx) } else { None };
                let grid = self.render_grid(today, now, p, window, cx);
                island.children(notices).children(up_next).child(div().flex_1().min_h_0().child(grid))
            }
        }
    }

    /// The title row (Calendar, what the view shows, Refresh) over the views' bar: Agenda, Day
    /// and Week, then the previous page, Today and the next page.
    fn render_header(&self, now: DateTime<Local>, p: &'static Palette, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let subtitle = match self.view {
            View::Agenda => now.format("%A, %B %-d").to_string(),
            View::Day => model::day_title(self.focus_day),
            View::Week => model::week_title(model::week_of(self.focus_day)),
        };
        let paged = self.view != View::Agenda;
        let unit = if self.view == View::Week { "week" } else { "day" };
        let views = [(View::Agenda, "Agenda", "A"), (View::Day, "Day", "D"), (View::Week, "Week", "W")];
        title_bar("calendar-header", window, cx)
            .flex_shrink_0()
            .px(px(space::S10))
            .pb(px(space::S8))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(layout::HEADER_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(space::S10))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl(px(space::S6))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(space::S8))
                                    .child(div().text_size(px(text::PANE_TITLE)).font_weight(FontWeight::BOLD).child("Calendar"))
                                    .when(self.loading, |d| {
                                        d.child(div().text_size(px(text::CAPTION)).text_color(p.muted).child("Updating…"))
                                    }),
                            )
                            .child(div().truncate().text_size(px(text::CAPTION)).text_color(p.muted).child(subtitle)),
                    )
                    .child(
                        capsule(p)
                            .child(self.calendar_list_button(p, cx))
                            .child(div().w(px(stroke::HAIRLINE)).h(px(16.)).mx(px(space::S2)).bg(p.line))
                            .child(
                                icon_button("calendar-new", Icon::AddDraft, "New event", self.can_create(), p.glyph)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.create_event(None, window, cx))),
                            )
                            .child(
                                icon_button("calendar-refresh", Icon::Refresh, "Refresh", self.has_calendars(), p.glyph)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.refresh(&Refresh, window, cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .pl(px(space::S6))
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .child(segmented("calendar-views", views.iter().position(|(view, ..)| *view == self.view), p).children(
                        views.map(|(view, label, key)| {
                            let tip: SharedString = format!("{label} ({key})").into();
                            segment(label, None, None)
                                .tooltip(tooltip(tip))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_view(view, cx)))
                        }),
                    ))
                    .child(div().flex_1())
                    .child(
                        capsule(p)
                            .when(paged, |d| {
                                d.child(
                                    icon_button("calendar-previous", Icon::Back, format!("Previous {unit} (←)"), true, p.glyph)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.page(-1, cx))),
                                )
                            })
                            .child(icon_button("calendar-today", Icon::Today, "Today (T)", true, p.glyph).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| this.today(&Today, window, cx)),
                            ))
                            .when(paged, |d| {
                                d.child(
                                    icon_button("calendar-next", Icon::Disclosure, format!("Next {unit} (→)"), true, p.glyph)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.page(1, cx))),
                                )
                            }),
                    ),
            )
    }

    /// Calendars: the list of every account's calendars, under the capsule button that shows
    /// their active color dots and count.
    fn calendar_list_button(&self, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut colors = Vec::new();
        let mut total = 0usize;
        for (index, account) in self.accounts.iter().enumerate().filter(|(_, account)| account.granted) {
            for calendar in &account.calendars {
                total += 1;
                if calendar.shown {
                    let color = model::hex_color(calendar.color.as_deref()).unwrap_or_else(|| design::account_color(index));
                    colors.push(dot_color(color, p));
                }
            }
        }
        let listed = total > 0;
        let shown = colors.len();
        let detail = listed.then(|| {
            if shown < total { format!("{shown}/{total}").into() } else { total.to_string().into() }
        });
        let leading = Some(color_dots(&colors, 4, p.capsule, p).into_any_element());
        // The app's buttons keep their presses from what is under them, the popover's own
        // trigger included: the button opens it, and the popover closes it (a press outside, Esc).
        let trigger = capsule_button("calendar-list", leading, "Calendars", detail, self.calendar_list, listed, p)
            .debug_selector(|| "calendar-list-button".into())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.calendar_list = !this.calendar_list;
                cx.notify();
            }));
        if !listed {
            return trigger.into_any_element();
        }
        let pane = cx.entity().downgrade();
        Popover::new("calendar-list-popover")
            .appearance(false)
            .anchor(Anchor::TopRight)
            .offset(px(metrics::POPOVER_GAP))
            .trigger(trigger)
            .open(self.calendar_list)
            .on_open_change(move |&open, _, cx| {
                pane.update(cx, |this, cx| {
                    this.calendar_list = open;
                    cx.notify();
                })
                .ok();
            })
            .when(self.calendar_list, |popover| popover.child(self.render_calendar_list(p, cx)))
            .into_any_element()
    }

    /// Each account's calendars grouped into "My calendars" (owned) and "Other calendars"
    /// (shared or subscribed), with a color swatch checkbox per calendar, inline access hints,
    /// Show/Hide all and hover Solo ("Only").
    fn render_calendar_list(&self, p: &'static Palette, cx: &mut Context<Self>) -> impl IntoElement {
        popover(p)
            .id("calendar-list")
            .debug_selector(|| "calendar-list".into())
            .w(px(metrics::LIST_WIDTH))
            .max_h(px(metrics::LIST_HEIGHT))
            .overflow_y_scroll()
            .p(px(space::S6))
            .pb(px(space::S8))
            .text_size(px(text::BODY))
            .text_color(p.text)
            .children(self.calendar_list_rows("calendar", false, p, cx))
    }

    /// The "Calendars" section in the window's left sidebar while Calendar is in view.
    pub fn render_sidebar_section(&self, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rows = self.calendar_list_rows("sidebar-calendar", true, p, cx);
        (!rows.is_empty()).then(|| {
            div()
                .id("sidebar-calendars")
                .debug_selector(|| "sidebar-calendars".into())
                .mt(px(space::S6))
                .mb(px(space::S4))
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element()
        })
    }

    fn calendar_list_rows(
        &self,
        prefix: &str,
        sidebar: bool,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let granted: Vec<(usize, &mailstream::CalendarAccount)> = self
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, account)| account.granted && !account.calendars.is_empty())
            .collect();
        if granted.is_empty() {
            return Vec::new();
        }
        let total: usize = granted.iter().map(|(_, a)| a.calendars.len()).sum();
        let shown: usize = granted.iter().flat_map(|(_, a)| &a.calendars).filter(|c| c.shown).count();
        let all_shown = shown == total;
        let several_accounts = granted.len() > 1;

        let mut rows: Vec<AnyElement> = Vec::new();
        let show_all_id = format!("{prefix}-show-all");
        let show_all_sel = show_all_id.clone();
        rows.push(
            div()
                .h(px(if sidebar { 24. } else { metrics::LIST_ROW }))
                .px(px(space::S8))
                .flex()
                .items_center()
                .gap(px(space::S6))
                .when(!sidebar, |d| d.child(icon(Icon::Layers, icon_size::ROW, p.glyph)))
                .child(
                    div()
                        .text_size(px(if sidebar { text::CAPTION } else { text::FOOTNOTE }))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if sidebar { p.muted } else { p.text })
                        .child("Calendars"),
                )
                .child(div().text_size(px(text::CAPTION)).text_color(p.muted).child(format!("{shown}/{total}")))
                .child(div().flex_1())
                .child(
                    button(
                        ElementId::Name(show_all_id.into()),
                        if all_shown { "Hide all" } else { "Show all" },
                        ButtonKind::Plain(p.accent),
                        ControlSize::Small,
                        true,
                        p,
                    )
                    .debug_selector(move || show_all_sel.clone())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_all_calendars(!all_shown, cx))),
                )
                .into_any_element(),
        );

        for (pos, (index, account)) in granted.into_iter().enumerate() {
            if !sidebar || pos > 0 {
                rows.push(div().my(px(space::S4)).child(hairline(p)).into_any_element());
            }
            let account_title = if !account.label.is_empty() && account.label != account.id {
                format!("{} · {}", account.label, account.id)
            } else {
                account.id.clone()
            };
            let account_all = account.calendars.iter().all(|c| c.shown);
            let account_id = account.id.clone();
            rows.push(
                div()
                    .h(px(metrics::LIST_ROW - 2.))
                    .px(px(space::S8))
                    .flex()
                    .items_center()
                    .gap(px(space::S6))
                    .child(mark_dot(design::account_color(index)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(text::FOOTNOTE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(account_title),
                    )
                    .when(!sidebar && several_accounts && account.calendars.len() > 1, |d| {
                        d.child(
                            button(
                                ElementId::Name(format!("{prefix}-account-all:{account_id}").into()),
                                if account_all { "Hide all" } else { "Show all" },
                                ButtonKind::Plain(p.muted),
                                ControlSize::Small,
                                true,
                                p,
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_account_calendars(&account_id, !account_all, cx);
                            })),
                        )
                    })
                    .into_any_element(),
            );

            let mut mine: Vec<&CalendarInfo> = account.calendars.iter().filter(|c| c.owned()).collect();
            mine.sort_by_key(|c| (!c.primary, c.name.to_lowercase()));
            let mut others: Vec<&CalendarInfo> = account.calendars.iter().filter(|c| !c.owned()).collect();
            others.sort_by_key(|c| (!c.writable, c.name.to_lowercase()));

            for (group_title, group) in [("My calendars", mine), ("Other calendars", others)] {
                if group.is_empty() {
                    continue;
                }
                rows.push(
                    div()
                        .h(px(metrics::LIST_GROUP))
                        .px(px(space::S8))
                        .flex()
                        .items_end()
                        .pb(px(space::S2))
                        .text_size(px(text::TAG))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted)
                        .child(group_title)
                        .into_any_element(),
                );
                for calendar in group {
                    let color = dot_color(
                        model::hex_color(calendar.color.as_deref()).unwrap_or_else(|| design::account_color(index)),
                        p,
                    );
                    let meta: Option<SharedString> = match calendar.access.as_str() {
                        _ if calendar.primary && account.calendars.len() > 1 => Some("Primary".into()),
                        _ if calendar.owned() => None,
                        "freeBusyReader" => Some("Busy only".into()),
                        _ if calendar.writable => Some("Shared".into()),
                        _ => Some("Read only".into()),
                    };
                    let (account_id, calendar_id, calendar_shown) =
                        (account.id.clone(), calendar.id.clone(), calendar.shown);
                    let solo = (total > 1).then(|| {
                        let is_solo = shown == 1 && calendar_shown;
                        let (solo_account, solo_calendar) = (account_id.clone(), calendar_id.clone());
                        let solo_sel = format!("{prefix}-only:{account_id}:{calendar_id}");
                        button(
                            ElementId::Name(solo_sel.clone().into()),
                            if is_solo { "All" } else { "Only" },
                            ButtonKind::Secondary,
                            ControlSize::Small,
                            true,
                            p,
                        )
                        .debug_selector(move || solo_sel.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.solo_calendar(&solo_account, &solo_calendar, cx);
                        }))
                        .into_any_element()
                    });
                    let row_sel = format!("{prefix}-shown:{account_id}:{calendar_id}");
                    rows.push(
                        swatch_row(
                            ElementId::Name(row_sel.clone().into()),
                            calendar_shown,
                            color,
                            calendar.name.clone(),
                            meta,
                            solo,
                            p,
                        )
                        .debug_selector(move || row_sel.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.show_calendar(&account_id, &calendar_id, !calendar_shown, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
        }
        rows
    }

    /// This week's seven days: today raised on a card, the others over a bar as long as their
    /// booked hours. A day the agenda lists scrolls it there; another opens in the day view.
    fn render_week_strip(&self, today: NaiveDate, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let days = model::week_days(model::week_of(today));
        div().flex_shrink_0().h(px(metrics::WEEK_STRIP)).mx(px(space::S10)).mb(px(space::S8)).flex().children(
            days.into_iter().enumerate().map(|(slot, day)| {
                div()
                    .id(ElementId::Name(format!("week-day:{day}").into()))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .rounded(px(metrics::CARD_RADIUS))
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.strip_day(day, cx)))
                    .child(day_header(day, day == today, self.week_booked[slot], p))
            }),
        )
    }

    /// The agenda scrolls to `day`'s header when it lists the day; the day view shows days it
    /// does not (gone by, nothing planned).
    fn strip_day(&mut self, day: NaiveDate, cx: &mut Context<Self>) {
        let at = self.entries.iter().position(|entry| matches!(entry, Entry::Day { day: listed, .. } if *listed == day));
        match at {
            Some(item_ix) => {
                self.agenda.scroll_to(ListOffset { item_ix, offset_in_item: px(0.) });
                cx.notify();
            }
            None => self.show_day_view(day, cx),
        }
    }

    /// The meeting about to start or under way on one card: how soon, its title, and Join when
    /// it has a call; a click opens it. The pane's clock keeps it current.
    fn render_up_next(&self, now: DateTime<Local>, p: &'static Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let index = model::up_next(&self.events, now)?;
        let event = &self.events[index];
        let key = event.key.clone();
        let url = event.item.meeting_url.clone();
        let raised = p.raised_shadow.clone();
        Some(
            div()
                .flex_shrink_0()
                .px(px(space::S12))
                .pb(px(space::S8))
                .child(
                    div()
                        .id("up-next")
                        .h(px(metrics::UP_NEXT))
                        .px(px(space::S12))
                        .flex()
                        .items_center()
                        .gap(px(space::S8))
                        .rounded(px(metrics::CARD_RADIUS))
                        .bg(p.card)
                        .border(px(stroke::HAIRLINE))
                        .border_color(p.card_border)
                        .shadow(p.card_shadow.clone())
                        .cursor_pointer()
                        .hover(move |s| s.shadow(raised.clone()))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_clicked(key.clone(), event, cx)))
                        .child(mark_dot(p.accent))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(p.accent)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(model::up_next_label(event, now)),
                        )
                        .child(
                            div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(event.item.title.clone()),
                        )
                        .when_some(url, |d, url| {
                            d.child(
                                icon_label_button("up-next-join", Some(Icon::VideoFill), "Join", ButtonKind::Primary, ControlSize::Small, true, p)
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        cx.open_url(&url);
                                    }),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_entry(&self, ix: usize, now: DateTime<Local>, p: &'static Palette, cx: &mut Context<Self>) -> AnyElement {
        let line = match &self.entries[ix] {
            Entry::Notice(account) => self.render_notice(*account, p, cx).into_any_element(),
            Entry::Day { title, summary, .. } => div()
                .h(px(rows::SECTION_HEIGHT))
                .flex()
                .items_end()
                .gap(px(space::S8))
                .pb(px(5.))
                .pl(px(rows::SECTION_X - rows::INSET))
                .pr(px(space::S12))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(text::FOOTNOTE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted)
                        .child(title.clone()),
                )
                .child(div().flex_shrink_0().text_size(px(text::CAPTION)).text_color(p.muted).child(summary.clone()))
                .into_any_element(),
            Entry::Event { index, day } => self.render_row(*index, *day, now, p, cx),
            Entry::Note(note) => div()
                .h(px(metrics::NOTE_HEIGHT))
                .px(px(space::S24))
                .flex()
                .items_center()
                .justify_center()
                .text_center()
                .text_color(p.muted)
                .child(note.clone())
                .into_any_element(),
        };
        // The list's own padding stops at its top and bottom; lines step in by hand.
        div().px(px(rows::INSET)).child(line).into_any_element()
    }

    /// An account whose calendar cannot show: its calendar is not connected (Connect asks the
    /// provider in the browser, MS-306), or its last fetch failed. Its × closes it until that
    /// changes (MS-322).
    fn render_notice(&self, index: usize, p: &'static Palette, cx: &mut Context<Self>) -> Div {
        let account = &self.accounts[index];
        let failed = account.problem.clone().filter(|_| account.granted);
        let title = if account.label.is_empty() { account.id.clone() } else { account.label.clone() };
        let (tone, message) = match &failed {
            Some(problem) => (Tone::Error, format!("Could not update: {problem}")),
            None => (Tone::Action, "Connect this account's calendar to show its events here.".to_owned()),
        };
        // As tall as its message wraps to: the list measures each line.
        div().py(px(space::S4)).px(px(space::S12 - rows::INSET)).child(
            notice(tone, Some(title.into()), message, false, p).when(failed.is_none(), |d| {
                let waiting = self.connecting.as_ref() == Some(&account.id);
                let idle = self.connecting.is_none();
                let id = account.id.clone();
                d.child(
                    button(
                        ElementId::Name(format!("connect-calendar:{}", account.id).into()),
                        if waiting { "Waiting for Browser…" } else { "Connect Calendar…" },
                        ButtonKind::Secondary,
                        ControlSize::Small,
                        idle,
                        p,
                    )
                    .when(idle, |b| b.on_click(cx.listener(move |this, _, _, cx| this.connect_calendar(id.clone(), cx)))),
                )
            })
            .child(
                notice_close(ElementId::Name(format!("calendar-notice-close:{}", account.id).into()), p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.dismiss_account_notice(index, cx))),
            ),
        )
    }

    /// One event in the agenda: a time column (when it starts and how long it runs, or All day),
    /// its calendar's dot, the title and where it is, then its guests' avatars, or the answers
    /// when the account is invited and has not answered. Past events fade; declined ones fade
    /// further, struck through. Selected, the row lifts as a card.
    fn render_row(
        &self,
        index: usize,
        day: NaiveDate,
        now: DateTime<Local>,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let event = &self.events[index];
        let item = &event.item;
        let past = event.end <= now;
        let declined = event.declined();
        // A timed event shows its times on the day it starts; days it runs through say All day,
        // and the day it ends, when it ends.
        let starts = !item.all_day && event.start.date_naive() == day;
        let ends = !item.all_day && (event.end.date_naive() == day || event.end == model::midnight(model::next_day(day)));
        let time = if starts { model::clock(event.start) } else { "All day".to_owned() };
        let under = match (starts, ends) {
            (true, true) => Some(model::short_duration((event.end - event.start).num_minutes())),
            (false, true) => Some(format!("until {}", model::clock(event.end))),
            _ => None,
        };
        let several = self.accounts.iter().filter(|account| account.granted).count() > 1;
        let mut detail: Vec<String> = Vec::new();
        detail.extend(item.location.clone());
        detail.extend(item.meeting_url.as_deref().map(|url| model::meeting_label(url).to_owned()));
        if several || item.calendar_name.as_ref().is_some_and(|name| name != &item.account) {
            detail.push(item.calendar_name.clone().unwrap_or_else(|| item.account.clone()));
        }
        let detail = detail.join(" · ");
        let tag_label = match item.response.as_deref() {
            Some("needsAction") => Some("Invited"),
            Some("tentative") => Some("Maybe"),
            _ => None,
        }
        // On someone else's calendar the answer is theirs, not the account's.
        .filter(|_| item.own);
        let reply = self.reply(event);
        let selected = self.opened.as_ref() == Some(&event.key);
        let trailing: Option<AnyElement> = if reply.invited {
            Some(self.reply_switch(index, reply, "row", p, cx).into_any_element())
        } else {
            let guests = model::others(item);
            (!guests.is_empty()).then(|| guest_avatars(&guests, 3, p.card, p, cx).into_any_element())
        };
        let glyph = if item.location.is_some() {
            Some(Icon::Location)
        } else if item.meeting_url.is_some() {
            Some(Icon::Video)
        } else {
            None
        };
        let key = event.key.clone();
        let content = div()
            .relative()
            .size_full()
            .pl(px(space::S4))
            .pr(px(space::S12))
            .flex()
            .items_center()
            .when(past || declined, |d| d.opacity(if declined { metrics::DECLINED_OPACITY } else { metrics::PAST_OPACITY }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(metrics::TIME_WIDTH))
                    .flex()
                    .flex_col()
                    .items_end()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(time))
                    .when_some(under, |d, under| {
                        d.child(div().max_w_full().truncate().text_size(px(text::FOOTNOTE)).text_color(p.muted).child(under))
                    }),
            )
            .child(div().flex_shrink_0().w(px(space::S12)))
            .child(mark_dot(dot_color(self.event_color(event), p)))
            .child(div().flex_shrink_0().w(px(space::S8)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .when(declined, |d| d.line_through())
                            .child(item.title.clone()),
                    )
                    .when(!detail.is_empty() || tag_label.is_some(), |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(3.))
                                .when_some(tag_label, |d, label| {
                                    d.child(tag(label, Some(p.waiting), p).h(px(16.)).mr(px(3.)))
                                })
                                .when_some(glyph, |d, glyph| d.child(icon(glyph, icon_size::INLINE, p.muted)))
                                .child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(detail)),
                        )
                    }),
            )
            .when_some(trailing, |d, trailing| d.child(div().flex_shrink_0().w(px(space::S8))).child(trailing));
        list_row(ElementId::Name(format!("event:{}:{}:{}:{day}", key.account, key.calendar, key.id).into()), selected, false, p)
            .h(px(metrics::ROW_HEIGHT))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_clicked(key.clone(), event, cx)))
            .child(content)
            .into_any_element()
    }

    /// Going, Maybe and Decline side by side, the account's answer raised and checked; the
    /// answer being sent shows the clock. `place` keeps the row's and the detail's apart.
    pub(super) fn reply_switch(
        &self,
        index: usize,
        reply: ReplyState,
        place: &str,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> TabBar {
        let key = &self.events[index].key;
        let id = format!("reply:{place}:{}:{}:{}", key.account, key.calendar, key.id);
        let chosen = model::REPLIES.iter().position(|(value, _)| reply.chosen == Some(*value));
        segmented(ElementId::Name(id.into()), chosen, p).children(model::REPLIES.map(|(value, label)| {
            let tint = if value == "declined" { p.danger } else { p.accent };
            let chosen = reply.chosen == Some(value);
            let glyph = if reply.sending == Some(value) {
                Some(Icon::Waiting)
            } else if chosen {
                Some(Icon::Check)
            } else {
                None
            };
            let enabled = reply.answerable && reply.sending.is_none() && !chosen;
            let target = key.clone();
            segment(label, glyph.map(|glyph| (glyph, tint)), chosen.then_some(tint))
                // Never opens the row under it.
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    if enabled {
                        this.respond(&target, value, cx);
                    }
                }))
        }))
    }
}

/// A calendar's dot: its color a little toward the card under it.
pub(super) fn dot_color(color: Hsla, p: &Palette) -> Hsla {
    color.blend(design::tint(p.card, metrics::DOT_SOFTEN))
}

/// Guests as overlapping avatars, each ringed in `ring` (the surface under them): the first
/// `max`, then +N for the rest.
pub(super) fn guest_avatars(guests: &[&CalendarPerson], max: usize, ring: Hsla, p: &'static Palette, cx: &mut gpui::App) -> Div {
    let size = metrics::AVATAR;
    let step = size * metrics::AVATAR_STEP;
    let ring_width = metrics::AVATAR_RING;
    let shown = guests.len().min(max);
    let rest = guests.len() - shown;
    let count = shown + usize::from(rest > 0);
    let disc = |slot: usize| {
        div()
            .absolute()
            .left(px(slot as f32 * step - ring_width))
            .top(px(-ring_width))
            .size(px(size + 2. * ring_width))
            .p(px(ring_width))
            .rounded_full()
            .bg(ring)
    };
    div()
        .relative()
        .flex_shrink_0()
        .w(px(step * (count.max(1) - 1) as f32 + size))
        .h(px(size))
        .children(
            guests[..shown]
                .iter()
                .enumerate()
                .map(|(slot, person)| {
                    let address = guest_address(person);
                    disc(slot).child(avatars::avatar(&address.email, &mailstream::Avatar::of(&address), size, p, cx))
                }),
        )
        .when(rest > 0, |d| {
            d.child(
                disc(shown).child(
                    div()
                        .size_full()
                        .rounded_full()
                        .bg(p.chip)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(size * design::avatar::LETTER))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted)
                        .child(format!("+{rest}")),
                ),
            )
        })
}
