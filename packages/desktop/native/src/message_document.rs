use crate::markdown::{self, Kind, Node};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Default, Debug, PartialEq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub target: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub style: InlineStyle,
}
#[derive(Clone, Debug)]
pub struct Text {
    pub value: Arc<str>,
    pub spans: Vec<Span>,
    pub offset: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum BlockKind {
    Paragraph,
    Heading(u32),
    Code(String),
    Quote(String),
    List(bool, u32),
    Item(u32),
    Table,
    TableSection,
    Row,
    Cell(bool, u32),
    Rule,
    Html,
    Image { source: String, alt: String },
}
#[derive(Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub text: Option<Text>,
    pub children: Vec<Block>,
}
pub struct Document {
    pub blocks: Vec<Block>,
    pub plain: Arc<str>,
}
impl Document {
    pub fn plain(source: &str) -> Self {
        let text = Text {
            value: Arc::from(source),
            spans: vec![Span {
                range: 0..source.len(),
                style: InlineStyle::default(),
            }],
            offset: 0,
        };
        Self {
            blocks: vec![Block {
                kind: BlockKind::Paragraph,
                text: Some(text),
                children: vec![],
            }],
            plain: Arc::from(source),
        }
    }
    pub fn markdown(source: &str) -> anyhow::Result<Self> {
        let node = markdown::parse(source)?;
        let mut plain = String::new();
        let blocks = blocks(&node.children, &mut plain);
        Ok(Self {
            blocks,
            plain: plain.into(),
        })
    }
    pub fn selected(&self, range: Range<usize>) -> Option<&str> {
        self.plain.get(range)
    }
}
fn inline(nodes: &[Node], style: &InlineStyle, value: &mut String, spans: &mut Vec<Span>) {
    for node in nodes {
        if node.kind == Kind::Text {
            let start = value.len();
            value.push_str(&node.text);
            if start < value.len() {
                spans.push(Span {
                    range: start..value.len(),
                    style: style.clone(),
                });
            }
            continue;
        }
        let mut next = style.clone();
        match node.kind {
            Kind::Span(0) => next.italic = true,
            Kind::Span(1) => next.bold = true,
            Kind::Span(2 | 3) => next.target = Some(node.target.clone()),
            Kind::Span(4) => next.code = true,
            Kind::Span(6) => next.strike = true,
            _ => {}
        }
        inline(&node.children, &next, value, spans);
    }
}
fn text(nodes: &[Node], plain: &mut String, code: bool) -> Text {
    let mut value = String::new();
    let mut spans = Vec::new();
    inline(
        nodes,
        &InlineStyle {
            code,
            ..Default::default()
        },
        &mut value,
        &mut spans,
    );
    // HighlightedCodeBlock removes the fence terminator's final newline from display.
    // Preserve authored blank lines; the copy action separately drops trailing line breaks.
    if code && value.ends_with('\n') {
        value.pop();
        spans.retain(|span| span.range.start < value.len());
        for span in &mut spans {
            span.range.end = span.range.end.min(value.len());
        }
    }
    if !plain.is_empty() {
        plain.push_str("\n\n");
    }
    let offset = plain.len();
    plain.push_str(&value);
    Text {
        value: value.into(),
        spans,
        offset,
    }
}
fn blocks(nodes: &[Node], plain: &mut String) -> Vec<Block> {
    let mut result = Vec::new();
    let mut inlines = Vec::new();
    fn flush(inlines: &mut Vec<Node>, result: &mut Vec<Block>, plain: &mut String) {
        if !inlines.is_empty() {
            result.push(Block {
                kind: BlockKind::Paragraph,
                text: Some(text(inlines, plain, false)),
                children: vec![],
            });
            inlines.clear();
        }
    }
    for node in nodes {
        let Kind::Block(kind) = node.kind else {
            inlines.push(node.clone());
            continue;
        };
        flush(&mut inlines, &mut result, plain);
        if kind == 9 && node.children.iter().any(is_image_span) {
            let mut segment = Vec::new();
            for child in &node.children {
                if is_image_span(child) {
                    if !segment.is_empty() {
                        result.push(Block {
                            kind: BlockKind::Paragraph,
                            text: Some(text(&segment, plain, false)),
                            children: vec![],
                        });
                        segment.clear();
                    }
                    result.push(Block {
                        kind: BlockKind::Image {
                            source: child.target.clone(),
                            alt: child.plain_text(),
                        },
                        text: None,
                        children: vec![],
                    });
                } else {
                    segment.push(child.clone());
                }
            }
            if !segment.is_empty() {
                result.push(Block {
                    kind: BlockKind::Paragraph,
                    text: Some(text(&segment, plain, false)),
                    children: vec![],
                });
            }
            continue;
        }
        let kind = match kind {
            0 => {
                result.extend(blocks(&node.children, plain));
                continue;
            }
            1 => BlockKind::Quote(String::new()),
            2 => BlockKind::List(false, 1),
            3 => BlockKind::List(true, node.number),
            4 => BlockKind::Item(node.number),
            5 => BlockKind::Rule,
            6 => BlockKind::Heading(node.number),
            7 => BlockKind::Code(node.target.clone()),
            8 => BlockKind::Html,
            9 => BlockKind::Paragraph,
            10 => BlockKind::Table,
            11 | 12 => BlockKind::TableSection,
            13 => BlockKind::Row,
            14 | 15 => BlockKind::Cell(kind == 14, node.number),
            18 => BlockKind::Quote(node.target.clone()),
            _ => BlockKind::Paragraph,
        };
        let leaf = matches!(
            kind,
            BlockKind::Paragraph
                | BlockKind::Heading(_)
                | BlockKind::Code(_)
                | BlockKind::Html
                | BlockKind::Cell(..)
        );
        result.push(Block {
            kind: kind.clone(),
            text: leaf.then(|| text(&node.children, plain, matches!(kind, BlockKind::Code(_)))),
            children: if leaf {
                vec![]
            } else {
                blocks(&node.children, plain)
            },
        });
    }
    flush(&mut inlines, &mut result, plain);
    result
}

fn is_image_span(node: &Node) -> bool {
    node.kind == Kind::Span(3)
}

impl Document {
    /// Inline image sources in source order.
    pub fn image_sources(&self) -> Vec<String> {
        fn collect(blocks: &[Block], images: &mut Vec<String>) {
            for block in blocks {
                if let BlockKind::Image { source, .. } = &block.kind {
                    images.push(source.clone());
                }
                collect(&block.children, images);
            }
        }
        let mut images = Vec::new();
        collect(&self.blocks, &mut images);
        images
    }
}

pub fn copied_code(code: &str) -> &str {
    // Match the existing code-copy action: pasted code must not execute its last line.
    code.trim_end_matches(['\r', '\n'])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cross_paragraph_copy_uses_visible_unicode_text_and_styles_keep_byte_ranges() {
        let doc =
            Document::markdown("**你好** first\n\nSecond `code` and [link](https://example.test)")
                .unwrap();
        assert_eq!(&*doc.plain, "你好 first\n\nSecond code and link");
        let first = doc.blocks[0].text.as_ref().unwrap();
        assert_eq!(first.spans[0].range, 0..6);
        assert!(first.spans[0].style.bold);
        let start = doc.plain.find("first").unwrap();
        let end = doc.plain.find(" and").unwrap();
        assert_eq!(doc.selected(start..end), Some("first\n\nSecond code"));
        assert_eq!(doc.selected(1..3), None);
    }
    #[test]
    fn markdown_images_become_blocks_and_keep_surrounding_text() {
        let doc =
            Document::markdown("before ![shot](images/a.png) after\n\n![only](b.jpg)").unwrap();
        assert!(matches!(&doc.blocks[0].kind, BlockKind::Paragraph));
        assert_eq!(
            doc.blocks[0].text.as_ref().unwrap().value.as_ref(),
            "before "
        );
        assert!(
            matches!(&doc.blocks[1].kind, BlockKind::Image { source, alt } if source == "images/a.png" && alt == "shot")
        );
        assert_eq!(
            doc.blocks[2].text.as_ref().unwrap().value.as_ref(),
            " after"
        );
        assert!(
            matches!(&doc.blocks[3].kind, BlockKind::Image { source, .. } if source == "b.jpg")
        );
        assert_eq!(doc.image_sources(), vec!["images/a.png", "b.jpg"]);
    }

    #[test]
    fn fenced_code_display_and_copy_match_existing_newline_rules() {
        let doc = Document::markdown("```sh\nprintf hello\n\n```\n\nAfter").unwrap();
        let text = doc.blocks[0].text.as_ref().unwrap();
        assert_eq!(&*text.value, "printf hello\n");
        assert_eq!(text.spans.last().unwrap().range.end, text.value.len());
        assert_eq!(copied_code(&text.value), "printf hello");
        assert_eq!(&*doc.plain, "printf hello\n\n\nAfter");
        assert_eq!(copied_code("a\r\n\r\n"), "a");
        let empty = Document::markdown("```\n```").unwrap();
        assert_eq!(&*empty.blocks[0].text.as_ref().unwrap().value, "");
    }
    #[test]
    fn tight_nested_lists_and_tables_retain_every_leaf_in_source_order() {
        let doc =
            Document::markdown("1. parent\n   - nested\n2. end\n\n| a | b |\n| - | - |\n| c | d |")
                .unwrap();
        assert_eq!(&*doc.plain, "parent\n\nnested\n\nend\n\na\n\nb\n\nc\n\nd");
        assert_eq!(doc.blocks[0].kind, BlockKind::List(true, 1));
        assert_eq!(doc.blocks[1].kind, BlockKind::Table);
    }
}
