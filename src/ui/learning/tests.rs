//! Tests for the learning window.

use super::*;
use gpui_kit::component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};

#[test]
fn course_markdown_keeps_web_links_and_code_examples_without_opening_local_paths() {
    let markdown = "[基础诊断](lessons/01-prerequisites.md) 和 [**评分**](worksheets/rubric.md)\n\n[官网](https://example.org/guide)\n\n`[行内样例](example.md)`\n\n```python\nprint('[代码样例](example.md)')\n```\n\n[参考资料][ref]\n\n[ref]: references.md\n";
    let display = course_display_markdown(markdown);
    assert!(display.starts_with("基础诊断 和 评分\n"));
    assert!(display.contains("[官网](https://example.org/guide)"));
    assert!(display.contains("`[行内样例](example.md)`"));
    assert!(display.contains("print('[代码样例](example.md)')"));
    assert!(display.contains("\n参考资料\n"));
    let links: Vec<_> = pulldown_cmark::Parser::new(&display)
        .filter_map(|event| match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                Some(dest_url.into_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(links, ["https://example.org/guide"]);
}

#[test]
fn guide_and_its_supplementary_documents_use_the_course_source_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("courses/agent-foundations");
    for document in CourseDocument::ALL {
        assert_eq!(
            document.markdown(),
            std::fs::read_to_string(root.join(document.path())).unwrap()
        );
    }
    // All local reading links must resolve to bundled documents or one of
    // the original six lessons, not a file inaccessible from the desktop.
    let mut destinations = CourseDocument::ALL
        .iter()
        .map(|document| root.join(document.path()).canonicalize().unwrap())
        .collect::<std::collections::HashSet<_>>();
    for filename in [
        "01-prerequisites.md",
        "02-observe-loop.md",
        "03-build-manual.md",
        "04-recover-failures.md",
        "05-build-langgraph.md",
        "06-transfer-and-review.md",
    ] {
        destinations.insert(root.join("lessons").join(filename).canonicalize().unwrap());
    }
    for (index, document) in CourseDocument::CHAPTERS.iter().enumerate().skip(1) {
        let chapter = root.join(document.path());
        let exercise = chapter
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("exercises")
            .join(chapter.file_name().unwrap())
            .canonicalize()
            .unwrap();
        let lessons = ngy_book_studio::learning_records::lessons_for((index + 1) as u8).unwrap();
        assert_eq!(lessons.len(), 1);
        assert_eq!(
            lessons[0].markdown,
            std::fs::read_to_string(&exercise).unwrap()
        );
        destinations.insert(exercise);
    }
    for document in CourseDocument::ALL {
        let path = root.join(document.path());
        for event in pulldown_cmark::Parser::new(document.markdown()) {
            if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) = event
            {
                let destination = dest_url.split('#').next().unwrap();
                if destination.starts_with("https://")
                    || destination.starts_with("http://")
                    || !destination.ends_with(".md")
                {
                    continue;
                }
                let resolved = path
                    .parent()
                    .unwrap()
                    .join(destination)
                    .canonicalize()
                    .unwrap_or_else(|error| {
                        panic!("{} -> {destination}: {error}", document.path())
                    });
                assert!(
                    destinations.contains(&resolved),
                    "missing desktop destination: {} -> {destination}",
                    document.path()
                );
            }
        }
    }
    assert_eq!(
        StudyPane::default(),
        StudyPane::Document(CourseDocument::Tutorial)
    );
}

#[test]
fn tutorial_chapters_keep_the_original_experiment_steps_separate() {
    let chapters = CourseDocument::CHAPTERS;
    assert_eq!(chapters.len(), 10);
    assert_eq!(chapters[0], CourseDocument::Overview);
    assert_eq!(chapters[9], CourseDocument::Capstone);
    for (index, chapter) in chapters.iter().enumerate() {
        assert_eq!(chapter.chapter_index(), Some(index));
        assert_eq!(chapters.iter().filter(|item| *item == chapter).count(), 1);
    }
    for material in CourseDocument::MATERIALS {
        assert_eq!(material.chapter_index(), None);
    }
    // Browsing the tutorial does not turn chapter numbers into persisted
    // first-chapter lesson indices or invalidate existing submissions.
    let original = LearningWorkspace {
        lesson_index: 5,
        ..Default::default()
    };
    assert!(original.validate().is_ok());
    assert!(
        LearningWorkspace {
            lesson_index: 6,
            ..original
        }
        .validate()
        .is_err()
    );
}

#[test]
fn python_highlighting_colors_python_tokens_in_both_themes() {
    register_python_highlighting();
    let config = LanguageRegistry::singleton()
        .language(CODE_LANGUAGE)
        .unwrap();
    assert_eq!(config.name.as_ref(), "python");
    let code = "# 读取资料\ndef research():\n    return '松果项目', 42\n";
    let mut highlighter = SyntaxHighlighter::new(CODE_LANGUAGE);
    highlighter.update(None, &code.into(), None);
    for theme in [
        HighlightTheme::default_light(),
        HighlightTheme::default_dark(),
    ] {
        let styles = highlighter.styles(&(0..code.len()), &*theme);
        for (token, capture) in [
            ("# 读取资料", "comment"),
            ("def", "keyword"),
            ("research", "function"),
            ("return", "keyword"),
            ("松果项目", "string"),
            ("42", "number"),
        ] {
            let start = code.find(token).unwrap();
            let expected = theme.style.syntax.style(capture).unwrap().color;
            assert!(expected.is_some());
            assert!(
                styles.iter().any(|(range, style)| range.start <= start
                    && range.end >= start + token.len()
                    && style.color == expected),
                "missing {capture} color for {token}: {styles:?}"
            );
        }
    }
}

#[test]
fn python_highlighting_reclassifies_code_after_editing_to_a_comment() {
    register_python_highlighting();
    let mut highlighter = SyntaxHighlighter::new(CODE_LANGUAGE);
    let theme = HighlightTheme::default_light();
    let original = "return '正文'\n";
    highlighter.update(None, &original.into(), None);
    let edited = "# return '正文'\n";
    highlighter.update(None, &edited.into(), None);
    let styles = highlighter.styles(&(0..edited.len()), &*theme);
    assert!(styles.iter().any(|(range, style)| range.start == 0
        && range.end >= edited.trim_end().len()
        && style.color == theme.style.syntax.style("comment").unwrap().color));
    assert!(
        !styles
            .iter()
            .any(|(_, style)| style.color == theme.style.syntax.style("keyword").unwrap().color)
    );
}

#[test]
fn close_waits_for_execution_and_persistence_before_removal() {
    assert_eq!(close_action(true, false, true), CloseAction::Wait);
    assert_eq!(close_action(false, true, false), CloseAction::Wait);
    assert_eq!(close_action(false, false, true), CloseAction::Save);
    assert_eq!(close_action(false, false, false), CloseAction::Remove);
}

#[test]
fn imported_pass_is_always_presented_as_unverified() {
    assert_eq!(run_label("completed", true, true), "导入记录 · 待核验");
    assert_eq!(run_label("completed", true, false), "本次运行检查通过");
    assert_eq!(run_label("cancelled", false, false), "本次运行已取消");
}

#[test]
fn restored_future_or_invalid_times_are_not_claimed_as_recent() {
    assert_eq!(relative_record_time("120", 100), "记录时间待核验");
    assert_eq!(relative_record_time("not-a-date", 100), "记录时间待核验");
    assert_eq!(relative_record_time("99", 100), "刚刚");
    assert_eq!(relative_record_time("1", 121), "2 分钟前");
}

#[test]
fn output_cannot_escape_its_markdown_code_fence() {
    let output = "工具输出\n```\n![image](https://example.invalid/image)\n```";
    let fenced = fenced_text(output, "text");
    assert!(fenced.starts_with("````text\n"));
    assert!(fenced.ends_with("\n````"));
    assert!(fenced.contains(output));
    assert_eq!(short_text("资料读取完成", 4), "资料读取…");
}
