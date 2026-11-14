//! The tag editor (MS-339, `state/tags.rs`): a popover over the window with the conversations'
//! category, four choices, and their topic labels, each with a remove button, a field for a new
//! one and the labels the lists already use to pick from. Esc or a press outside closes it.

use super::*;
use crate::controls::{field_frame, popover};
use gpui::component::input::Escape;
use gpui::{anchored, deferred, point};

/// The editor's width.
const WIDTH: f32 = 296.;
/// How far under the window's top it opens from the keyboard or the menu bar.
const TOP: f32 = 72.;
/// A category choice's height.
const CHOICE: f32 = 30.;
/// Labels offered at most.
const OFFERED: usize = 8;

impl MailApp {
    pub(super) fn render_tag_editor(&self, p: &'static Palette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.tag_editor.as_ref()?;
        let targets = self.tag_targets();
        if targets.is_empty() || !self.mail_in_view() {
            return None;
        }
        let chosen = targets[0].category.filter(|category| targets.iter().all(|c| c.category == Some(*category)));
        let tagged = targets.iter().any(|c| c.category.is_some());
        // The conversations' labels, in the order they first come.
        let mut current: Vec<String> = Vec::new();
        for label in targets.iter().flat_map(|c| &c.labels) {
            if !current.contains(label) {
                current.push(label.clone());
            }
        }
        let typed = editor.input.read(cx).value().trim().to_lowercase();
        let offered: Vec<String> = self
            .known_labels()
            .into_iter()
            .filter(|label| !current.contains(label) && label.to_lowercase().contains(&typed))
            .take(OFFERED)
            .collect();
        let title: SharedString = match &targets[..] {
            [one] if one.title.is_empty() => "(no subject)".into(),
            [one] => one.title.clone().into(),
            many => format!("{} conversations", many.len()).into(),
        };

        let heading = |text: &'static str| div().text_size(px(text::FOOTNOTE)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child(text);
        let mut choices = Category::ALL.into_iter().map(|category| {
            let color = design::category_color(category, p);
            let on = chosen == Some(category);
            div()
                .id(ElementId::Name(format!("tag-category:{}", category.as_str()).into()))
                .flex_1()
                .min_w_0()
                .h(px(CHOICE))
                .px(px(space::S10))
                .flex()
                .items_center()
                .gap(px(space::S8))
                .rounded(px(radius::CONTROL))
                .border(px(stroke::HAIRLINE))
                .cursor_pointer()
                .when(on, |d| d.bg(design::tint(color, 0.14)).border_color(design::tint(color, 0.6)).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.border_color(p.border).hover(|s| s.bg(p.hover)))
                .child(icon(Icon::for_category(category), icon_size::ROW, color))
                .child(div().flex_1().min_w_0().truncate().child(category.label()))
                .when(on, |d| d.child(icon(Icon::Check, icon_size::INLINE, color)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_category(category, cx)))
        });
        // Two to a row.
        let mut rows = Vec::new();
        while let Some(first) = choices.next() {
            rows.push(div().flex().gap(px(space::S6)).child(first).children(choices.next()));
        }
        let held = current.into_iter().enumerate().map(|(ix, label)| {
            let dot = mark_dot(design::topic_color(&label, p.dark)).into_any_element();
            pill(label.clone(), Some(dot), p).pr(px(space::S4)).child(
                div()
                    .id(ElementId::NamedInteger("tag-remove".into(), ix as u64))
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .child(icon(Icon::Close, icon_size::INLINE, p.muted))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.remove_label(label.clone(), cx))),
            )
        });
        let offers = offered.into_iter().enumerate().map(|(ix, label)| {
            let color = design::topic_color(&label, p.dark);
            div()
                .id(ElementId::NamedInteger("tag-offer".into(), ix as u64))
                .cursor_pointer()
                .child(tag(label.clone(), Some(color), p))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.add_label(label.clone(), cx)))
        });
        let focused = editor.input.focus_handle(cx).is_focused(window);
        let labels = div()
            .flex()
            .flex_col()
            .gap(px(space::S8))
            .child(heading("Labels"))
            .child(div().flex().flex_wrap().gap(px(space::S6)).children(held))
            .child(
                field_frame(focused, false, p)
                    .h(px(CHOICE))
                    .px(px(space::S8))
                    .child(div().flex_1().min_w_0().child(input::field(&editor.input))),
            )
            .when(!tagged, |d| d.child(div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child("Pick a category first: labels go with it.")))
            .when(tagged, |d| {
                d.child(div().flex().flex_wrap().gap(px(space::S6)).children(offers)).child(
                    div().text_size(px(text::FOOTNOTE)).text_color(p.muted).child("Two at most: a new label comes first and replaces the second."),
                )
            });

        let panel = popover(p)
            .id("tag-editor")
            .w(px(WIDTH))
            .p(px(space::S12))
            .flex()
            .flex_col()
            .gap(px(space::S12))
            .occlude()
            // A press in it never reaches the list, the pages or the title bar under it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_tag_editor(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_tag_editor(window, cx)))
            .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::S8))
                    .child(heading("Category"))
                    .children(rows),
            )
            .child(labels);
        let position = editor.position.unwrap_or_else(|| {
            let width = f32::from(window.viewport_size().width);
            point(px(((width - WIDTH) / 2.).max(0.)), px(TOP))
        });
        Some(deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(panel)).into_any_element())
    }
}
