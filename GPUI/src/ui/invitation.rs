//! A meeting invitation in a message (Flutter's `InvitationCard`, MS-210): when and where, who
//! organizes it, and the account's answer with Accept / Tentative / Decline. The message's own
//! words show at once; the calendar's event replaces them once it is read
//! (`state/invitations.rs`).

use chrono::Local;
use mailstream::{CalendarPerson, InvitationStatus, MeetingInvitation};

use super::*;
use crate::controls::{segment, segmented};

/// The answers an invitation takes, as the engine names them, with their labels.
const ANSWERS: [(&str, &str); 3] = [("accepted", "Accept"), ("tentative", "Tentative"), ("declined", "Decline")];

/// The tag saying how the account answered.
fn answered(response: Option<&str>) -> Option<&'static str> {
    match response? {
        "accepted" => Some("You accepted"),
        "tentative" => Some("You said maybe"),
        "declined" => Some("You declined"),
        _ => None,
    }
}

fn person(person: &CalendarPerson) -> String {
    match (person.name.as_deref(), person.email.as_deref()) {
        (Some(name), Some(email)) if name != email => format!("{name} <{email}>"),
        (Some(name), _) => name.to_owned(),
        (None, Some(email)) => email.to_owned(),
        (None, None) => "someone".to_owned(),
    }
}

/// Why it cannot be answered here, when it cannot.
fn why_not(status: Option<&InvitationStatus>, cancelled: bool) -> Option<String> {
    if cancelled {
        return Some("This meeting was cancelled.".into());
    }
    let status = status?;
    if !status.granted || !status.editable {
        return Some(format!("Sign in to {} again (Settings › Accounts) to answer from here.", status.account));
    }
    if let Some(problem) = &status.problem {
        return Some(format!("Could not read the calendar: {problem}"));
    }
    match &status.event {
        None => Some("Not in your calendar yet.".into()),
        Some(event) if event.response.as_deref() == Some("organizer") => Some("You organized this meeting.".into()),
        Some(_) => None,
    }
}

impl MailApp {
    pub(super) fn render_invitation(
        &self,
        message: &Message,
        invitation: &MeetingInvitation,
        p: &'static Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let card = self.invitations.get(&message.id);
        let status = card.and_then(|card| card.status.as_ref());
        let event = status.and_then(|status| status.event.as_ref());
        let cancelled = invitation.method == "cancel";
        let response = event.and_then(|event| event.response.as_deref());
        let organizer = event.and_then(|event| event.organizer.as_ref()).or(invitation.organizer.as_ref());
        let location = event.and_then(|event| event.location.clone()).or_else(|| invitation.location.clone());
        let title = event.map_or(&invitation.title, |event| &event.title).clone();
        let mut when = match event {
            Some(event) => format::meeting_when(
                event.all_day,
                (event.start_day.as_deref(), event.end_day.as_deref()),
                &event.start,
                &event.end,
                Local::now(),
            ),
            None => format::meeting_when(
                invitation.all_day,
                (invitation.start_day.as_deref(), invitation.end_day.as_deref()),
                &invitation.start,
                &invitation.end,
                Local::now(),
            ),
        };
        if invitation.recurring {
            when.push_str(" · Repeats");
        }
        let answerable = !cancelled
            && status.is_some_and(|status| status.target.is_some() && status.editable)
            && response != Some("organizer");
        let sending = card.and_then(|card| card.sending);
        let chosen = sending.or(response);
        let line = |glyph: Icon, value: String| {
            div()
                .pt(px(space::S6))
                .flex()
                .items_start()
                .gap(px(space::S8))
                .child(div().pt(px(1.)).child(icon(glyph, icon_size::ROW, p.muted)))
                .child(div().flex_1().min_w_0().child(value))
        };
        let answers = ANSWERS.map(|(value, label)| {
            let tint = if value == "declined" { p.danger } else { p.accent };
            let glyph = match sending {
                Some(sent) if sent == value => Some(Icon::Waiting),
                _ if chosen == Some(value) => Some(Icon::Check),
                _ => None,
            };
            let id = message.id.clone();
            segment(ElementId::Name(format!("invitation:{}:{value}", message.id).into()), "", chosen == Some(value), p)
                .gap(px(space::S4))
                .when(chosen == Some(value), |d| d.text_color(tint))
                .when_some(glyph, |d, glyph| d.child(icon(glyph, icon_size::INLINE, tint)))
                .child(label)
                .when(sending.is_none() && chosen != Some(value), |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.respond_invitation(&id, value, cx)))
                })
        });
        div()
            .px(px(14.))
            .py(px(space::S12))
            .flex()
            .flex_col()
            .rounded(px(radius::PANE))
            .bg(p.background)
            .border(px(stroke::RIM))
            .border_color(p.line)
            .text_size(px(text::BODY))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::S8))
                    .child(icon(Icon::Calendar, 18., if cancelled { p.danger } else { p.accent }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .line_clamp(2)
                            .font_weight(FontWeight::SEMIBOLD)
                            .when(cancelled || response == Some("declined"), |d| d.line_through())
                            .child(title),
                    )
                    .when(cancelled, |d| d.child(tag("Cancelled", Some(p.danger), p)))
                    .when_some(answered(response).filter(|_| !cancelled), |d, label| {
                        d.child(tag(label, (response != Some("declined")).then_some(p.accent), p))
                    }),
            )
            .child(line(Icon::Waiting, when))
            .when_some(location, |d, location| d.child(line(Icon::Location, location)))
            .when_some(organizer.filter(|_| response != Some("organizer")), |d, organizer| {
                d.child(line(Icon::Person, format!("Organized by {}", person(organizer))))
            })
            .when_some(why_not(status, cancelled), |d, note| d.child(div().pt(px(space::S8)).text_color(p.muted).child(note)))
            .when(answerable, |d| {
                d.child(
                    div()
                        .pt(px(space::S10))
                        .flex()
                        .items_center()
                        .gap(px(space::S10))
                        .child(div().text_color(p.muted).child("Going?"))
                        .child(segmented(p).children(answers)),
                )
            })
            .when_some(card.and_then(|card| card.error.clone()), |d, error| {
                d.child(div().pt(px(space::S8)).text_color(p.danger).child(error))
            })
    }
}
