use std::ops::Range;

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement,
    StrikethroughStyle, StyledText, UnderlineStyle, px, rgb,
};
use pulldown_cmark::{Event, Tag};
use url::Url;

use super::Node;
use crate::theme;

#[derive(Default)]
pub(super) struct Text {
    pub(super) text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: Vec<(Range<usize>, String)>,
}

impl Text {
    pub(super) fn from_nodes(nodes: &[Node<'_>]) -> Self {
        let mut text = Self::default();
        text.append(nodes, HighlightStyle::default(), true);
        text
    }

    pub(super) fn render(self, id: usize) -> AnyElement {
        let text = StyledText::new(self.text).with_highlights(self.highlights);
        if self.links.is_empty() {
            return text.into_any_element();
        }
        let (ranges, targets): (Vec<_>, Vec<_>) = self.links.into_iter().unzip();
        InteractiveText::new(("markdown-text", id), text)
            .on_click(ranges, move |index, _, cx| cx.open_url(&targets[index]))
            .into_any_element()
    }

    fn append(&mut self, nodes: &[Node<'_>], style: HighlightStyle, links_enabled: bool) {
        for node in nodes {
            match node {
                Node::Leaf(event) => self.push_event(event, style),
                Node::Element(
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. },
                    children,
                ) => {
                    self.link(dest_url, children, style, links_enabled);
                }
                Node::Element(tag, children) => {
                    let mut style = style;
                    match tag {
                        Tag::Strong => style.font_weight = Some(FontWeight::BOLD),
                        Tag::Emphasis => style.font_style = Some(FontStyle::Italic),
                        Tag::Strikethrough => {
                            style.strikethrough = Some(StrikethroughStyle {
                                thickness: px(1.),
                                color: None,
                            });
                        }
                        _ => {}
                    }
                    self.append(children, style, links_enabled);
                }
            }
        }
    }

    fn link(
        &mut self,
        destination: &str,
        nodes: &[Node<'_>],
        mut style: HighlightStyle,
        enabled: bool,
    ) {
        let target = if enabled { web_url(destination) } else { None };
        let start = self.text.len();
        if target.is_some() {
            style.color = Some(rgb(0xb7cdee).into());
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            });
        }
        self.append(nodes, style, enabled && target.is_none());
        if let Some(target) = target
            && self.text.len() > start
        {
            self.links.push((start..self.text.len(), target));
        }
    }

    fn push_event(&mut self, event: &Event<'_>, mut style: HighlightStyle) {
        match event {
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push(text, style)
            }
            Event::Code(text) => {
                style.background_color = Some(rgb(theme::HOVER).into());
                self.push(text, style);
            }
            Event::SoftBreak => self.push(" ", style),
            Event::HardBreak => self.push("\n", style),
            Event::FootnoteReference(label) => self.push(&format!("[{label}]"), style),
            _ => {}
        }
    }

    fn push(&mut self, text: &str, style: HighlightStyle) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if style != HighlightStyle::default() {
            self.highlights.push((start..self.text.len(), style));
        }
    }
}

fn web_url(destination: &str) -> Option<String> {
    let url = Url::parse(destination).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::markdown::parse;

    #[test]
    fn keeps_link_and_style_ranges_on_utf8_boundaries() {
        let nodes =
            parse("Olá **[*ação*](https://example.com)** e `猫`.\nPróxima  \nlinha").unwrap();
        let text = Text::from_nodes(&nodes);
        assert_eq!(text.text, "Olá ação e 猫. Próxima\nlinha");
        let (range, target) = &text.links[0];
        assert_eq!(&text.text[range.clone()], "ação");
        assert_eq!(target, "https://example.com/");
        assert_eq!(text.highlights[0].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(text.highlights[0].1.font_style, Some(FontStyle::Italic));
        assert_eq!(&text.text[text.highlights[1].0.clone()], "猫");
    }

    #[test]
    fn displays_unsafe_links_and_raw_html_as_text() {
        let nodes = parse("[local](file:///tmp/document) <b>trecho</b>").unwrap();
        let text = Text::from_nodes(&nodes);
        assert_eq!(text.text, "local <b>trecho</b>");
        assert!(text.links.is_empty());
    }
}
