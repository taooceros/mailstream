//! Extracts rich HTML and clean plain text from Blitz's active text selection (`HtmlPage::selected_content`).

use blitz_dom::{BaseDocument, NodeId, local_name};
use style::values::computed::TextDecorationLine;
use style::values::computed::font::FontStyle;

use crate::html_page::HtmlSelection;
use crate::mail_html::is_mail_link;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    code: bool,
    link: Option<String>,
}

#[derive(Clone, Debug)]
struct InlineSpan {
    text: String,
    style: InlineStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Blockquote,
    List { ordered: bool },
    ListItem,
    Table,
    TableHead,
    TableBody,
    TableRow,
    TableCell { header: bool },
    Pre,
    Heading(u8),
    Paragraph,
}

struct SelectedBlock {
    blocks: Vec<(NodeId, BlockKind)>,
    spans: Vec<InlineSpan>,
    is_pre: bool,
    partial: bool,
}

/// Extracts clean HTML and plain text for the document's current text selection.
pub(crate) fn extract_selection(doc: &BaseDocument) -> Option<HtmlSelection> {
    let ranges = doc.get_text_selection_ranges();
    if ranges.is_empty() {
        return None;
    }

    let mut items = Vec::with_capacity(ranges.len());
    for &(root_id, start, end) in &ranges {
        let Some(root_node) = doc.get_node(root_id) else { continue };
        let Some(inline_layout) = root_node.element_data().and_then(|el| el.inline_layout_data.as_ref()) else {
            continue;
        };
        let full_text = &inline_layout.text;
        if start >= end || end > full_text.len() {
            continue;
        }
        let partial = start > 0 || end < full_text.len();
        let blocks = block_chain(doc, root_id);
        let is_pre = blocks.iter().any(|(_, kind)| matches!(kind, BlockKind::Pre));
        let spans = extract_inline_spans(doc, root_id, full_text, inline_layout, start, end, is_pre);
        if spans.iter().all(|s| s.text.is_empty()) {
            continue;
        }
        items.push(SelectedBlock { blocks, spans, is_pre, partial });
    }

    if items.is_empty() {
        return None;
    }

    // If multiple inline roots live directly under the same <li> or <td>/<th> (e.g. anonymous
    // blocks beside nested blocks), keep them separated in HTML rather than mashing words together.
    separate_sibling_blocks_in_containers(&mut items);

    let html = render_html(doc, &items);
    let text = render_plain_text(doc, &items);
    if text.is_empty() && html.is_empty() {
        None
    } else {
        Some(HtmlSelection { text, html })
    }
}

fn separate_sibling_blocks_in_containers(items: &mut [SelectedBlock]) {
    if items.len() <= 1 {
        return;
    }
    for i in 0..items.len() {
        let Some(&(last_id, last_kind)) = items[i].blocks.last() else { continue };
        if !matches!(last_kind, BlockKind::ListItem | BlockKind::TableCell { .. }) {
            continue;
        }
        let shares_container = (i > 0 && items[i - 1].blocks.iter().any(|&(id, _)| id == last_id))
            || (i + 1 < items.len() && items[i + 1].blocks.iter().any(|&(id, _)| id == last_id));
        if shares_container {
            items[i].blocks.push((last_id, BlockKind::Paragraph));
        }
    }
}

fn extract_inline_spans(
    doc: &BaseDocument,
    root_id: NodeId,
    full_text: &str,
    inline_layout: &blitz_dom::node::TextLayout,
    start: usize,
    end: usize,
    is_pre: bool,
) -> Vec<InlineSpan> {
    let mut raw: Vec<(usize, usize, NodeId)> = Vec::new();
    for line in inline_layout.layout.lines() {
        for run in line.runs() {
            for cluster in run.clusters() {
                let range = cluster.text_range();
                let s = range.start.max(start);
                let e = range.end.min(end);
                if s < e && full_text.is_char_boundary(s) && full_text.is_char_boundary(e) {
                    raw.push((s, e, cluster.style().brush.id));
                }
            }
        }
    }
    raw.sort_by_key(|&(s, e, _)| (s, e));

    let mut spans: Vec<InlineSpan> = Vec::new();
    let mut push_segment = |seg_start: usize, seg_end: usize, node_id: NodeId| {
        if seg_start >= seg_end
            || !full_text.is_char_boundary(seg_start)
            || !full_text.is_char_boundary(seg_end)
        {
            return;
        }
        let text = full_text[seg_start..seg_end].replace('\u{FFFC}', "");
        if text.is_empty() {
            return;
        }
        let style = inline_style(doc, node_id, root_id, is_pre);
        if let Some(last) = spans.last_mut()
            && last.style == style
        {
            last.text.push_str(&text);
        } else {
            spans.push(InlineSpan { text, style });
        }
    };

    let mut cursor = start;
    for (s, e, node_id) in raw {
        if e <= cursor {
            continue;
        }
        let s = s.max(cursor);
        if s > cursor {
            push_segment(cursor, s, root_id);
        }
        push_segment(s, e, node_id);
        cursor = e;
    }
    if cursor < end {
        push_segment(cursor, end, root_id);
    }
    spans
}

fn inline_style(doc: &BaseDocument, node_id: NodeId, root_id: NodeId, is_pre: bool) -> InlineStyle {
    let mut style = InlineStyle::default();
    let mut curr = Some(node_id);
    let mut past_root = false;
    let mut weight_decided = false;
    let mut italic_decided = false;

    while let Some(id) = curr {
        let Some(node) = doc.get_node(id) else { break };
        if node.data.is_element_with_tag_name(&local_name!("body"))
            || node.data.is_element_with_tag_name(&local_name!("html"))
        {
            break;
        }
        let is_heading_or_th = matches_tag(node, &["h1", "h2", "h3", "h4", "h5", "h6", "th"]);
        if !past_root && !is_heading_or_th {
            if matches_tag(node, &["strong", "b"]) {
                style.bold = true;
            }
            if matches_tag(node, &["em", "i", "cite", "dfn"]) {
                style.italic = true;
            }
            if matches_tag(node, &["del", "s", "strike"]) {
                style.strike = true;
            }
            if matches_tag(node, &["u", "ins"]) {
                style.underline = true;
            }
            if !is_pre && matches_tag(node, &["code", "kbd", "samp", "tt"]) {
                style.code = true;
            }
            if let Some(computed) = node.primary_styles() {
                if !weight_decided {
                    let w = computed.get_font().font_weight.value();
                    if w >= 600.0 {
                        style.bold = true;
                        weight_decided = true;
                    } else if node.data.is_element_with_tag_name(&local_name!("b")) && w < 500.0 {
                        // Respect explicit `font-weight: normal` on `<b>` (e.g. rich wrappers).
                        style.bold = false;
                        weight_decided = true;
                    }
                }
                if !italic_decided && computed.get_font().font_style != FontStyle::NORMAL {
                    style.italic = true;
                    italic_decided = true;
                }
                let deco = computed.get_text().text_decoration_line;
                if deco.contains(TextDecorationLine::LINE_THROUGH) {
                    style.strike = true;
                }
                if deco.contains(TextDecorationLine::UNDERLINE) {
                    style.underline = true;
                }
            }
        }
        if style.link.is_none()
            && node.data.is_element_with_tag_name(&local_name!("a"))
            && let Some(href) = node.attr(local_name!("href"))
        {
            let href = href.trim();
            if is_mail_link(href) {
                style.link = Some(href.to_string());
            }
        }
        if id == root_id {
            past_root = true;
        }
        curr = node.parent;
    }

    if style.link.is_some() {
        // Links carry an underline by default; don't emit redundant `<u>` around `<a>`.
        style.underline = false;
    }
    style
}

fn matches_tag(node: &blitz_dom::Node, tags: &[&str]) -> bool {
    node.element_data().is_some_and(|el| tags.iter().any(|t| &*el.name.local == *t))
}

fn block_chain(doc: &BaseDocument, root_id: NodeId) -> Vec<(NodeId, BlockKind)> {
    let mut raw: Vec<(NodeId, BlockKind)> = Vec::new();
    let mut curr = Some(root_id);
    while let Some(id) = curr {
        let Some(node) = doc.get_node(id) else { break };
        if node.data.is_element_with_tag_name(&local_name!("body"))
            || node.data.is_element_with_tag_name(&local_name!("html"))
        {
            break;
        }
        if let Some(el) = node.element_data() {
            let tag = el.name.local.as_ref();
            let kind = match tag {
                "blockquote" => Some(BlockKind::Blockquote),
                "ul" => Some(BlockKind::List { ordered: false }),
                "ol" => Some(BlockKind::List { ordered: true }),
                "li" => Some(BlockKind::ListItem),
                "table" if is_data_table(doc, id) => Some(BlockKind::Table),
                "thead" => Some(BlockKind::TableHead),
                "tbody" | "tfoot" => Some(BlockKind::TableBody),
                "tr" => Some(BlockKind::TableRow),
                "th" => Some(BlockKind::TableCell { header: true }),
                "td" => Some(BlockKind::TableCell { header: false }),
                "pre" => Some(BlockKind::Pre),
                "h1" => Some(BlockKind::Heading(1)),
                "h2" => Some(BlockKind::Heading(2)),
                "h3" => Some(BlockKind::Heading(3)),
                "h4" => Some(BlockKind::Heading(4)),
                "h5" => Some(BlockKind::Heading(5)),
                "h6" => Some(BlockKind::Heading(6)),
                "p" => Some(BlockKind::Paragraph),
                _ => None,
            };
            if let Some(kind) = kind {
                raw.push((id, kind));
            }
        }
        curr = node.parent;
    }
    raw.reverse();

    // Drop orphan table sub-parts when their enclosing `<table>` was unwrapped as a layout table.
    let mut chain: Vec<(NodeId, BlockKind)> = Vec::with_capacity(raw.len() + 1);
    let mut in_table = false;
    for (id, kind) in raw {
        match kind {
            BlockKind::Table => {
                in_table = true;
                chain.push((id, kind));
            }
            BlockKind::TableHead | BlockKind::TableBody | BlockKind::TableRow | BlockKind::TableCell { .. } => {
                if in_table {
                    chain.push((id, kind));
                }
            }
            _ => {
                in_table = false;
                chain.push((id, kind));
            }
        }
    }

    if !matches!(
        chain.last().map(|(_, k)| *k),
        Some(
            BlockKind::Paragraph
                | BlockKind::Heading(_)
                | BlockKind::Pre
                | BlockKind::ListItem
                | BlockKind::TableCell { .. }
        )
    ) {
        chain.push((root_id, BlockKind::Paragraph));
    }
    chain
}

fn is_data_table(doc: &BaseDocument, table_id: NodeId) -> bool {
    let Some(table) = doc.get_node(table_id) else { return false };
    if table
        .attr(local_name!("role"))
        .is_some_and(|r| r.eq_ignore_ascii_case("presentation") || r.eq_ignore_ascii_case("none"))
    {
        return false;
    }
    let mut has_th = false;
    let mut row_count = 0usize;
    let mut max_cols = 0usize;
    let mut rows = Vec::new();
    collect_rows(doc, table_id, &mut rows);
    for row_id in rows {
        let Some(row) = doc.get_node(row_id) else { continue };
        row_count += 1;
        let mut cols = 0usize;
        for &cell_id in &row.children {
            let Some(cell) = doc.get_node(cell_id) else { continue };
            if cell.data.is_element_with_tag_name(&local_name!("th")) {
                has_th = true;
                cols += 1;
                if cell_has_complex_blocks(doc, cell_id) {
                    return false;
                }
            } else if cell.data.is_element_with_tag_name(&local_name!("td")) {
                cols += 1;
                if cell_has_complex_blocks(doc, cell_id) {
                    return false;
                }
            }
        }
        max_cols = max_cols.max(cols);
    }
    if max_cols == 0 || row_count == 0 {
        return false;
    }
    has_th || (max_cols >= 2 && row_count >= 2)
}

fn collect_rows(doc: &BaseDocument, node_id: NodeId, rows: &mut Vec<NodeId>) {
    let Some(node) = doc.get_node(node_id) else { return };
    for &child_id in &node.children {
        let Some(child) = doc.get_node(child_id) else { continue };
        if child.data.is_element_with_tag_name(&local_name!("tr")) {
            rows.push(child_id);
        } else if matches_tag(child, &["thead", "tbody", "tfoot"]) {
            collect_rows(doc, child_id, rows);
        }
    }
}

fn cell_has_complex_blocks(doc: &BaseDocument, cell_id: NodeId) -> bool {
    let Some(cell) = doc.get_node(cell_id) else { return false };
    let mut p_count = 0usize;
    for &child_id in &cell.children {
        let Some(child) = doc.get_node(child_id) else { continue };
        if matches_tag(child, &["table", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "blockquote", "pre"]) {
            return true;
        }
        if matches_tag(child, &["p", "div"]) {
            p_count += 1;
            if p_count > 1 || cell_has_complex_blocks(doc, child_id) {
                return true;
            }
        }
    }
    false
}

fn list_item_ordinal(doc: &BaseDocument, list_id: NodeId, item_id: NodeId) -> usize {
    let Some(list) = doc.get_node(list_id) else { return 1 };
    let start = list
        .attr(local_name!("start"))
        .and_then(|s| s.trim().parse::<isize>().ok())
        .unwrap_or(1)
        .max(1) as usize;
    let mut index = 0usize;
    for &child_id in &list.children {
        let Some(child) = doc.get_node(child_id) else { continue };
        if child.data.is_element_with_tag_name(&local_name!("li")) {
            if child_id == item_id {
                return start + index;
            }
            index += 1;
        }
    }
    start
}

fn render_html(doc: &BaseDocument, items: &[SelectedBlock]) -> String {
    // A partial selection inside a single paragraph stays inline so pasting mid-sentence doesn't
    // introduce extra paragraph breaks.
    if items.len() == 1
        && items[0].partial
        && items[0].blocks.len() == 1
        && matches!(items[0].blocks[0].1, BlockKind::Paragraph)
    {
        let mut out = String::new();
        render_inline_html(&items[0].spans, items[0].is_pre, &mut out);
        return out;
    }

    let mut out = String::new();
    let mut stack: Vec<(NodeId, BlockKind)> = Vec::new();
    for item in items {
        let common = stack
            .iter()
            .zip(item.blocks.iter())
            .take_while(|(a, b)| a == b)
            .count();
        while stack.len() > common {
            let (_, kind) = stack.pop().unwrap();
            close_html_tag(kind, &mut out);
        }
        for i in common..item.blocks.len() {
            let (id, kind) = item.blocks[i];
            match kind {
                BlockKind::List { ordered: true } => {
                    let start = item
                        .blocks
                        .get(i + 1)
                        .filter(|(_, next_kind)| *next_kind == BlockKind::ListItem)
                        .map(|&(li_id, _)| list_item_ordinal(doc, id, li_id))
                        .unwrap_or(1);
                    if start > 1 {
                        out.push_str(&format!("<ol start=\"{start}\">"));
                    } else {
                        out.push_str("<ol>");
                    }
                }
                _ => open_html_tag(kind, &mut out),
            }
            stack.push((id, kind));
        }
        render_inline_html(&item.spans, item.is_pre, &mut out);
    }
    while let Some((_, kind)) = stack.pop() {
        close_html_tag(kind, &mut out);
    }
    out
}

fn open_html_tag(kind: BlockKind, out: &mut String) {
    match kind {
        BlockKind::Blockquote => out.push_str("<blockquote>"),
        BlockKind::List { ordered: false } => out.push_str("<ul>"),
        BlockKind::List { ordered: true } => out.push_str("<ol>"),
        BlockKind::ListItem => out.push_str("<li>"),
        BlockKind::Table => out.push_str("<table>"),
        BlockKind::TableHead => out.push_str("<thead>"),
        BlockKind::TableBody => out.push_str("<tbody>"),
        BlockKind::TableRow => out.push_str("<tr>"),
        BlockKind::TableCell { header: true } => out.push_str("<th>"),
        BlockKind::TableCell { header: false } => out.push_str("<td>"),
        BlockKind::Pre => out.push_str("<pre><code>"),
        BlockKind::Heading(level) => out.push_str(&format!("<h{level}>")),
        BlockKind::Paragraph => out.push_str("<p>"),
    }
}

fn close_html_tag(kind: BlockKind, out: &mut String) {
    match kind {
        BlockKind::Blockquote => out.push_str("</blockquote>"),
        BlockKind::List { ordered: false } => out.push_str("</ul>"),
        BlockKind::List { ordered: true } => out.push_str("</ol>"),
        BlockKind::ListItem => out.push_str("</li>"),
        BlockKind::Table => out.push_str("</table>"),
        BlockKind::TableHead => out.push_str("</thead>"),
        BlockKind::TableBody => out.push_str("</tbody>"),
        BlockKind::TableRow => out.push_str("</tr>"),
        BlockKind::TableCell { header: true } => out.push_str("</th>"),
        BlockKind::TableCell { header: false } => out.push_str("</td>"),
        BlockKind::Pre => out.push_str("</code></pre>"),
        BlockKind::Heading(level) => out.push_str(&format!("</h{level}>")),
        BlockKind::Paragraph => out.push_str("</p>"),
    }
}

fn render_inline_html(spans: &[InlineSpan], is_pre: bool, out: &mut String) {
    let mut open_link: Option<&str> = None;
    for span in spans {
        let style = &span.style;
        if open_link != style.link.as_deref() {
            if open_link.is_some() {
                out.push_str("</a>");
            }
            if let Some(href) = &style.link {
                out.push_str("<a href=\"");
                escape_html_into(href, out);
                out.push_str("\">");
            }
            open_link = style.link.as_deref();
        }
        if style.bold {
            out.push_str("<strong>");
        }
        if style.italic {
            out.push_str("<em>");
        }
        if style.strike {
            out.push_str("<del>");
        }
        if style.underline {
            out.push_str("<u>");
        }
        if style.code {
            out.push_str("<code>");
        }

        for ch in span.text.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '\n' if !is_pre => out.push_str("<br>"),
                _ => out.push(ch),
            }
        }

        if style.code {
            out.push_str("</code>");
        }
        if style.underline {
            out.push_str("</u>");
        }
        if style.strike {
            out.push_str("</del>");
        }
        if style.italic {
            out.push_str("</em>");
        }
        if style.bold {
            out.push_str("</strong>");
        }
    }
    if open_link.is_some() {
        out.push_str("</a>");
    }
}

fn escape_html_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

fn render_plain_text(doc: &BaseDocument, items: &[SelectedBlock]) -> String {
    let mut out = String::new();
    let mut list_counters: Vec<(NodeId, bool, usize)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let mut item_text = String::new();
        for span in &item.spans {
            item_text.push_str(&span.text);
        }
        if item_text.is_empty() {
            continue;
        }

        if idx > 0 {
            let prev = &items[idx - 1];
            let sep = plain_separator(&prev.blocks, &item.blocks);
            out.push_str(sep);
        }

        // Update ordered list counter state.
        let active_lists: Vec<(NodeId, bool)> = item
            .blocks
            .iter()
            .filter_map(|&(id, kind)| match kind {
                BlockKind::List { ordered } => Some((id, ordered)),
                _ => None,
            })
            .collect();
        list_counters.retain(|(id, _, _)| active_lists.iter().any(|(a, _)| a == id));
        for &(id, ordered) in &active_lists {
            if !list_counters.iter().any(|(c_id, _, _)| *c_id == id) {
                let initial = if ordered {
                    item.blocks
                        .iter()
                        .find(|(_, k)| *k == BlockKind::ListItem)
                        .map(|&(li_id, _)| list_item_ordinal(doc, id, li_id).saturating_sub(1))
                        .unwrap_or(0)
                } else {
                    0
                };
                list_counters.push((id, ordered, initial));
            }
        }

        let quote_depth = item
            .blocks
            .iter()
            .filter(|(_, k)| *k == BlockKind::Blockquote)
            .count();
        let quote_prefix = if quote_depth > 0 {
            "> ".repeat(quote_depth)
        } else {
            String::new()
        };

        let is_new_list_item = item
            .blocks
            .iter()
            .rposition(|(_, k)| *k == BlockKind::ListItem)
            .is_some_and(|pos| {
                let li_id = item.blocks[pos].0;
                idx == 0 || !items[idx - 1].blocks.iter().any(|&(id, _)| id == li_id)
            });

        let list_prefix = if is_new_list_item {
            let depth = list_counters.len().saturating_sub(1);
            if let Some((_, ordered, count)) = list_counters.last_mut() {
                *count += 1;
                let indent = "  ".repeat(depth);
                if *ordered {
                    format!("{indent}{count}. ")
                } else {
                    format!("{indent}• ")
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let mut lines = item_text.split('\n');
        if let Some(first) = lines.next() {
            out.push_str(&quote_prefix);
            out.push_str(&list_prefix);
            out.push_str(first);
            for line in lines {
                out.push('\n');
                out.push_str(&quote_prefix);
                out.push_str(line);
            }
        }
    }
    out
}

fn plain_separator(prev: &[(NodeId, BlockKind)], curr: &[(NodeId, BlockKind)]) -> &'static str {
    let common = prev.iter().zip(curr.iter()).take_while(|(a, b)| a == b).count();
    if let Some((_, BlockKind::TableRow)) = common.checked_sub(1).and_then(|i| prev.get(i)) {
        return "\t";
    }
    if common > 0 && matches!(prev.get(common - 1), Some((_, BlockKind::Table | BlockKind::TableHead | BlockKind::TableBody))) {
        return "\n";
    }
    if common > 0 && matches!(prev.get(common - 1), Some((_, BlockKind::List { .. }))) {
        return "\n";
    }
    if common > 0 && prev.get(common - 1).is_some_and(|(_, k)| *k == BlockKind::Blockquote) {
        let prefix = "> ".repeat(prev[..common].iter().filter(|(_, k)| *k == BlockKind::Blockquote).count());
        // Returning static separator; caller prefixes lines, so `\n\n` separates paragraphs cleanly.
        let _ = prefix;
        return "\n\n";
    }
    "\n\n"
}

