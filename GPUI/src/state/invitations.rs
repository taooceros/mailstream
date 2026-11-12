//! Meeting invitations in the reader (Flutter's `InvitationCard`, MS-210): each message carrying
//! one asks the account's calendar (`calendar_invitation`) for the event it names and the
//! account's answer, and answers it there (`calendar_respond`), which tells the organizer. The
//! cards follow the calendar: every change it reports (`Event::Calendar`) reads them again.

use gpui::{Context, SharedString};
use mailstream::InvitationStatus;

use super::MailApp;
use crate::engine::engine;
use crate::model::ShownMessage;

/// What the reader knows of one message's invitation beyond what the message says.
pub struct InvitationCard {
    conversation: String,
    /// The calendar's answer, once read.
    pub status: Option<InvitationStatus>,
    /// The answer being sent, until the calendar takes it.
    pub sending: Option<&'static str>,
    pub error: Option<SharedString>,
    reading: bool,
    /// The calendar changed while it was being read: read it once more.
    again: bool,
}

impl MailApp {
    /// Reads the calendar for the invitations in `messages` of `conversation` it has not read yet.
    pub(super) fn read_invitations(&mut self, conversation: &str, messages: &[ShownMessage], cx: &mut Context<Self>) {
        for message in messages.iter().filter(|message| message.invitation.is_some()) {
            if self.invitations.contains_key(&message.id) {
                continue;
            }
            let card = InvitationCard {
                conversation: conversation.to_owned(),
                status: None,
                sending: None,
                error: None,
                reading: false,
                again: false,
            };
            self.invitations.insert(message.id.clone(), card);
            self.read_invitation(message.id.clone(), cx);
        }
    }

    /// The calendar changed: every card may show another answer.
    pub(super) fn calendar_changed(&mut self, cx: &mut Context<Self>) {
        let messages: Vec<String> = self.invitations.keys().cloned().collect();
        for message in messages {
            self.read_invitation(message, cx);
        }
    }

    fn read_invitation(&mut self, message: String, cx: &mut Context<Self>) {
        let Some(card) = self.invitations.get_mut(&message) else { return };
        if card.reading {
            card.again = true;
            return;
        }
        card.reading = true;
        let (id, asked) = (card.conversation.clone(), message.clone());
        self.background(cx, async move { engine().calendar_invitation(id, asked).await }, move |this, result, cx| {
            let Some(card) = this.invitations.get_mut(&message) else { return };
            card.reading = false;
            match result {
                Ok(status) => {
                    card.status = Some(status);
                    if card.sending.is_none() {
                        card.error = None;
                    }
                }
                Err(error) => card.error = Some(error.to_string().into()),
            }
            if std::mem::take(&mut card.again) {
                this.read_invitation(message, cx);
            }
            cx.notify();
        });
    }

    /// Answers `message`'s invitation (`accepted`, `tentative` or `declined`) through the
    /// calendar, then reads the card again.
    pub fn respond_invitation(&mut self, message: &str, response: &'static str, cx: &mut Context<Self>) {
        let Some(card) = self.invitations.get_mut(message) else { return };
        let Some(status) = card.status.as_ref() else { return };
        let Some(target) = status.target.clone() else { return };
        if card.sending.is_some() {
            return;
        }
        card.sending = Some(response);
        card.error = None;
        let (account, calendar) = (status.account.clone(), status.calendar.clone());
        let (id, asked) = (card.conversation.clone(), message.to_owned());
        let message = message.to_owned();
        cx.notify();
        self.background(
            cx,
            async move {
                let answered = engine().calendar_respond(account, calendar, target, response.to_owned(), true).await;
                answered.map_err(|error| format!("Could not answer: {error}"))?;
                engine().calendar_invitation(id, asked).await.map_err(|error| error.to_string())
            },
            move |this, result, cx| {
                let Some(card) = this.invitations.get_mut(&message) else { return };
                card.sending = None;
                match result {
                    Ok(status) => card.status = Some(status),
                    Err(error) => card.error = Some(error.into()),
                }
                cx.notify();
            },
        );
    }
}
