//! Converts HTML fragments (from the system clipboard, Blitz or WebKit) into clean Markdown
//! matching what `Engine/src/mail/sending.rs` (`markdown_to_html`) sends.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::HashSet;

use html5ever::interface::{Attribute, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink as _};
use html5ever::{ExpandedName, ParseOpts, QualName, ns};

use crate::mail_html::is_mail_link;

#[derive(Clone, Debug)]
enum NodeKind {
    Document,
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
    },
    Text(String),
    Other,
}

#[derive(Clone, Debug)]
struct Node {
    kind: NodeKind,
    parent: Option<usize>,
    children: Vec<usize>,
}

struct DomSink {
    nodes: RefCell<Vec<Node>>,
    quirks: RefCell<QuirksMode>,
}

impl DomSink {
    fn new() -> Self {
        Self {
            nodes: RefCell::new(vec![Node {
                kind: NodeKind::Document,
                parent: None,
                children: Vec::new(),
            }]),
            quirks: RefCell::new(QuirksMode::NoQuirks),
        }
    }

    fn alloc(&self, kind: NodeKind) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        let id = nodes.len();
        nodes.push(Node { kind, parent: None, children: Vec::new() });
        id
    }

    fn detach(&self, id: usize) {
        let mut nodes = self.nodes.borrow_mut();
        if let Some(parent) = nodes[id].parent.take() {
            nodes[parent].children.retain(|&c| c != id);
        }
    }
}

impl TreeSink for DomSink {
    type Handle = usize;
    type Output = Vec<Node>;
    type ElemName<'a>
        = Ref<'a, QualName>
    where
        Self: 'a;

    fn finish(self) -> Self::Output {
        self.nodes.into_inner()
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Self::Handle {
        0
    }

    fn elem_name<'a>(&'a self, target: &'a Self::Handle) -> Self::ElemName<'a> {
        Ref::map(self.nodes.borrow(), |nodes| match &nodes[*target].kind {
            NodeKind::Element { name, .. } => name,
            _ => unreachable!("elem_name called on non-element"),
        })
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, _flags: ElementFlags) -> Self::Handle {
        self.alloc(NodeKind::Element { name, attrs })
    }

    fn create_comment(&self, _text: StrTendril) -> Self::Handle {
        self.alloc(NodeKind::Other)
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> Self::Handle {
        self.alloc(NodeKind::Other)
    }

    fn append(&self, parent: &Self::Handle, child: NodeOrText<Self::Handle>) {
        match child {
            NodeOrText::AppendNode(id) => {
                self.detach(id);
                let mut nodes = self.nodes.borrow_mut();
                nodes[id].parent = Some(*parent);
                nodes[*parent].children.push(id);
            }
            NodeOrText::AppendText(text) => {
                let mut nodes = self.nodes.borrow_mut();
                if let Some(&last) = nodes[*parent].children.last()
                    && let NodeKind::Text(existing) = &mut nodes[last].kind
                {
                    existing.push_str(&text);
                    return;
                }
                let id = nodes.len();
                nodes.push(Node {
                    kind: NodeKind::Text(text.into()),
                    parent: Some(*parent),
                    children: Vec::new(),
                });
                nodes[*parent].children.push(id);
            }
        }
    }

    fn append_based_on_parent_node(&self, element: &Self::Handle, prev_element: &Self::Handle, child: NodeOrText<Self::Handle>) {
        let has_parent = self.nodes.borrow()[*element].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, _name: StrTendril, _public_id: StrTendril, _system_id: StrTendril) {}

    fn get_template_contents(&self, target: &Self::Handle) -> Self::Handle {
        *target
    }

    fn same_node(&self, x: &Self::Handle, y: &Self::Handle) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        *self.quirks.borrow_mut() = mode;
    }

    fn append_before_sibling(&self, sibling: &Self::Handle, new_node: NodeOrText<Self::Handle>) {
        let Some(parent) = self.nodes.borrow()[*sibling].parent else { return };
        match new_node {
            NodeOrText::AppendNode(id) => {
                self.detach(id);
                let mut nodes = self.nodes.borrow_mut();
                let pos = nodes[parent].children.iter().position(|&c| c == *sibling).unwrap_or(nodes[parent].children.len());
                nodes[id].parent = Some(parent);
                nodes[parent].children.insert(pos, id);
            }
            NodeOrText::AppendText(text) => {
                let mut nodes = self.nodes.borrow_mut();
                let pos = nodes[parent].children.iter().position(|&c| c == *sibling).unwrap_or(nodes[parent].children.len());
                if pos > 0 {
                    let prev = nodes[parent].children[pos - 1];
                    if let NodeKind::Text(existing) = &mut nodes[prev].kind {
                        existing.push_str(&text);
                        return;
                    }
                }
                let id = nodes.len();
                nodes.push(Node {
                    kind: NodeKind::Text(text.into()),
                    parent: Some(parent),
                    children: Vec::new(),
                });
                nodes[parent].children.insert(pos, id);
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &Self::Handle, attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
        if let NodeKind::Element { attrs: existing, .. } = &mut nodes[*target].kind {
            let names: HashSet<ExpandedName<'_>> = existing.iter().map(|a| a.name.expanded()).collect();
            let to_add: Vec<Attribute> = attrs.into_iter().filter(|a| !names.contains(&a.name.expanded())).collect();
            existing.extend(to_add);
        }
    }

    fn remove_from_parent(&self, target: &Self::Handle) {
        self.detach(*target);
    }

    fn reparent_children(&self, node: &Self::Handle, new_parent: &Self::Handle) {
        let mut nodes = self.nodes.borrow_mut();
        let children = std::mem::take(&mut nodes[*node].children);
        for &child in &children {
            nodes[child].parent = Some(*new_parent);
        }
        nodes[*new_parent].children.extend(children);
    }
}

/// Converts an HTML document or fragment into clean Markdown.
pub fn html_to_markdown(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let nodes = html5ever::parse_document(DomSink::new(), ParseOpts::default()).one(html);
    let root = find_body(&nodes).unwrap_or(0);
    let mut ctx = RenderCtx { nodes: &nodes };
    let out = ctx.render_block_children(root, false);
    trim_markdown(&out)
}

fn find_body(nodes: &[Node]) -> Option<usize> {
    nodes.iter().position(|node| matches!(&node.kind, NodeKind::Element { name, .. } if name.ns == ns!(html) && &*name.local == "body"))
}

struct RenderCtx<'a> {
    nodes: &'a [Node],
}

impl<'a> RenderCtx<'a> {
    fn tag(&self, id: usize) -> Option<&str> {
        match &self.nodes[id].kind {
            NodeKind::Element { name, .. } if name.ns == ns!(html) => Some(&name.local),
            _ => None,
        }
    }

    fn attr(&self, id: usize, attr_name: &str) -> Option<&str> {
        match &self.nodes[id].kind {
            NodeKind::Element { attrs, .. } => attrs
                .iter()
                .find(|a| (&*a.name.local).eq_ignore_ascii_case(attr_name))
                .map(|a| a.value.as_ref()),
            _ => None,
        }
    }

    fn style_prop(&self, id: usize, prop_name: &str) -> Option<&str> {
        let style = self.attr(id, "style")?;
        let mut matched = None;
        for decl in style.split(';') {
            let Some((k, v)) = decl.split_once(':') else { continue };
            if k.trim().eq_ignore_ascii_case(prop_name) {
                matched = Some(v.trim());
            }
        }
        matched
    }

    fn is_hidden(&self, id: usize) -> bool {
        if self.attr(id, "hidden").is_some() {
            return true;
        }
        if let Some(display) = self.style_prop(id, "display")
            && display.eq_ignore_ascii_case("none")
        {
            return true;
        }
        false
    }

    fn is_skipped(&self, id: usize) -> bool {
        self.is_hidden(id)
            || matches!(
                self.tag(id),
                Some("head" | "script" | "style" | "noscript" | "template" | "meta" | "link" | "title" | "svg")
            )
    }

    fn render_block_children(&mut self, parent: usize, in_table_cell: bool) -> String {
        let mut blocks: Vec<String> = Vec::new();
        let mut inline_buf = String::new();

        let flush_inline = |inline_buf: &mut String, blocks: &mut Vec<String>| {
            let trimmed = trim_inline_lines(inline_buf);
            inline_buf.clear();
            if !trimmed.is_empty() {
                blocks.push(trimmed);
            }
        };

        for &child in &self.nodes[parent].children {
            if self.is_skipped(child) {
                continue;
            }
            match self.tag(child) {
                None => {
                    if let NodeKind::Text(text) = &self.nodes[child].kind {
                        let collapsed = collapse_ws(text);
                        push_inline_text(&mut inline_buf, &collapsed);
                    }
                }
                Some(tag) if is_block_tag(tag) => {
                    flush_inline(&mut inline_buf, &mut blocks);
                    let rendered = self.render_block_node(child, in_table_cell);
                    if !rendered.is_empty() {
                        blocks.push(rendered);
                    }
                }
                Some(_) => {
                    let rendered = self.render_inline(child, in_table_cell, false);
                    push_inline_text(&mut inline_buf, &rendered);
                }
            }
        }
        flush_inline(&mut inline_buf, &mut blocks);
        blocks.join(if in_table_cell { " " } else { "\n\n" })
    }

    fn render_block_node(&mut self, id: usize, in_table_cell: bool) -> String {
        if self.is_skipped(id) {
            return String::new();
        }
        let Some(tag) = self.tag(id) else { return String::new() };
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<usize>().unwrap_or(1).clamp(1, 6);
                let text = self.render_inline_children(id, true, false);
                let text = collapse_single_line(&text);
                if text.is_empty() {
                    String::new()
                } else {
                    format!("{} {text}", "#".repeat(level))
                }
            }
            "blockquote" => {
                let inner = self.render_block_children(id, false);
                let trimmed = trim_markdown(&inner);
                if trimmed.is_empty() {
                    String::new()
                } else {
                    trimmed
                        .lines()
                        .map(|line| if line.is_empty() { ">".to_string() } else { format!("> {line}") })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "pre" => {
                let lang = self.code_lang(id);
                let mut code = String::new();
                self.collect_pre_text(id, &mut code);
                let code = code.strip_prefix('\n').unwrap_or(&code).trim_end_matches('\n');
                if code.is_empty() {
                    return String::new();
                }
                let fence = fence_for(code);
                if let Some(lang) = lang {
                    format!("{fence}{lang}\n{code}\n{fence}")
                } else {
                    format!("{fence}\n{code}\n{fence}")
                }
            }
            "ul" => self.render_list(id, None),
            "ol" => {
                let start = self.attr(id, "start").and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1);
                self.render_list(id, Some(start))
            }
            "hr" => "---".to_string(),
            "table" => {
                if !in_table_cell && self.is_data_table(id) {
                    self.render_table(id)
                } else {
                    self.render_block_children(id, in_table_cell)
                }
            }
            "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" => self.render_block_children(id, in_table_cell),
            "li" => self.render_block_children(id, in_table_cell),
            _ => self.render_block_children(id, in_table_cell),
        }
    }

    fn render_list(&mut self, list_id: usize, mut ordered_index: Option<usize>) -> String {
        let mut items = Vec::new();
        for &child in &self.nodes[list_id].children {
            if self.is_skipped(child) {
                continue;
            }
            match self.tag(child) {
                Some("li") => {
                    let prefix = match &mut ordered_index {
                        Some(idx) => {
                            let p = format!("{idx}. ");
                            *idx += 1;
                            p
                        }
                        None => "- ".to_string(),
                    };
                    let (checkbox, skip_first_checkbox) = self.list_item_checkbox(child);
                    let content = self.render_list_item(child, skip_first_checkbox);
                    if content.is_empty() && checkbox.is_none() {
                        continue;
                    }
                    let task = match checkbox {
                        Some(true) => "[x] ",
                        Some(false) => "[ ] ",
                        None => "",
                    };
                    let indent = " ".repeat(prefix.len());
                    let mut lines = content.lines();
                    let first = lines.next().unwrap_or("");
                    let mut item = format!("{prefix}{task}{first}");
                    for line in lines {
                        item.push('\n');
                        if !line.is_empty() {
                            item.push_str(&indent);
                            item.push_str(line);
                        }
                    }
                    items.push(item);
                }
                Some("ul") => {
                    let nested = self.render_list(child, None);
                    if !nested.is_empty() {
                        items.push(indent_lines(&nested, 2));
                    }
                }
                Some("ol") => {
                    let start = self.attr(child, "start").and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1);
                    let nested = self.render_list(child, Some(start));
                    if !nested.is_empty() {
                        items.push(indent_lines(&nested, 3));
                    }
                }
                _ => {}
            }
        }
        items.join("\n")
    }

    fn list_item_checkbox(&self, li_id: usize) -> (Option<bool>, Option<usize>) {
        for &child in &self.nodes[li_id].children {
            if self.tag(child) == Some("input")
                && self.attr(child, "type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox"))
            {
                return (Some(self.attr(child, "checked").is_some()), Some(child));
            }
            if matches!(self.tag(child), Some("p" | "div")) {
                for &grandchild in &self.nodes[child].children {
                    if self.tag(grandchild) == Some("input")
                        && self.attr(grandchild, "type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox"))
                    {
                        return (Some(self.attr(grandchild, "checked").is_some()), Some(grandchild));
                    }
                }
            }
        }
        (None, None)
    }

    fn render_list_item(&mut self, li_id: usize, skip_checkbox: Option<usize>) -> String {
        let mut blocks: Vec<(bool, String)> = Vec::new();
        let mut inline_buf = String::new();
        let flush_inline = |inline_buf: &mut String, blocks: &mut Vec<(bool, String)>| {
            let trimmed = trim_inline_lines(inline_buf);
            inline_buf.clear();
            if !trimmed.is_empty() {
                blocks.push((false, trimmed));
            }
        };
        for &child in &self.nodes[li_id].children {
            if Some(child) == skip_checkbox || self.is_skipped(child) {
                continue;
            }
            match self.tag(child) {
                None => {
                    if let NodeKind::Text(text) = &self.nodes[child].kind {
                        let collapsed = collapse_ws(text);
                        push_inline_text(&mut inline_buf, &collapsed);
                    }
                }
                Some(tag) if is_block_tag(tag) => {
                    let nested_list = matches!(tag, "ul" | "ol");
                    flush_inline(&mut inline_buf, &mut blocks);
                    let rendered = self.render_block_node(child, false);
                    if !rendered.is_empty() {
                        blocks.push((nested_list, rendered));
                    }
                }
                Some(_) => {
                    let rendered = self.render_inline_skip(child, false, false, skip_checkbox);
                    push_inline_text(&mut inline_buf, &rendered);
                }
            }
        }
        flush_inline(&mut inline_buf, &mut blocks);
        let mut out = String::new();
        for (idx, (is_nested_list, text)) in blocks.into_iter().enumerate() {
            if idx > 0 {
                out.push_str(if is_nested_list { "\n" } else { "\n\n" });
            }
            out.push_str(&text);
        }
        out
    }

    fn is_data_table(&self, table_id: usize) -> bool {
        if self.attr(table_id, "role").is_some_and(|r| r.eq_ignore_ascii_case("presentation") || r.eq_ignore_ascii_case("none")) {
            return false;
        }
        let mut has_th = false;
        let mut row_count = 0usize;
        let mut max_cols = 0usize;
        let mut rows = Vec::new();
        self.collect_table_rows(table_id, &mut rows);
        for row in rows {
            row_count += 1;
            let mut cols = 0usize;
            for &cell in &self.nodes[row].children {
                match self.tag(cell) {
                    Some("th") => {
                        has_th = true;
                        cols += 1;
                        if self.cell_has_complex_blocks(cell) {
                            return false;
                        }
                    }
                    Some("td") => {
                        cols += 1;
                        if self.cell_has_complex_blocks(cell) {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
            max_cols = max_cols.max(cols);
        }
        if max_cols == 0 || row_count == 0 {
            return false;
        }
        has_th || (max_cols >= 2 && row_count >= 2)
    }

    fn collect_table_rows(&self, node_id: usize, rows: &mut Vec<usize>) {
        for &child in &self.nodes[node_id].children {
            match self.tag(child) {
                Some("tr") => rows.push(child),
                Some("thead" | "tbody" | "tfoot") => self.collect_table_rows(child, rows),
                _ => {}
            }
        }
    }

    fn cell_has_complex_blocks(&self, cell_id: usize) -> bool {
        let mut p_count = 0usize;
        for &child in &self.nodes[cell_id].children {
            match self.tag(child) {
                Some("table" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "blockquote" | "pre") => return true,
                Some("p" | "div") => {
                    p_count += 1;
                    if p_count > 1 || self.cell_has_complex_blocks(child) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn render_table(&mut self, table_id: usize) -> String {
        let mut row_ids = Vec::new();
        self.collect_table_rows(table_id, &mut row_ids);
        let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
        let mut cols = 0usize;
        for row_id in row_ids {
            let mut cells = Vec::new();
            let mut is_header = true;
            let mut any_cell = false;
            for &cell_id in &self.nodes[row_id].children {
                match self.tag(cell_id) {
                    Some("th") => {
                        any_cell = true;
                        cells.push(escape_table_cell(&collapse_single_line(&self.render_block_children(cell_id, true))));
                    }
                    Some("td") => {
                        any_cell = true;
                        is_header = false;
                        cells.push(escape_table_cell(&collapse_single_line(&self.render_block_children(cell_id, true))));
                    }
                    _ => {}
                }
            }
            if any_cell {
                cols = cols.max(cells.len());
                rows.push((is_header && rows.is_empty(), cells));
            }
        }
        if rows.is_empty() || cols == 0 {
            return String::new();
        }
        let mut out = Vec::new();
        let first_is_header = rows[0].0;
        let pad_row = |cells: &[String]| -> String {
            let mut row = String::from("|");
            for i in 0..cols {
                row.push(' ');
                row.push_str(cells.get(i).map(String::as_str).unwrap_or(""));
                row.push_str(" |");
            }
            row
        };
        let sep_row = || -> String {
            let mut row = String::from("|");
            for _ in 0..cols {
                row.push_str(" --- |");
            }
            row
        };
        if first_is_header {
            out.push(pad_row(&rows[0].1));
            out.push(sep_row());
            for (_, cells) in rows.into_iter().skip(1) {
                out.push(pad_row(&cells));
            }
        } else {
            out.push(pad_row(&[]));
            out.push(sep_row());
            for (_, cells) in rows {
                out.push(pad_row(&cells));
            }
        }
        out.join("\n")
    }

    fn code_lang(&self, pre_id: usize) -> Option<String> {
        for &child in &self.nodes[pre_id].children {
            if self.tag(child) == Some("code")
                && let Some(class) = self.attr(child, "class")
            {
                for token in class.split_whitespace() {
                    if let Some(lang) = token.strip_prefix("language-").or_else(|| token.strip_prefix("lang-"))
                        && !lang.is_empty()
                    {
                        return Some(lang.to_string());
                    }
                }
            }
        }
        None
    }

    fn collect_pre_text(&self, id: usize, out: &mut String) {
        if self.is_skipped(id) {
            return;
        }
        match &self.nodes[id].kind {
            NodeKind::Text(text) => out.push_str(&text.replace("\r\n", "\n")),
            NodeKind::Element { .. } => {
                if self.tag(id) == Some("br") {
                    out.push('\n');
                    return;
                }
                for &child in &self.nodes[id].children {
                    self.collect_pre_text(child, out);
                }
                if matches!(self.tag(id), Some("div" | "p")) && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }

    fn render_inline_children(&self, parent: usize, in_table_cell: bool, in_link: bool) -> String {
        self.render_inline_children_skip(parent, in_table_cell, in_link, None)
    }

    fn render_inline_children_skip(
        &self,
        parent: usize,
        in_table_cell: bool,
        in_link: bool,
        skip: Option<usize>,
    ) -> String {
        let mut out = String::new();
        for &child in &self.nodes[parent].children {
            if Some(child) == skip {
                continue;
            }
            let piece = self.render_inline_skip(child, in_table_cell, in_link, skip);
            push_inline_text(&mut out, &piece);
        }
        out
    }

    fn render_inline(&self, id: usize, in_table_cell: bool, in_link: bool) -> String {
        self.render_inline_skip(id, in_table_cell, in_link, None)
    }

    fn render_inline_skip(
        &self,
        id: usize,
        in_table_cell: bool,
        in_link: bool,
        skip: Option<usize>,
    ) -> String {
        if self.is_skipped(id) {
            return String::new();
        }
        match &self.nodes[id].kind {
            NodeKind::Text(text) => collapse_ws(text),
            NodeKind::Element { .. } => {
                let tag = self.tag(id).unwrap_or("");
                match tag {
                    "br" => {
                        if in_table_cell {
                            " ".to_string()
                        } else {
                            "\n".to_string()
                        }
                    }
                    "img" => {
                        let alt = self.attr(id, "alt").unwrap_or("").trim();
                        let src = self.attr(id, "src").unwrap_or("").trim();
                        if !in_link
                            && (src.starts_with("https://") || src.starts_with("http://"))
                            && !alt.is_empty()
                        {
                            format!("![{}]({})", escape_link_label(alt), format_link_url(src))
                        } else {
                            alt.to_string()
                        }
                    }
                    "code" | "kbd" | "samp" | "tt" => {
                        let mut text = String::new();
                        self.collect_plain_inline(id, &mut text);
                        let collapsed = collapse_ws(&text);
                        wrap_inline_code(collapsed.trim())
                    }
                    "a" => {
                        let inner = self.render_inline_children_skip(id, in_table_cell, true, skip);
                        let trimmed = inner.trim();
                        let Some(href) = self.attr(id, "href").map(str::trim).filter(|h| !h.is_empty()) else {
                            return inner;
                        };
                        if in_link || trimmed.is_empty() || !is_mail_link(href) {
                            return inner;
                        }
                        // Preserve leading/trailing whitespace outside the Markdown link.
                        let lead = &inner[..inner.len() - inner.trim_start().len()];
                        let trail = &inner[inner.trim_end().len()..];
                        let label = if trimmed == href {
                            trimmed.to_string()
                        } else {
                            format!("[{}]({})", escape_link_label(trimmed), format_link_url(href))
                        };
                        format!("{lead}{label}{trail}")
                    }
                    _ => {
                        let inner = self.render_inline_children_skip(id, in_table_cell, in_link, skip);
                        let (bold, italic, strike) = self.inline_style_flags(id, tag);
                        apply_inline_markers(&inner, bold, italic, strike)
                    }
                }
            }
            _ => String::new(),
        }
    }

    fn collect_plain_inline(&self, id: usize, out: &mut String) {
        if self.is_skipped(id) {
            return;
        }
        match &self.nodes[id].kind {
            NodeKind::Text(t) => out.push_str(t),
            NodeKind::Element { .. } => {
                if self.tag(id) == Some("br") {
                    out.push(' ');
                    return;
                }
                for &child in &self.nodes[id].children {
                    self.collect_plain_inline(child, out);
                }
            }
            _ => {}
        }
    }

    fn inline_style_flags(&self, id: usize, tag: &str) -> (bool, bool, bool) {
        let mut bold = matches!(tag, "strong" | "b");
        let mut italic = matches!(tag, "em" | "i" | "cite" | "dfn");
        let mut strike = matches!(tag, "del" | "s" | "strike");

        if let Some(weight) = self.style_prop(id, "font-weight") {
            let w = weight.to_ascii_lowercase();
            if w == "normal" || w == "lighter" || w.parse::<u16>().is_ok_and(|n| n < 600) {
                bold = false;
            } else if w == "bold" || w == "bolder" || w.parse::<u16>().is_ok_and(|n| n >= 600) {
                bold = true;
            }
        }
        if let Some(style) = self.style_prop(id, "font-style") {
            let s = style.to_ascii_lowercase();
            if s == "normal" {
                italic = false;
            } else if s == "italic" || s.starts_with("oblique") {
                italic = true;
            }
        }
        if let Some(deco) = self.style_prop(id, "text-decoration").or_else(|| self.style_prop(id, "text-decoration-line")) {
            let d = deco.to_ascii_lowercase();
            if d == "none" {
                strike = false;
            } else if d.contains("line-through") {
                strike = true;
            }
        }
        (bold, italic, strike)
    }
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        let ch = if ch == '\u{00A0}' { ' ' } else { ch };
        if ch.is_ascii_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

fn push_inline_text(out: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }
    if piece == "\n" {
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
        return;
    }
    let piece = if out.is_empty() || out.ends_with(' ') || out.ends_with('\n') {
        piece.trim_start_matches(' ')
    } else {
        piece
    };
    out.push_str(piece);
}

fn trim_inline_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| line.trim_matches(' '))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

fn collapse_single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn apply_inline_markers(text: &str, bold: bool, italic: bool, strike: bool) -> String {
    if (!bold && !italic && !strike) || text.trim().is_empty() {
        return text.to_string();
    }
    let wrap_segment = |seg: &str| -> String {
        if seg.trim().is_empty() {
            return seg.to_string();
        }
        let lead = &seg[..seg.len() - seg.trim_start().len()];
        let trail = &seg[seg.trim_end().len()..];
        let mut core = seg.trim().to_string();
        if strike {
            core = format!("~~{core}~~");
        }
        if italic {
            core = format!("*{core}*");
        }
        if bold {
            core = format!("**{core}**");
        }
        format!("{lead}{core}{trail}")
    };
    if text.contains('\n') {
        text.split('\n').map(wrap_segment).collect::<Vec<_>>().join("\n")
    } else {
        wrap_segment(text)
    }
}

fn wrap_inline_code(code: &str) -> String {
    if code.is_empty() {
        return String::new();
    }
    let mut max_run = 0usize;
    let mut run = 0usize;
    for b in code.bytes() {
        if b == b'`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(max_run + 1);
    if code.starts_with('`') || code.ends_with('`') {
        format!("{fence} {code} {fence}")
    } else {
        format!("{fence}{code}{fence}")
    }
}

fn fence_for(code: &str) -> String {
    let mut max_run = 2usize;
    let mut run = 0usize;
    for b in code.bytes() {
        if b == b'`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(max_run + 1)
}

fn escape_link_label(label: &str) -> String {
    label.replace('[', "\\[").replace(']', "\\]")
}

fn format_link_url(url: &str) -> String {
    if url.contains([' ', '(', ')']) {
        format!("<{url}>")
    } else {
        url.to_string()
    }
}

fn escape_table_cell(cell: &str) -> String {
    cell.replace('|', "\\|")
}

fn indent_lines(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines()
        .map(|line| if line.is_empty() { String::new() } else { format!("{pad}{line}") })
        .collect::<Vec<_>>()
        .join("\n")
}

fn trim_markdown(text: &str) -> String {
    text.trim_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::html_to_markdown;

    #[test]
    fn converts_inline_formatting_and_links() {
        let html = r#"<p>Hello <strong>Alice</strong>, check <a href="https://example.com">the <em>spec</em></a>, <code>foo()</code>, and <del>old</del>.</p>"#;
        assert_eq!(
            html_to_markdown(html),
            "Hello **Alice**, check [the *spec*](https://example.com), `foo()`, and ~~old~~."
        );
    }

    #[test]
    fn keeps_whitespace_outside_emphasis_markers_and_ignores_google_docs_normal_b() {
        let html = r#"<b id="docs-internal-guid-1" style="font-weight:normal;"><span style="font-weight:700">Bold </span>and <span style="font-style:italic">italic</span></b>"#;
        assert_eq!(html_to_markdown(html), "**Bold** and *italic*");
    }

    #[test]
    fn converts_headings_lists_quotes_code_blocks_and_data_tables() {
        let html = concat!(
            "<h2>Release Plan</h2>",
            "<ul><li>First</li><li>Second<ul><li>Nested</li></ul></li></ul>",
            "<ol start=\"3\"><li>Third</li></ol>",
            "<blockquote><p>Line one</p><p>Line two</p></blockquote>",
            "<pre><code class=\"language-rust\">let x = 1;\nlet y = 2;</code></pre>",
            "<table><thead><tr><th>Name</th><th>Status</th></tr></thead><tbody><tr><td>CI</td><td>Green</td></tr></tbody></table>"
        );
        assert_eq!(
            html_to_markdown(html),
            concat!(
                "## Release Plan\n\n",
                "- First\n",
                "- Second\n",
                "  - Nested\n\n",
                "3. Third\n\n",
                "> Line one\n>\n> Line two\n\n",
                "```rust\nlet x = 1;\nlet y = 2;\n```\n\n",
                "| Name | Status |\n| --- | --- |\n| CI | Green |"
            )
        );
    }

    #[test]
    fn unwraps_email_layout_tables_instead_of_turning_them_into_pipe_tables() {
        let html = r#"<table role="presentation"><tr><td><h1>Welcome</h1><p>Body text</p></td></tr></table>"#;
        assert_eq!(html_to_markdown(html), "# Welcome\n\nBody text");
    }

    #[test]
    fn handles_chromium_clipboard_fragment_comments() {
        let html = "<html><body><!--StartFragment--><strong>Hi</strong> <a href=\"https://example.com\">link</a><!--EndFragment--></body></html>";
        assert_eq!(html_to_markdown(html), "**Hi** [link](https://example.com)");
    }
}
