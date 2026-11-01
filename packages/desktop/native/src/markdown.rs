use anyhow::{Result, ensure};
use std::{collections::BTreeMap, ffi::c_void, sync::OnceLock};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Block(u32),
    Span(u32),
    Text,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: Kind,
    pub number: u32,
    pub target: String,
    pub title: String,
    pub text: String,
    pub children: Vec<Node>,
}
impl Node {
    fn new(kind: Kind, number: u32, target: String, title: String) -> Self {
        Self {
            kind,
            number,
            target,
            title,
            text: String::new(),
            children: Vec::new(),
        }
    }
    pub fn plain_text(&self) -> String {
        let mut output = self.text.clone();
        for child in &self.children {
            output.push_str(&child.plain_text());
        }
        output
    }
}
struct Builder {
    stack: Vec<Node>,
    root: Option<Node>,
    invalid: bool,
}
unsafe extern "C" {
    fn paseo_md_parse(
        text: *const u8,
        len: u32,
        callback: extern "C" fn(*mut c_void, u32, u32, u32, *const u8, u32, *const u8, u32),
        data: *mut c_void,
    ) -> i32;
}
extern "C" fn event(
    data: *mut c_void,
    event: u32,
    kind: u32,
    number: u32,
    a: *const u8,
    a_len: u32,
    b: *const u8,
    b_len: u32,
) {
    // MD4C calls synchronously. The context and callback strings live until parse returns;
    // strings are copied here, and no C structure layout crosses into Rust.
    let builder = unsafe { &mut *data.cast::<Builder>() };
    let string = |pointer: *const u8, len: u32| {
        if len == 0 {
            String::new()
        } else {
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer, len as usize) })
                .into_owned()
        }
    };
    match event {
        0 | 2 => builder.stack.push(Node::new(
            if event == 0 {
                Kind::Block(kind)
            } else {
                Kind::Span(kind)
            },
            number,
            decode_entities(&string(a, a_len)),
            decode_entities(&string(b, b_len)),
        )),
        1 | 3 => {
            let Some(node) = builder.stack.pop() else {
                builder.invalid = true;
                return;
            };
            if let Some(parent) = builder.stack.last_mut() {
                parent.children.push(node);
            } else {
                builder.root = Some(node);
            }
        }
        4 => {
            let Some(parent) = builder.stack.last_mut() else {
                builder.invalid = true;
                return;
            };
            let text = match kind {
                1 => "�".into(),
                2 | 3 => "\n".into(),
                4 => decode_entities(&string(a, a_len)),
                _ => string(a, a_len),
            };
            if let Some(last) = parent
                .children
                .last_mut()
                .filter(|node| node.kind == Kind::Text)
            {
                last.text.push_str(&text);
            } else {
                let mut node = Node::new(Kind::Text, 0, String::new(), String::new());
                node.text = text;
                parent.children.push(node);
            }
        }
        _ => builder.invalid = true,
    }
}
pub fn parse(source: &str) -> Result<Node> {
    let mut builder = Builder {
        stack: Vec::new(),
        root: None,
        invalid: false,
    };
    let len = u32::try_from(source.len())?;
    let status = unsafe {
        paseo_md_parse(
            source.as_ptr(),
            len,
            event,
            (&mut builder as *mut Builder).cast(),
        )
    };
    ensure!(
        status == 0 && !builder.invalid && builder.stack.is_empty(),
        "Markdown parse failed"
    );
    builder
        .root
        .ok_or_else(|| anyhow::anyhow!("Markdown parser returned no document"))
}
fn decode_entities(text: &str) -> String {
    static ENTITIES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    let names = ENTITIES.get_or_init(|| {
        serde_json::from_str(include_str!("markdown_entities.json"))
            .expect("generated HTML entity table")
    });
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';').filter(|end| *end <= 50) else {
            result.push_str(tail);
            return result;
        };
        let name = &tail[1..end];
        if name.contains('&') {
            result.push('&');
            rest = &tail[1..];
            continue;
        }
        let numeric = name
            .strip_prefix("#x")
            .or_else(|| name.strip_prefix("#X"))
            .and_then(|s| u32::from_str_radix(s, 16).ok())
            .or_else(|| name.strip_prefix('#').and_then(|s| s.parse().ok()));
        if let Some(mut number) = numeric {
            if number < 9
                || number == 11
                || (number > 13 && number < 32)
                || (number > 126 && number < 160)
                || (64975 < number && number < 65008)
                || number & 65535 == 65535
                || number & 65535 == 65534
            {
                number = 0xfffd;
            }
            result.push(char::from_u32(number).filter(|c| *c != '\0').unwrap_or('�'));
        } else if let Some(value) = names.get(name) {
            result.push_str(value);
        } else {
            result.push_str(&tail[..=end]);
        }
        rest = &tail[end + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_parser_preserves_nested_emphasis_unicode_and_literal_code() {
        let doc =
            parse("## 中文 **bold _nested_** &amp; `x < y`\n\n```rust\nlet x = \"&amp;\";\n```\n")
                .unwrap();
        assert_eq!(doc.children[0].kind, Kind::Block(6));
        assert_eq!(doc.children[0].number, 2);
        assert_eq!(doc.children[0].plain_text(), "中文 bold nested & x < y");
        assert_eq!(doc.children[1].kind, Kind::Block(7));
        assert_eq!(doc.children[1].target, "rust");
        assert_eq!(doc.children[1].plain_text(), "let x = \"&amp;\";\n");
    }
    #[test]
    fn tables_tasks_links_and_soft_breaks_keep_the_engine_semantics() {
        let doc=parse("- [x] done\n- [ ] next\n\n| Key | Value |\n| --- | --- |\n| a | b |\n\n[link](https://example.test/a%20b \"Title\")\nnext line").unwrap();
        assert_eq!(
            doc.children[0]
                .children
                .iter()
                .map(|item| item.number)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(doc.children[1].kind, Kind::Block(10));
        let link = &doc.children[2].children[0];
        assert_eq!(link.kind, Kind::Span(2));
        assert_eq!(link.target, "https://example.test/a%20b");
        assert_eq!(link.title, "Title");
        assert!(doc.children[2].plain_text().contains("\nnext line"));
    }
    #[test]
    fn empty_incomplete_and_embedded_nul_text_do_not_fabricate_markup() {
        assert!(parse("").unwrap().children.is_empty());
        assert_eq!(
            parse("unfinished **bold").unwrap().plain_text(),
            "unfinished **bold"
        );
        assert!(parse("a\0b").unwrap().plain_text().contains('�'));
        assert_eq!(
            parse("||literal spoiler||").unwrap().plain_text(),
            "||literal spoiler||"
        );
    }
}
