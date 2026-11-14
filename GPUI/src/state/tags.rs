//! Tagging by hand (MS-339): the tag editor sets the category and the topic labels of the
//! conversations an action applies to, as the AI tagger would (`mail.tag`, which keeps them as
//! the user's and syncs them where tags sync). Each change shows at once, rolls back when the
//! engine refuses it and is offered for undo, like every action on mail. Drawn in `ui/tags.rs`.

use super::*;
use crate::engine;
use crate::model::CategoryExt;

/// The tag editor over the window: what it applies to and where it opened.
pub struct TagEditor {
    pub ids: Vec<String>,
    /// The point it opened at (a context menu's, a pill's); None: centered under the window's top.
    pub position: Option<Point<Pixels>>,
    /// Where a new topic label is written; it also filters the labels offered.
    pub input: Entity<InputState>,
    _subscription: Subscription,
}

/// What one change asks of the engine.
enum TagChange {
    Category(Category),
    AddLabel(String),
    RemoveLabel(String),
}

impl MailApp {
    /// `l` and Message ▸ Tag…: the editor for what the action applies to.
    pub fn edit_tags(&mut self, _: &EditTags, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.context_menu.as_ref().map(|menu| menu.position);
        let ids = self.take_targets(window, cx).into_iter().map(|conversation| conversation.id).collect();
        self.open_tag_editor(ids, at, window, cx);
    }

    /// Opens the tag editor on `ids` at `position` (else centered under the window's top), the
    /// keys in its label field when any of them has a category to keep labels with.
    pub fn open_tag_editor(&mut self, ids: Vec<String>, position: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() || !self.mail_in_view() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a label"));
        let subscription = cx.subscribe_in(&input, window, Self::on_label_input);
        self.tag_editor = Some(TagEditor { ids, position, input: input.clone(), _subscription: subscription });
        if self.tag_targets().iter().any(|conversation| conversation.category.is_some()) {
            window.focus(&input.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Esc, a press outside: the editor closes, the keys back on the list.
    pub fn close_tag_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tag_editor.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn on_label_input(&mut self, input: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::PressEnter { .. } => {
                let label = input.read(cx).value().trim().to_owned();
                if !label.is_empty() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    self.add_label(label, cx);
                }
            }
            InputEvent::Change => cx.notify(),
            _ => {}
        }
    }

    /// The conversations the editor is open on, as shown now: in the list, else in the reader.
    pub fn tag_targets(&self) -> Vec<Conversation> {
        let Some(editor) = &self.tag_editor else { return Vec::new() };
        editor
            .ids
            .iter()
            .filter_map(|id| self.conversation(id).or_else(|| self.reader().filter(|open| &open.id == id).cloned()))
            .collect()
    }

    /// The topic labels of the lists in memory, most used first: what the editor offers.
    pub fn known_labels(&self) -> Vec<String> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for conversation in self.lists.values().flat_map(|list| &list.conversations) {
            for label in &conversation.labels {
                *counts.entry(label.as_str()).or_default() += 1;
            }
        }
        let mut labels: Vec<(&str, usize)> = counts.into_iter().collect();
        labels.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        labels.into_iter().map(|(label, _)| label.to_owned()).collect()
    }

    /// Puts the editor's conversations in `category`; a newsletter never waits on the user.
    pub fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        let targets: Vec<Conversation> =
            self.tag_targets().into_iter().filter(|conversation| conversation.category != Some(category)).collect();
        self.change_tags(targets, TagChange::Category(category), cx);
    }

    /// Adds `label` to the editor's conversations that have a category and lack it.
    pub fn add_label(&mut self, label: String, cx: &mut Context<Self>) {
        let targets: Vec<Conversation> = self
            .tag_targets()
            .into_iter()
            .filter(|conversation| {
                conversation.category.is_some()
                    && mailstream::edited_labels(&conversation.labels, std::slice::from_ref(&label), &[]) != conversation.labels
            })
            .collect();
        self.change_tags(targets, TagChange::AddLabel(label), cx);
    }

    /// Takes `label` off the editor's conversations.
    pub fn remove_label(&mut self, label: String, cx: &mut Context<Self>) {
        let targets: Vec<Conversation> =
            self.tag_targets().into_iter().filter(|conversation| conversation.labels.contains(&label)).collect();
        self.change_tags(targets, TagChange::RemoveLabel(label), cx);
    }

    /// Shows `change` on `targets` at once and asks the engine for it in one call; what it
    /// refuses goes back to how it was.
    fn change_tags(&mut self, targets: Vec<Conversation>, change: TagChange, cx: &mut Context<Self>) {
        if targets.is_empty() {
            return;
        }
        let before: HashMap<String, (Option<Category>, bool, Vec<String>)> = targets
            .iter()
            .map(|conversation| (conversation.id.clone(), (conversation.category, conversation.waiting, conversation.labels.clone())))
            .collect();
        for conversation in &targets {
            let id = conversation.id.as_str();
            match &change {
                TagChange::Category(category) => self.patch(id, |c| {
                    c.category = Some(*category);
                    if *category == Category::Newsletter {
                        c.waiting = false;
                    }
                }),
                TagChange::AddLabel(label) => {
                    self.patch(id, |c| c.labels = mailstream::edited_labels(&c.labels, std::slice::from_ref(label), &[]))
                }
                TagChange::RemoveLabel(label) => {
                    self.patch(id, |c| c.labels = mailstream::edited_labels(&c.labels, &[], std::slice::from_ref(label)))
                }
            }
        }
        self.rebuild_items();
        cx.notify();
        let ids: Vec<String> = targets.into_iter().map(|conversation| conversation.id).collect();
        let count = ids.len();
        let (text, failure): (SharedString, &'static str) = match &change {
            TagChange::Category(category) => (
                if count == 1 {
                    format!("Moved to {}", category.label()).into()
                } else {
                    format!("Moved {count} conversations to {}", category.label()).into()
                },
                "Couldn't change the category",
            ),
            TagChange::AddLabel(label) => (
                if count == 1 { format!("Labeled “{label}”").into() } else { format!("Labeled {count} conversations “{label}”").into() },
                "Couldn't add the label",
            ),
            TagChange::RemoveLabel(label) => (
                if count == 1 {
                    format!("Removed “{label}”").into()
                } else {
                    format!("Removed “{label}” from {count} conversations").into()
                },
                "Couldn't remove the label",
            ),
        };
        let asked = ids.clone();
        let work = async move {
            let (category, add, remove) = match change {
                TagChange::Category(category) => (Some(category.as_str().to_owned()), None, None),
                TagChange::AddLabel(label) => (None, Some(vec![label]), None),
                TagChange::RemoveLabel(label) => (None, None, Some(vec![label])),
            };
            let answer = engine().tag(asked.clone(), category, None, None, add, remove).await;
            engine::outcome(answer.map(|result| (result.failed, result.undo)), &asked)
        };
        self.background(cx, work, move |this, outcome, cx| {
            match &outcome.error {
                Some(error) => {
                    for id in &outcome.failed {
                        if let Some((category, waiting, labels)) = before.get(id) {
                            this.patch(id, |c| {
                                c.category = *category;
                                c.waiting = *waiting;
                                c.labels = labels.clone();
                            });
                        }
                    }
                    this.rebuild_items();
                    this.show_toast(format!("{failure}: {error}"), cx);
                    cx.notify();
                }
                None => this.offer_undo(text, outcome.undo, cx),
            }
        });
    }
}
