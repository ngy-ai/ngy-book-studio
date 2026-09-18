//! Tests for reader resources and appearance.

use super::*;
#[test]
fn rejects_encoded_parent_traversal() {
    assert!(safe_resource_path("/EPUB/%2e%2e/META-INF/container.xml").is_err());
    assert!(safe_resource_path("/EPUB/chapter.xhtml").is_ok());
}

#[test]
fn builds_custom_protocol_url() {
    assert_eq!(
        "epubreader://book/EPUB/chapter.xhtml#part-1",
        OpenedBook::url_for_href("/EPUB/chapter.xhtml#part-1")
    );
}

#[test]
fn builds_platform_navigation_url() {
    let url = OpenedBook::navigation_url_for_href("/EPUB/chapter.xhtml#part-1");
    #[cfg(target_os = "windows")]
    assert_eq!("http://epubreader.book/EPUB/chapter.xhtml#part-1", url);
    #[cfg(not(target_os = "windows"))]
    assert_eq!("epubreader://book/EPUB/chapter.xhtml#part-1", url);
}

#[test]
fn rejects_unsafe_mime_header_values() {
    assert_eq!(
        normalize_mime("Application/XHTML+XML; charset=UTF-8").as_deref(),
        Some("application/xhtml+xml")
    );
    assert_eq!(normalize_mime("text/html\r\nx-injected: true"), None);
}

#[test]
fn recognizes_only_internal_reader_urls() {
    assert_eq!(
        reader_url_path("http://epubreader.book/EPUB/chapter.xhtml#part"),
        Some("/EPUB/chapter.xhtml")
    );
    assert_eq!(
        reader_url_path("epubreader://book/EPUB/chapter.xhtml"),
        Some("/EPUB/chapter.xhtml")
    );
    assert_eq!(reader_url_path("https://epubreader.evil/chapter"), None);
}

/// The served stylesheet carries the text size a chapter renders at before
/// the reader changes it. The runtime clamps the same range in
/// `ui::reader::READER_INITIALIZATION_SCRIPT`, which pins it too.
#[test]
fn the_served_chapter_stylesheet_defaults_to_the_host_text_size() {
    assert!(
        READER_CSS.contains(&format!("--ngy-font-size: {READER_FONT_SIZE_DEFAULT}px")),
        "the served chapter stylesheet must default to the host's text size"
    );
}

#[test]
fn reading_text_size_steps_within_the_host_range() {
    assert_eq!(reader_font_size(0), READER_FONT_SIZE_MIN);
    assert_eq!(reader_font_size(200), READER_FONT_SIZE_MAX);
    assert_eq!(
        reader_font_size(READER_FONT_SIZE_DEFAULT),
        READER_FONT_SIZE_DEFAULT
    );

    // The applied script always carries the clamped size, whoever asks.
    assert!(
        reader_appearance_script(0, Appearance::Light)
            .contains(&format!("{READER_FONT_SIZE_MIN}px")),
        "a stored size below the range must not reach the chapter"
    );
    assert!(
        reader_appearance_script(u8::MAX, Appearance::Light)
            .contains(&format!("{READER_FONT_SIZE_MAX}px"))
    );
}

/// 皮肤也必须跟着字号一起下发：只改字号会让深色皮肤下的正文留在浅色样式表上，
/// 而只改皮肤会让下一次改字号把皮肤覆盖回兜底值。
#[test]
fn the_appearance_script_carries_the_skin_with_the_text_size() {
    let light = reader_appearance_script(18, Appearance::Light);
    let dark = reader_appearance_script(18, Appearance::Dark);
    assert_ne!(light, dark);
    for script in [&light, &dark] {
        assert!(script.contains("--ngy-font-size"));
        assert!(script.contains("--ngy-paper"));
        assert!(script.contains("--ngy-text"));
        assert!(script.contains("colorScheme"));
    }
    assert!(light.contains(&format!(
        "--ngy-paper', '{}'",
        Palette::hex(Palette::LIGHT.paper)
    )));
    assert!(dark.contains(&format!(
        "--ngy-paper', '{}'",
        Palette::hex(Palette::DARK.paper)
    )));
    assert!(light.contains("'light'"));
    assert!(dark.contains("'dark'"));
}
