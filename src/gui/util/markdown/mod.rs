mod inline;

use gpui::{
    AnyElement, FontWeight, HighlightStyle, StyledText, TextAlign, div, prelude::*, px, rgb,
};
use morpheus_sdk::tree_sitter::highlight::{HighlightCategory, highlight};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};

use crate::theme;

const CODE_FONT: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

enum Node<'a> {
    Element(Tag<'a>, Vec<Node<'a>>),
    Leaf(Event<'a>),
}

fn parse(markdown: &str) -> Option<Vec<Node<'_>>> {
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut root = Vec::new();
    let mut stack: Vec<(Tag<'_>, Vec<Node<'_>>)> = Vec::new();
    for event in Parser::new_ext(markdown, options) {
        let node = match event {
            Event::Start(tag) => {
                // Bound the depth of both the recursive renderer and the GPUI element tree.
                if stack.len() == 64 {
                    return None;
                }
                stack.push((tag, Vec::new()));
                continue;
            }
            Event::End(_) => {
                let (tag, children) = stack.pop()?;
                Node::Element(tag, children)
            }
            leaf => Node::Leaf(leaf),
        };
        match stack.last_mut() {
            Some((_, children)) => children.push(node),
            None => root.push(node),
        }
    }
    Some(root)
}

pub(crate) fn render(markdown: &str) -> AnyElement {
    match parse(markdown) {
        Some(nodes) => Renderer::default().blocks(&nodes),
        None => div().child(markdown.to_owned()).into_any_element(),
    }
}

#[derive(Default)]
struct Renderer {
    next_id: usize,
}

impl Renderer {
    fn id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn blocks(&mut self, nodes: &[Node<'_>]) -> AnyElement {
        let mut blocks = Vec::new();
        let mut start = 0;
        for (index, node) in nodes.iter().enumerate() {
            if let Some(block) = self.block(node) {
                if start < index {
                    blocks.push(self.inline(&nodes[start..index]));
                }
                blocks.push(block);
                start = index + 1;
            }
        }
        if start < nodes.len() {
            blocks.push(self.inline(&nodes[start..]));
        }
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap(px(9.))
            .children(blocks)
            .into_any_element()
    }

    fn block(&mut self, node: &Node<'_>) -> Option<AnyElement> {
        let (tag, children) = match node {
            Node::Element(tag, children) => (tag, children),
            Node::Leaf(Event::Rule) => {
                return Some(
                    div()
                        .h(px(1.))
                        .w_full()
                        .bg(rgb(theme::BORDER))
                        .into_any_element(),
                );
            }
            Node::Leaf(_) => return None,
        };

        Some(match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.inline(children),
            Tag::Heading { level, .. } => {
                let font_size = match level {
                    HeadingLevel::H1 => 25.,
                    HeadingLevel::H2 => 22.,
                    HeadingLevel::H3 => 19.,
                    _ => 16.,
                };
                div()
                    .text_size(px(font_size))
                    .line_height(px(font_size + 8.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .mt(px(4.))
                    .child(self.inline(children))
                    .into_any_element()
            }
            Tag::BlockQuote(_) => div()
                .border_l_2()
                .border_color(rgb(theme::FOCUS))
                .pl(px(12.))
                .py(px(2.))
                .text_color(rgb(theme::MUTED))
                .child(self.blocks(children))
                .into_any_element(),
            Tag::List(start) => self.list(*start, children),
            Tag::CodeBlock(kind) => self.code(kind, children),
            Tag::Table(alignments) => self.table(alignments, children),
            Tag::FootnoteDefinition(label) => self.marked_block(format!("[{label}]"), children),
            _ => return None,
        })
    }

    fn inline(&mut self, nodes: &[Node<'_>]) -> AnyElement {
        inline::Text::from_nodes(nodes).render(self.id())
    }

    fn list(&mut self, start: Option<u64>, items: &[Node<'_>]) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .children(items.iter().enumerate().filter_map(|(index, item)| {
                let Node::Element(Tag::Item, children) = item else {
                    return None;
                };
                let marker = match task_marker(children) {
                    Some(true) => "☑".into(),
                    Some(false) => "☐".into(),
                    None => start.map_or_else(
                        || "•".into(),
                        |n| format!("{}.", n.saturating_add(index as u64)),
                    ),
                };
                Some(self.marked_block(marker, children))
            }))
            .into_any_element()
    }

    fn marked_block(&mut self, marker: String, children: &[Node<'_>]) -> AnyElement {
        div()
            .flex()
            .gap(px(8.))
            .child(
                div()
                    .min_w(px(14.))
                    .flex_shrink_0()
                    .text_color(rgb(theme::MUTED))
                    .child(marker),
            )
            .child(div().flex_1().min_w_0().child(self.blocks(children)))
            .into_any_element()
    }

    fn code(&mut self, kind: &CodeBlockKind<'_>, children: &[Node<'_>]) -> AnyElement {
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or_default(),
            CodeBlockKind::Indented => "",
        };
        let source = inline::Text::from_nodes(children).text;
        let source = source.strip_suffix('\n').unwrap_or(&source);
        let spans = highlight(source, &language.to_ascii_lowercase()).unwrap_or_default();
        let mut last_end = 0;
        let highlights = spans.into_iter().filter_map(|span| {
            let start = span.start.max(last_end);
            if start >= span.end {
                return None;
            }
            last_end = span.end;
            let color = match span.category {
                HighlightCategory::Keyword => 0xcab0dd,
                HighlightCategory::String => 0xb8ceaa,
                HighlightCategory::Comment => theme::MUTED,
                HighlightCategory::Type | HighlightCategory::Builtin => 0xa5cbd2,
                HighlightCategory::Function => 0xb0c4e6,
                HighlightCategory::Number => 0xd9b599,
            };
            Some((
                start..span.end,
                HighlightStyle {
                    color: Some(rgb(color).into()),
                    ..Default::default()
                },
            ))
        });
        let code = StyledText::new(source.to_owned()).with_highlights(highlights);

        div()
            .flex()
            .flex_col()
            .min_w_0()
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .bg(rgb(theme::CHROME))
            .when(!language.is_empty(), |block| {
                block.child(
                    div()
                        .px(px(12.))
                        .pt(px(6.))
                        .text_size(px(12.))
                        .text_color(rgb(theme::MUTED))
                        .child(language.to_owned()),
                )
            })
            .child(
                div()
                    .id(("code-block", self.id()))
                    .overflow_x_scroll()
                    .p(px(12.))
                    .font_family(CODE_FONT)
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .whitespace_nowrap()
                    .child(code),
            )
            .into_any_element()
    }

    fn table(&mut self, alignments: &[Alignment], children: &[Node<'_>]) -> AnyElement {
        let rows = children.iter().enumerate().filter_map(|(index, row)| {
            let Node::Element(tag @ (Tag::TableHead | Tag::TableRow), cells) = row else {
                return None;
            };
            Some(
                div()
                    .flex()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(rgb(theme::BORDER))
                    })
                    .when(matches!(tag, Tag::TableHead), |row| {
                        row.bg(rgb(theme::SURFACE))
                            .font_weight(FontWeight::SEMIBOLD)
                    })
                    .children(cells.iter().zip(alignments).enumerate().filter_map(
                        |(column, (cell, alignment))| {
                            let Node::Element(Tag::TableCell, content) = cell else {
                                return None;
                            };
                            Some(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .px(px(10.))
                                    .py(px(6.))
                                    .when(column > 0, |cell| {
                                        cell.border_l_1().border_color(rgb(theme::BORDER))
                                    })
                                    .text_align(match alignment {
                                        Alignment::Center => TextAlign::Center,
                                        Alignment::Right => TextAlign::Right,
                                        _ => TextAlign::Left,
                                    })
                                    .child(self.inline(content)),
                            )
                        },
                    )),
            )
        });
        let body = div()
            .flex()
            .flex_col()
            .min_w(px(132. * alignments.len() as f32))
            .w_full()
            .children(rows);
        div()
            .id(("markdown-table", self.id()))
            .min_w_0()
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .overflow_x_scroll()
            .child(body)
            .into_any_element()
    }
}

fn task_marker(children: &[Node<'_>]) -> Option<bool> {
    let first = match children.first()? {
        Node::Element(Tag::Paragraph, nodes) => nodes.first()?,
        node => node,
    };
    match first {
        Node::Leaf(Event::TaskListMarker(checked)) => Some(*checked),
        _ => None,
    }
}
