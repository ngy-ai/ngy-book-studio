//! Block extraction and translation-source slots.

use super::*;

#[test]
fn block_texts_extract_innermost_blocks_and_ignore_hidden_text() {
    let source = r#"<div id="sbo-rt-content">
            <h1>Rust Brain Teasers</h1>
            <script>ignored()</script>
            <p>Copyright 2022
               second line</p>
            <ul><li><p>Nested item</p></li></ul>
            <table><tbody><tr><td>单元格</td></tr></tbody></table>
        </div>"#;
    assert_eq!(
        block_texts_from_html(source).unwrap(),
        vec![
            "Rust Brain Teasers",
            "Copyright 2022 second line",
            "Nested item",
            "单元格"
        ],
    );
    // An image-only chapter has nothing to translate.
    assert!(
        block_texts_from_html(r#"<div id="Cover"><img src="ngy-asset:cover"></div>"#)
            .unwrap()
            .is_empty()
    );
    assert!(block_texts_from_html("a\0b").is_err());
}

#[test]
fn translation_sources_keep_formatting_slots_breaks_and_code_context() {
    let source = "<h2>Chapter <em>one</em></h2>\
            <p>Read <strong>very <em>carefully</em></strong>: \
            <code>call(&quot;x&quot;)</code><br>Next <sup>2</sup>\u{a0}line.</p>\
            <p><code>untranslated()</code></p>\
            <pre><code>never translate this</code></pre>";
    let blocks = translation_blocks_from_html(source).unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].text, "Chapter one");
    assert_eq!(blocks[0].segments, ["Chapter ", "one"]);
    assert_eq!(
        blocks[1].text,
        "Read very carefully: call(\"x\")Next 2 line."
    );
    assert_eq!(
        blocks[1].segments,
        [
            "Read ",
            "very ",
            "carefully",
            ": ",
            "Next ",
            "2",
            "\u{a0}line."
        ]
    );
}

#[test]
fn translation_sources_exclude_inert_descendants_and_blank_leaves() {
    let source = "<p>Alpha<script>secret()</script><style>ignored{}</style>\
            <noscript>fallback</noscript><template><p>inert</p></template>\
            <strong>Beta</strong><span>\u{feff}\u{a0}</span>Gamma</p>\
            <p>\u{feff}\u{a0}</p>";
    let blocks = translation_blocks_from_html(source).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "AlphaBeta Gamma");
    assert_eq!(blocks[0].segments, ["Alpha", "Beta", "Gamma"]);
}

#[test]
fn translation_sources_skip_leaves_that_are_only_invisible_format_characters() {
    // 现场回归：代码行用 ZWSP 缩进，ZWSP 不属于 ECMAScript `\s`，旧过滤把它当成
    // 翻译槽；模型只能回空白，整块被 `empty_segment_text` 拒绝并停在 96 号块。
    // 该块本身是代码，现在整块不再进入翻译（见 code_blocks_are_never_translated）。
    let code_line = "<p><span>\u{200b}\u{200b}</span><span>const</span>\
            <span> THREE_AND_A_BIT : f32 = 3.4028236;</span></p>";
    assert!(translation_blocks_from_html(code_line).unwrap().is_empty());
    // 正文块保留同一保护：只由不可见格式字符组成的叶子不成为翻译槽，而不可见字符
    // 仍留在匹配文本里（读者侧靠这段文本定位块级元素），所以只能排除翻译槽。
    let prose = "<p><span>\u{200b}\u{200b}</span><span>甲</span><span> 乙</span></p>";
    let blocks = translation_blocks_from_html(prose).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "\u{200b}\u{200b}甲 乙");
    assert_eq!(blocks[0].segments, ["甲", " 乙"]);
}

#[test]
fn code_blocks_are_never_translated() {
    // Code that is not marked up with pre/code is still code: Calibre/Word
    // exports and the acceptance EPUB keep every code line in its own `<p>`.
    for source in [
        "<p>const THREE_AND_A_BIT : f32 = 3.4028236;</p>",
        "<p>fn main() {</p><p>}</p>",
        "<p>let a = 1<br>return a;</p>",
        "<p>// 注释行</p>",
        "<p>#include &lt;stdio.h&gt;</p>",
        "<p>count =&gt; count + 1</p>",
        "<p>total += 1</p>",
        "<p>&lt;div class=\"code\"&gt;</p>",
        // 现场（2026-09-15，《RUST AND SCALA FOR BEGINNERS》azw3，块 3579/3588/
        // 3620/3623）：一行代码被语法高亮的 span 切成 3–5 个片段，模型要在 JSON
        // 里转义 `"\n"` 这种字面量，两次尝试后整块被跳过。
        "<p>println(result1+\"\\n\"+result2+\"\\n\"+result3)</p>",
        "<p>println(&quot;10 + 10 = &quot;+result)</p>",
        // 同一本书的块 3580：类型注解加注释头。
        "<p>def functionExample(a:Int = 0, b:Int = 0):Int = { // Parameters with default values as 0</p>",
    ] {
        assert!(
            translation_blocks_from_html(source).unwrap().is_empty(),
            "{source} must not be translated"
        );
    }
    // Prose stays translatable, including the punctuation a code line carries,
    // a quotation, a colon without a following space, a statement keyword at the
    // start of a sentence, and inline code that merely appears inside a sentence.
    let prose = "<p>见上文（注 1）。</p><p>A note (see above)</p>\
            <p>Read <code>x = 1;</code> now</p><p>https://example.test/a/b</p>\
            <p>第一章：起步</p><p>a &lt; b 的关系</p>\
            <p>He said &quot;hello&quot; to me.</p><p>let me explain the difference</p>\
            <p>Note: the file is here</p>";
    let blocks = translation_blocks_from_html(prose).unwrap();
    assert_eq!(blocks.len(), 9);
    assert_eq!(blocks[2].segments, ["Read ", " now"]);
    assert_eq!(blocks[5].segments, ["a < b 的关系"]);
    assert_eq!(blocks[6].text, "He said \"hello\" to me.");
}

#[test]
fn translation_sources_preserve_repeated_blocks_and_nested_cell_order() {
    let source = "<ul><li><p>Same <b>text</b></p></li><li>Same <b>text</b></li></ul>\
            <blockquote><p>Quote</p></blockquote>\
            <table><tr><th>Title</th><td><p>Cell <i>value</i></p></td></tr></table>";
    let blocks = translation_blocks_from_html(source).unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["Same text", "Same text", "Quote", "Title", "Cell value"]
    );
    assert_eq!(blocks[0].segments, blocks[1].segments);
    assert_eq!(blocks[4].segments, ["Cell ", "value"]);
}

#[test]
fn translation_sources_reject_invalid_or_excessively_deep_source() {
    assert!(translation_blocks_from_html("<p>bad\0source</p>").is_err());
    let source = format!(
        "<p>{}text{}</p>",
        "<span>".repeat(MAX_DOCUMENT_DEPTH + 1),
        "</span>".repeat(MAX_DOCUMENT_DEPTH + 1)
    );
    assert!(translation_blocks_from_html(&source).is_err());
}

#[test]
fn reader_document_translation_slots_follow_original_body_without_ast_rewriting() {
    let source = r#"<!doctype html><html><head><title>Metadata</title>
            <script>ignore()</script></head><body>
            <p>Read <a href="https://example.test">this</a> now</p>
            <p><span>One</span><span>two</span></p>
            <ul><li>First <em>item</em></li></ul></body></html>"#;
    let blocks = translation_blocks_from_document_html(source).unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].segments, ["Read ", "this", " now"]);
    assert_eq!(blocks[1].text, "Onetwo");
    assert_eq!(blocks[1].segments, ["One", "two"]);
    assert_eq!(blocks[2].segments, ["First ", "item"]);
}
