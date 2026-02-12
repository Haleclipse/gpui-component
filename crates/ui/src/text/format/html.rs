extern crate markup5ever_rcdom as rcdom;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{DefiniteLength, SharedString, px, relative};
use html5ever::tendril::TendrilSink;
use html5ever::{LocalName, ParseOpts, local_name, parse_document};
use markup5ever_rcdom::{Node, NodeData, RcDom};

use crate::text::document::ParsedDocument;
use crate::text::node::{
    self, BlockNode, CodeBlock, ImageNode, InlineNode, LinkMark, NodeContext, Paragraph, Table,
    TableRow, TextMark,
};

const BLOCK_ELEMENTS: [&str; 35] = [
    "html",
    "body",
    "head",
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "summary",
    "dialog",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
    "style",
    "script",
];

/// Parse HTML into AST Node.
pub(crate) fn parse(source: &str, cx: &mut NodeContext) -> Result<ParsedDocument, SharedString> {
    let opts = ParseOpts {
        ..Default::default()
    };

    let bytes = cleanup_html(&source);
    let mut cursor = std::io::Cursor::new(bytes);
    // Ref
    // https://github.com/servo/html5ever/blob/main/rcdom/examples/print-rcdom.rs
    let dom = parse_document(RcDom::default(), opts)
        .from_utf8()
        .read_from(&mut cursor)
        .map_err(|e| SharedString::from(format!("{:?}", e)))?;

    let mut paragraph = Paragraph::default();
    // NOTE: The outer paragraph is not used.
    let node: BlockNode =
        parse_node(&dom.document, &mut paragraph, cx).unwrap_or(BlockNode::Unknown);
    let node = node.compact();

    Ok(ParsedDocument {
        source: source.to_string().into(),
        blocks: vec![node],
    })
}

fn cleanup_html(source: &str) -> Vec<u8> {
    let mut w = std::io::Cursor::new(vec![]);
    let mut r = std::io::Cursor::new(source);
    let mut minify = super::html5minify::Minifier::new(&mut w);
    minify.omit_doctype(true);
    if let Ok(()) = minify.minify(&mut r) {
        w.into_inner()
    } else {
        source.bytes().collect()
    }
}

fn attr_value(attrs: &RefCell<Vec<html5ever::Attribute>>, name: LocalName) -> Option<String> {
    attrs.borrow().iter().find_map(|attr| {
        if attr.name.local == name {
            Some(attr.value.to_string())
        } else {
            None
        }
    })
}

/// Check if an element's class attribute contains an emoji class name.
///
/// Discourse marks emoji images with `class="emoji"` or `class="emoji-only"`.
fn is_emoji_class(attrs: &RefCell<Vec<html5ever::Attribute>>) -> bool {
    attr_value(attrs, local_name!("class"))
        .map(|c| {
            c.split_whitespace()
                .any(|cls| cls == "emoji" || cls == "emoji-only")
        })
        .unwrap_or(false)
}

/// Get style properties to HashMap
/// TODO: Use cssparser to parse style attribute.
fn style_attrs(attrs: &RefCell<Vec<html5ever::Attribute>>) -> HashMap<String, String> {
    let mut styles = HashMap::new();
    let Some(css_text) = attr_value(attrs, local_name!("style")) else {
        return styles;
    };

    for decl in css_text.split(';') {
        let mut parts = decl.splitn(2, ':');
        if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
            styles.insert(
                key.trim().to_lowercase().to_string(),
                value.trim().to_string(),
            );
        }
    }

    styles
}

/// Parse length value from style attribute.
///
/// When is percentage, it will be converted to relative length.
/// Else, it will be converted to pixels.
fn value_to_length(value: &str) -> Option<DefiniteLength> {
    if value.ends_with("%") {
        value
            .trim_end_matches("%")
            .parse::<f32>()
            .ok()
            .map(|v| relative(v / 100.))
    } else {
        value
            .trim_end_matches("px")
            .parse()
            .ok()
            .map(|v| px(v).into())
    }
}

/// Get width, height from attributes or parse them from style attribute.
fn attr_width_height(
    attrs: &RefCell<Vec<html5ever::Attribute>>,
) -> (Option<DefiniteLength>, Option<DefiniteLength>) {
    let mut width = None;
    let mut height = None;

    if let Some(value) = attr_value(attrs, local_name!("width")) {
        width = value_to_length(&value);
    }

    if let Some(value) = attr_value(attrs, local_name!("height")) {
        height = value_to_length(&value);
    }

    if width.is_none() || height.is_none() {
        let styles = style_attrs(attrs);
        if width.is_none() {
            width = styles.get("width").and_then(|v| value_to_length(&v));
        }
        if height.is_none() {
            height = styles.get("height").and_then(|v| value_to_length(&v));
        }
    }

    (width, height)
}

fn parse_table_row(table: &mut Table, node: &Rc<Node>) {
    let mut row = TableRow::default();
    let mut count = 0;
    for child in node.children.borrow().iter() {
        match child.data {
            NodeData::Element {
                ref name,
                ref attrs,
                ..
            } if name.local == local_name!("td") || name.local == local_name!("th") => {
                if child.children.borrow().is_empty() {
                    continue;
                }

                count += 1;
                parse_table_cell(&mut row, child, attrs);
            }
            _ => {}
        }
    }

    if count > 0 {
        table.children.push(row);
    }
}

fn parse_table_cell(
    row: &mut node::TableRow,
    node: &Rc<Node>,
    attrs: &RefCell<Vec<html5ever::Attribute>>,
) {
    let mut paragraph = Paragraph::default();
    for child in node.children.borrow().iter() {
        parse_paragraph(&mut paragraph, child);
    }
    let width = attr_width_height(attrs).0;
    let table_cell = node::TableCell {
        children: paragraph,
        width,
    };
    row.children.push(table_cell);
}

/// Trim text but leave at least one space.
///
/// - Before: " \r\n Hello world \t "
/// - After: " Hello world "
#[allow(dead_code)]
fn trim_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());

    for (i, c) in text.chars().enumerate() {
        if c.is_whitespace() {
            if i > 0 && out.ends_with(' ') {
                continue;
            }
        }
        out.push(c);
    }

    out
}

fn parse_paragraph(
    paragraph: &mut Paragraph,
    node: &Rc<Node>,
) -> (String, Vec<(Range<usize>, TextMark)>) {
    let mut text = String::new();
    let mut marks = vec![];

    /// Append new_text and new_marks to text and marks.
    fn merge_child_text(
        text: &mut String,
        marks: &mut Vec<(Range<usize>, TextMark)>,
        new_text: &str,
        new_marks: &[(Range<usize>, TextMark)],
    ) {
        let offset = text.len();
        text.push_str(new_text);
        for (range, style) in new_marks {
            marks.push((range.start + offset..new_text.len() + offset, style.clone()));
        }
    }

    match &node.data {
        NodeData::Text { contents } => {
            let part = &contents.borrow();
            text.push_str(&part);
            paragraph.push_str(&text);
        }
        NodeData::Element { name, attrs, .. } => match name.local {
            local_name!("em") | local_name!("i") => {
                let mut child_paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                    merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
                }
                marks.push((0..text.len(), TextMark::default().italic()));
                paragraph.push(InlineNode::new(&text).marks(marks.clone()));
            }
            local_name!("strong") | local_name!("b") => {
                let mut child_paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                    merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
                }
                marks.push((0..text.len(), TextMark::default().bold()));
                paragraph.push(InlineNode::new(&text).marks(marks.clone()));
            }
            local_name!("del") | local_name!("s") => {
                let mut child_paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                    merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
                }
                marks.push((0..text.len(), TextMark::default().strikethrough()));
                paragraph.push(InlineNode::new(&text).marks(marks.clone()));
            }
            local_name!("code") => {
                let mut child_paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                    merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
                }
                marks.push((0..text.len(), TextMark::default().code()));
                paragraph.push(InlineNode::new(&text).marks(marks.clone()));
            }
            local_name!("a") => {
                let mut child_paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                    merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
                }

                marks.push((
                    0..text.len(),
                    TextMark::default().link(LinkMark {
                        url: attr_value(&attrs, local_name!("href"))
                            .unwrap_or_default()
                            .into(),
                        title: attr_value(&attrs, local_name!("title")).map(Into::into),
                        ..Default::default()
                    }),
                ));
                paragraph.push(InlineNode::new(&text).marks(marks.clone()));
            }
            local_name!("img") => {
                let Some(src) = attr_value(attrs, local_name!("src")) else {
                    if cfg!(debug_assertions) {
                        tracing::warn!("Image node missing src attribute");
                    }
                    return (text, marks);
                };

                let alt = attr_value(attrs, local_name!("alt"));
                let title = attr_value(attrs, local_name!("title"));
                let (width, height) = attr_width_height(attrs);

                paragraph.push_image(ImageNode {
                    url: src.into(),
                    link: None,
                    alt: alt.map(Into::into),
                    width,
                    height,
                    title: title.map(Into::into),
                    is_inline: is_emoji_class(attrs),
                });
            }
            _ => {
                // All unknown tags to as text
                let mut child_paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                    merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
                }
                paragraph.push(InlineNode::new(&text).marks(marks.clone()));
            }
        },
        _ => {
            let mut child_paragraph = Paragraph::default();
            for child in node.children.borrow().iter() {
                let (child_text, child_marks) = parse_paragraph(&mut child_paragraph, &child);
                merge_child_text(&mut text, &mut marks, &child_text, &child_marks);
            }
            paragraph.push(InlineNode::new(&text).marks(marks.clone()));
        }
    }

    (text, marks)
}

fn parse_node(
    node: &Rc<Node>,
    paragraph: &mut Paragraph,
    cx: &mut NodeContext,
) -> Option<BlockNode> {
    match node.data {
        NodeData::Text { ref contents } => {
            let text = contents.borrow().to_string();
            if text.len() > 0 {
                paragraph.push_str(&text);
            }

            None
        }
        NodeData::Element {
            ref name,
            ref attrs,
            ..
        } => match name.local {
            local_name!("br") => Some(BlockNode::Break {
                html: true,
                span: None,
            }),
            local_name!("h1")
            | local_name!("h2")
            | local_name!("h3")
            | local_name!("h4")
            | local_name!("h5")
            | local_name!("h6") => {
                let mut children = vec![];
                consume_paragraph(&mut children, paragraph);

                let level = name
                    .local
                    .chars()
                    .last()
                    .unwrap_or('6')
                    .to_digit(10)
                    .unwrap_or(6) as u8;

                let mut paragraph = Paragraph::default();
                for child in node.children.borrow().iter() {
                    parse_paragraph(&mut paragraph, child);
                }

                let heading = BlockNode::Heading {
                    level,
                    children: paragraph,
                    span: None,
                };
                if children.len() > 0 {
                    children.push(heading);

                    Some(BlockNode::Root {
                        children,
                        span: None,
                    })
                } else {
                    Some(heading)
                }
            }
            local_name!("img") => {
                let Some(src) = attr_value(attrs, local_name!("src")) else {
                    if cfg!(debug_assertions) {
                        tracing::warn!("image node missing src attribute");
                    }
                    return None;
                };

                let alt = attr_value(&attrs, local_name!("alt"));
                let title = attr_value(&attrs, local_name!("title"));
                let (width, height) = attr_width_height(&attrs);
                let is_inline = is_emoji_class(attrs);

                if is_inline {
                    // Inline emoji: accumulate into the current paragraph
                    // so they flow with surrounding text instead of creating
                    // separate block-level paragraphs.
                    paragraph.push_image(ImageNode {
                        url: src.into(),
                        link: None,
                        title: title.map(Into::into),
                        alt: alt.map(Into::into),
                        width,
                        height,
                        is_inline,
                    });
                    None
                } else {
                    // Block image: flush current paragraph, create a new one
                    let mut children = vec![];
                    consume_paragraph(&mut children, paragraph);

                    let mut new_paragraph = Paragraph::default();
                    new_paragraph.push_image(ImageNode {
                        url: src.into(),
                        link: None,
                        title: title.map(Into::into),
                        alt: alt.map(Into::into),
                        width,
                        height,
                        is_inline,
                    });

                    if children.len() > 0 {
                        children.push(BlockNode::Paragraph(new_paragraph));
                        Some(BlockNode::Root {
                            children,
                            span: None,
                        })
                    } else {
                        Some(BlockNode::Paragraph(new_paragraph))
                    }
                }
            }
            local_name!("ul") | local_name!("ol") => {
                let ordered = name.local == local_name!("ol");
                let children = consume_children_nodes(node, paragraph, cx);
                Some(BlockNode::List {
                    children,
                    ordered,
                    span: None,
                })
            }
            local_name!("li") => {
                let mut children = vec![];
                consume_paragraph(&mut children, paragraph);

                for child in node.children.borrow().iter() {
                    let mut child_paragraph = Paragraph::default();
                    if let Some(child_node) = parse_node(child, &mut child_paragraph, cx) {
                        children.push(child_node);
                    }
                    if child_paragraph.text_len() > 0 {
                        // If last child is paragraph, merge child
                        if let Some(last_child) = children.last_mut() {
                            if let BlockNode::Paragraph(last_paragraph) = last_child {
                                last_paragraph.merge(child_paragraph);
                                continue;
                            }
                        }

                        children.push(BlockNode::Paragraph(child_paragraph));
                    }
                }

                consume_paragraph(&mut children, paragraph);

                Some(BlockNode::ListItem {
                    children,
                    spread: false,
                    checked: None,
                    span: None,
                })
            }
            local_name!("table") => {
                let mut children = vec![];
                consume_paragraph(&mut children, paragraph);

                let mut table = Table::default();
                for child in node.children.borrow().iter() {
                    match child.data {
                        NodeData::Element { ref name, .. }
                            if name.local == local_name!("tbody")
                                || name.local == local_name!("thead") =>
                        {
                            for sub_child in child.children.borrow().iter() {
                                parse_table_row(&mut table, &sub_child);
                            }
                        }
                        _ => {
                            parse_table_row(&mut table, &child);
                        }
                    }
                }
                consume_paragraph(&mut children, paragraph);

                let table = BlockNode::Table(table);
                if children.len() > 0 {
                    children.push(table);
                    Some(BlockNode::Root {
                        children,
                        span: None,
                    })
                } else {
                    Some(table)
                }
            }
            local_name!("blockquote") => {
                let children = consume_children_nodes(node, paragraph, cx);
                Some(BlockNode::Blockquote {
                    children,
                    span: None,
                })
            }
            local_name!("pre") => {
                let mut children = vec![];
                consume_paragraph(&mut children, paragraph);

                if let Some((code_text, lang)) = extract_pre_code(node, attrs) {
                    let code_block = BlockNode::CodeBlock(CodeBlock::new(
                        code_text.into(),
                        lang.map(SharedString::from),
                        &cx.style.highlight_theme,
                        None::<node::Span>,
                    ));
                    if children.is_empty() {
                        Some(code_block)
                    } else {
                        children.push(code_block);
                        Some(BlockNode::Root {
                            children,
                            span: None,
                        })
                    }
                } else {
                    // Fallback: treat as generic block element
                    for child in node.children.borrow().iter() {
                        if let Some(child_node) = parse_node(child, paragraph, cx) {
                            children.push(child_node);
                        }
                    }
                    consume_paragraph(&mut children, paragraph);
                    if children.is_empty() {
                        None
                    } else {
                        Some(BlockNode::Root {
                            children,
                            span: None,
                        })
                    }
                }
            }
            local_name!("style") | local_name!("script") => None,
            _ => {
                if BLOCK_ELEMENTS.contains(&name.local.trim()) {
                    let mut children: Vec<BlockNode> = vec![];

                    // Case:
                    //
                    // Hello <p>Inner text of block element</p> World

                    // Insert before text as a node -- The "Hello"
                    consume_paragraph(&mut children, paragraph);

                    // Inner of the block element -- The "Inner text of block element"
                    for child in node.children.borrow().iter() {
                        if let Some(child_node) = parse_node(child, paragraph, cx) {
                            children.push(child_node);
                        }
                    }
                    consume_paragraph(&mut children, paragraph);

                    if children.is_empty() {
                        None
                    } else {
                        Some(BlockNode::Root {
                            children,
                            span: None,
                        })
                    }
                } else {
                    // Others to as Inline
                    parse_paragraph(paragraph, node);

                    if paragraph.is_image() {
                        Some(BlockNode::Paragraph(paragraph.take()))
                    } else {
                        None
                    }
                }
            }
        },
        NodeData::Document => {
            let children = consume_children_nodes(node, paragraph, cx);
            Some(BlockNode::Root {
                children,
                span: None,
            })
        }
        NodeData::Doctype { .. }
        | NodeData::Comment { .. }
        | NodeData::ProcessingInstruction { .. } => None,
    }
}

fn consume_children_nodes(
    node: &Node,
    paragraph: &mut Paragraph,
    cx: &mut NodeContext,
) -> Vec<BlockNode> {
    let mut children = vec![];
    consume_paragraph(&mut children, paragraph);
    for child in node.children.borrow().iter() {
        if let Some(child_node) = parse_node(child, paragraph, cx) {
            children.push(child_node);
        }
        consume_paragraph(&mut children, paragraph);
    }

    children
}

fn consume_paragraph(children: &mut Vec<BlockNode>, paragraph: &mut Paragraph) {
    if paragraph.is_empty() {
        return;
    }

    children.push(BlockNode::Paragraph(paragraph.take()));
}

/// Extract code text and language from a `<pre>` element.
///
/// Handles patterns commonly produced by Discourse and other Markdown processors:
/// - `<pre><code class="language-rust">code</code></pre>`
/// - `<pre><code class="lang-rust">code</code></pre>`
/// - `<pre><code>code</code></pre>` (no language)
/// - `<pre>code</pre>` (no `<code>` wrapper)
fn extract_pre_code(
    node: &Rc<Node>,
    _pre_attrs: &RefCell<Vec<html5ever::Attribute>>,
) -> Option<(String, Option<String>)> {
    // Look for a <code> child element
    for child in node.children.borrow().iter() {
        if let NodeData::Element {
            ref name,
            ref attrs,
            ..
        } = child.data
        {
            if name.local == local_name!("code") {
                // Extract language from <code class="language-*"> or <code class="lang-*">
                let lang = extract_code_language(attrs);

                // Collect all text content from the <code> element
                let text = collect_text_content(child);
                if !text.is_empty() {
                    return Some((text, lang));
                }
            }
        }
    }

    // If no <code> child, collect text directly from <pre>
    let text = collect_text_content(node);
    if !text.is_empty() {
        Some((text, None))
    } else {
        None
    }
}

/// Extract language identifier from a `<code>` element's class attribute.
///
/// Recognises `class="language-*"` and `class="lang-*"` patterns.
fn extract_code_language(attrs: &RefCell<Vec<html5ever::Attribute>>) -> Option<String> {
    let class = attr_value(attrs, local_name!("class"))?;
    for cls in class.split_whitespace() {
        if let Some(lang) = cls.strip_prefix("language-") {
            return Some(lang.to_string());
        }
        if let Some(lang) = cls.strip_prefix("lang-") {
            return Some(lang.to_string());
        }
    }
    None
}

/// Recursively collect all text content from a DOM node.
fn collect_text_content(node: &Rc<Node>) -> String {
    let mut text = String::new();
    collect_text_recursive(node, &mut text);
    text
}

fn collect_text_recursive(node: &Rc<Node>, text: &mut String) {
    match &node.data {
        NodeData::Text { contents } => {
            text.push_str(&contents.borrow());
        }
        _ => {
            for child in node.children.borrow().iter() {
                collect_text_recursive(child, text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{px, relative};

    use crate::text::{
        document::ParsedDocument,
        node::{BlockNode, ImageNode, InlineNode, NodeContext, Paragraph},
    };

    use super::trim_text;

    #[test]
    fn test_cleanup_html() {
        let html = r#"<p>
            and
            <code>code</code>
            text
        </p>"#;
        let cleaned = super::cleanup_html(html);
        assert_eq!(
            String::from_utf8(cleaned).unwrap(),
            "<p>and <code>code</code> text"
        );

        let html = r#"<p>
            and
            <em>   <code>code</code>   <i>italic</i>   </em>
            text
        </p>"#;
        let cleaned = super::cleanup_html(html);
        assert_eq!(
            String::from_utf8(cleaned).unwrap(),
            "<p>and <em><code>code</code> <i>italic</i></em> text"
        );
    }

    #[test]
    fn test_trim_text() {
        assert_eq!(trim_text("  \n\tHello world \t\r "), " Hello world ",);
    }

    #[test]
    fn test_keep_spaces() {
        let html = r#"<p>and <code>code</code> text</p>"#;
        let mut cx = NodeContext::default();
        let node = super::parse(html, &mut cx).unwrap();
        assert_eq!(node.to_markdown(), "and `code` text");

        let html = r#"
            <div>
            <p>
                and
                <em>   <code>code</code>   <i>italic</i>   </em>
                text
            </p>
            <p>
                <img src="https://example.com/image.png" alt="Example" width="100" height="200" title="Example Image" />
            </p>
            <ul>
                <li>Item 1</li>
                <li>Item 2
                </li>
            </ul>
            </div>
        "#;
        let node = super::parse(html, &mut cx).unwrap();
        assert_eq!(
            node.to_markdown(),
            indoc::indoc! {r#"
            and *code italic* text

            ![Example](https://example.com/image.png "Example Image")

            - Item 1
            - Item 2
            "#}
            .trim()
        );
    }

    #[test]
    fn test_value_to_length() {
        assert_eq!(super::value_to_length("100px"), Some(px(100.).into()));
        assert_eq!(super::value_to_length("100%"), Some(relative(1.)));
        assert_eq!(super::value_to_length("56%"), Some(relative(0.56)));
        assert_eq!(super::value_to_length("240"), Some(px(240.).into()));
    }

    #[test]
    fn test_image() {
        let html = r#"<img src="https://example.com/image.png" alt="Example" width="100" height="200" title="Example Image" />"#;
        let mut cx = NodeContext::default();
        let node = super::parse(html, &mut cx).unwrap();
        assert_eq!(
            node,
            ParsedDocument {
                source: html.to_string().into(),
                blocks: vec![BlockNode::Paragraph(Paragraph {
                    span: None,
                    children: vec![InlineNode::image(ImageNode {
                        url: "https://example.com/image.png".to_string().into(),
                        alt: Some("Example".to_string().into()),
                        width: Some(px(100.).into()),
                        height: Some(px(200.).into()),
                        title: Some("Example Image".to_string().into()),
                        ..Default::default()
                    })],
                    ..Default::default()
                })]
            }
        );

        let html = r#"<img src="https://example.com/image.png" alt="Example" style="width: 80%" title="Example Image" />"#;
        let node = super::parse(html, &mut cx).unwrap();
        assert_eq!(
            node,
            ParsedDocument {
                source: html.to_string().into(),
                blocks: vec![BlockNode::Paragraph(Paragraph {
                    span: None,
                    children: vec![InlineNode::image(ImageNode {
                        url: "https://example.com/image.png".to_string().into(),
                        alt: Some("Example".to_string().into()),
                        width: Some(relative(0.8)),
                        height: None,
                        title: Some("Example Image".to_string().into()),
                        ..Default::default()
                    })],
                    ..Default::default()
                })]
            }
        );
    }

    #[test]
    fn test_pre_code_block_with_language() {
        // Discourse style: <pre><code class="lang-rust">
        let html = r#"<pre><code class="lang-rust">fn main() {
    println!("Hello");
}</code></pre>"#;
        let mut cx = NodeContext::default();
        let doc = super::parse(html, &mut cx).unwrap();
        // Should produce a CodeBlock
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            BlockNode::CodeBlock(cb) => {
                assert_eq!(cb.lang(), Some("rust".into()));
                assert_eq!(
                    cb.code().as_ref(),
                    "fn main() {\n    println!(\"Hello\");\n}"
                );
            }
            other => panic!("Expected CodeBlock, got: {:?}", other),
        }
    }

    #[test]
    fn test_pre_code_block_language_prefix() {
        // Standard Markdown style: <pre><code class="language-javascript">
        let html = r#"<pre><code class="language-javascript">const x = 42;</code></pre>"#;
        let mut cx = NodeContext::default();
        let doc = super::parse(html, &mut cx).unwrap();
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            BlockNode::CodeBlock(cb) => {
                assert_eq!(cb.lang(), Some("javascript".into()));
                assert_eq!(cb.code().as_ref(), "const x = 42;");
            }
            other => panic!("Expected CodeBlock, got: {:?}", other),
        }
    }

    #[test]
    fn test_pre_code_block_no_language() {
        // No language specified
        let html = r#"<pre><code>plain code here</code></pre>"#;
        let mut cx = NodeContext::default();
        let doc = super::parse(html, &mut cx).unwrap();
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            BlockNode::CodeBlock(cb) => {
                assert_eq!(cb.lang(), None);
                assert_eq!(cb.code().as_ref(), "plain code here");
            }
            other => panic!("Expected CodeBlock, got: {:?}", other),
        }
    }

    #[test]
    fn test_pre_without_code() {
        // <pre> without <code> wrapper
        let html = r#"<pre>raw preformatted text</pre>"#;
        let mut cx = NodeContext::default();
        let doc = super::parse(html, &mut cx).unwrap();
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            BlockNode::CodeBlock(cb) => {
                assert_eq!(cb.lang(), None);
                assert_eq!(cb.code().as_ref(), "raw preformatted text");
            }
            other => panic!("Expected CodeBlock, got: {:?}", other),
        }
    }

    #[test]
    fn test_pre_code_to_markdown() {
        let html = r#"<pre><code class="lang-rust">let x = 1;</code></pre>"#;
        let mut cx = NodeContext::default();
        let doc = super::parse(html, &mut cx).unwrap();
        assert_eq!(doc.to_markdown(), "```rust\nlet x = 1;\n```");
    }
}
