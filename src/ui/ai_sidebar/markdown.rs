//! Keep the native Markdown presentation without granting model text navigation
//! or resource-loading behavior. Use exactly the GFM parser used by TextView;
//! applying offsets from a different Markdown grammar leaves parsing gaps.

use ::markdown::{ParseOptions, mdast::Node, to_mdast};

pub(super) fn assistant_display_markdown(content: &str) -> String {
    let Ok(tree) = to_mdast(content, &ParseOptions::gfm()) else {
        return literal_message(content);
    };
    let mut replacements = Vec::new();
    let mut pending = vec![&tree];
    while let Some(node) = pending.pop() {
        let replacement = match node {
            Node::Html(html) => Some(literal_markdown(&html.value)),
            Node::Image(image) => Some(image_label(&image.alt)),
            Node::ImageReference(image) => Some(image_label(&image.alt)),
            Node::Link(_) | Node::LinkReference(_) => {
                let label = visible_text(node);
                Some(literal_markdown(if label.is_empty() {
                    "（链接）"
                } else {
                    &label
                }))
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            let Some(position) = node.position() else {
                return literal_message(content);
            };
            replacements.push((position.start.offset..position.end.offset, replacement));
        } else if let Some(children) = node.children() {
            pending.extend(children.iter().rev());
        }
    }
    if replacements.is_empty() {
        return content.to_owned();
    }

    // Build once in source order rather than repeatedly moving the suffix for
    // every replacement in a response with many citations or images.
    let mut display = String::with_capacity(content.len());
    let mut cursor = 0;
    for (range, replacement) in replacements {
        let Some(unchanged) = content.get(cursor..range.start) else {
            return literal_message(content);
        };
        if content.get(range.clone()).is_none() {
            return literal_message(content);
        }
        display.push_str(unchanged);
        display.push_str(&replacement);
        cursor = range.end;
    }
    display.push_str(&content[cursor..]);

    // Replacing a block can change how following Markdown is parsed (including
    // reference definitions). Verify the exact final source that TextView gets.
    match to_mdast(&display, &ParseOptions::gfm()) {
        Ok(tree) if has_no_active_content(&tree) => display,
        _ => literal_message(content),
    }
}

fn image_label(alt: &str) -> String {
    literal_markdown(if alt.is_empty() { "（图片）" } else { alt })
}

fn visible_text(node: &Node) -> String {
    let mut text = String::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        match node {
            Node::Image(image) => text.push_str(&image.alt),
            Node::ImageReference(image) => text.push_str(&image.alt),
            Node::Break(_) => text.push('\n'),
            _ => {
                if let Some(children) = node.children() {
                    pending.extend(children.iter().rev());
                } else {
                    text.push_str(&node.to_string());
                }
            }
        }
    }
    text
}

fn literal_markdown(text: &str) -> String {
    use std::fmt::Write as _;

    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_punctuation() {
            // Character references are decoded as text, without reinterpreting
            // their punctuation as links, HTML, emphasis or Markdown delimiters.
            write!(&mut escaped, "&#{};", character as u32)
                .expect("formatting into a String cannot fail");
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn literal_message(content: &str) -> String {
    // Keep every original line and indentation in the exceptional fallback.
    // A fence longer than any run in the input cannot be closed by model text.
    let mut longest = 0usize;
    let mut current = 0usize;
    for character in content.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat(longest.saturating_add(1).max(3));
    format!("{fence}text\n{content}\n{fence}\n")
}

fn has_no_active_content(tree: &Node) -> bool {
    let mut pending = vec![tree];
    while let Some(node) = pending.pop() {
        if matches!(
            node,
            Node::Html(_)
                | Node::Image(_)
                | Node::ImageReference(_)
                | Node::Link(_)
                | Node::LinkReference(_)
        ) {
            return false;
        }
        if let Some(children) = node.children() {
            pending.extend(children);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display_tree(markdown: &str) -> Node {
        let display = assistant_display_markdown(markdown);
        let tree = to_mdast(&display, &ParseOptions::gfm()).unwrap();
        assert!(has_no_active_content(&tree), "unsafe display: {display}");
        tree
    }

    #[test]
    fn preserves_markdown_structure_and_code_verbatim() {
        let content = "# 标题\n\n**粗体**、*强调*、~~删除~~ 和 `a < b`。\n\n> 引用\n\n1. 有序\n2. 第二项\n\n- 无序\n- [x] 已完成\n\n| 名称 | 值 |\n| --- | --- |\n| 示例 | 42 |\n\n---\n\n```python\n    print('[link](file:///secret)')\n    # <img src=https://example.test/tracker>\n```\n";
        assert_eq!(assistant_display_markdown(content), content);
        let tree = display_tree(content);
        let children = tree.children().unwrap();
        assert!(children.iter().any(|node| matches!(node, Node::Heading(_))));
        assert!(
            children
                .iter()
                .any(|node| matches!(node, Node::Blockquote(_)))
        );
        assert!(children.iter().any(|node| matches!(node, Node::Table(_))));
        assert!(children.iter().any(|node| matches!(node, Node::Code(_))));
    }

    #[test]
    fn links_and_autolinks_keep_labels_without_navigation() {
        let content = "[中文 **标签**](file:///C:/private.txt) [命令](shell:AppsFolder) [脚本](javascript:alert(1))\n\n<https://example.test/path> https://example.test/path www.example.test user@example.test\n\n[完整引用][ref] [ref][] [ref]\n\n[ref]: https://example.test/reference\n";
        let tree = display_tree(content);
        let text = visible_text(&tree);
        for label in [
            "中文 标签",
            "命令",
            "脚本",
            "https://example.test/path",
            "www.example.test",
            "user@example.test",
            "完整引用",
            "ref",
        ] {
            assert!(text.contains(label), "missing label: {label}; {text}");
        }
        assert!(!text.contains("file:///C:/private.txt"));
        assert!(!text.contains("shell:AppsFolder"));
    }

    #[test]
    fn remote_and_local_images_keep_alt_without_loading() {
        let content = "![网络图片](https://example.test/tracker) ![本地图片](file:///C:/private.png) ![数据图片](data:image/png;base64,AAAA) ![参考图片][image]\n\n[![有链接的图片](https://example.test/image)](https://example.test/link) ![](https://example.test/empty)\n\n[image]: https://example.test/ref\n";
        let text = visible_text(&display_tree(content));
        for label in [
            "网络图片",
            "本地图片",
            "数据图片",
            "参考图片",
            "有链接的图片",
            "（图片）",
        ] {
            assert!(text.contains(label), "missing alt: {label}; {text}");
        }
        assert!(!text.contains("private.png"));
        assert!(!text.contains("base64"));
    }

    #[test]
    fn raw_html_is_visible_literal_text() {
        let content = "<div>\n<img src=\"https://example.test/tracker\">\n<a href=\"file:///C:/private\">原始链接</a>\n<script>alert('x')</script>\n</div>\n\n行内 <img src='https://example.test/inline'> 和 <b>文字</b>。";
        let text = visible_text(&display_tree(content));
        assert!(text.contains("<img src=\"https://example.test/tracker\">"));
        assert!(text.contains("<script>alert('x')</script>"));
        assert!(text.contains("<b>文字</b>"));
    }

    #[test]
    fn labels_cannot_become_html_or_markdown_when_decoded() {
        let content = "[&lt;img src=x&gt;](https://example.test) ![https://example.test](x) [www.example.test](x) [user@example.test](x) [\u{4e2d}\\[evil\\]\\(file:///x\\)](x)";
        let text = visible_text(&display_tree(content));
        assert!(text.contains("<img src=x>"));
        assert!(text.contains("https://example.test"));
        assert!(text.contains("中[evil](file:///x)"));
    }

    #[test]
    fn streaming_prefixes_remain_safe_and_completed_code_keeps_its_contents() {
        let content = "# 回复\r\n\r\n[说明](https://example.test)\r\n\r\n```html\r\n<img src=\"https://example.test/code\">\r\n```\r\n\r\n![图片](https://example.test/image)";
        for (offset, _) in content.char_indices() {
            display_tree(&content[..offset]);
        }
        let tree = display_tree(content);
        let code = tree
            .children()
            .unwrap()
            .iter()
            .find_map(|node| match node {
                Node::Code(code) => Some(code),
                _ => None,
            })
            .unwrap();
        assert_eq!(code.lang.as_deref(), Some("html"));
        assert_eq!(code.value, "<img src=\"https://example.test/code\">");
    }

    #[test]
    fn literal_fallback_cannot_activate_markdown() {
        let content = "    <img src=x>\n[link](file:///x)\n![image](https://example.test)\nwww.example.test user@example.test\n\n``````\n<script>\n``````";
        let tree = to_mdast(&literal_message(content), &ParseOptions::gfm()).unwrap();
        assert!(has_no_active_content(&tree));
        assert_eq!(tree.children().unwrap().len(), 1);
        let Node::Code(code) = &tree.children().unwrap()[0] else {
            panic!("the complete fallback must remain a single literal code block");
        };
        assert_eq!(code.value, content);
    }
}
