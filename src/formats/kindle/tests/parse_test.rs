//! Tests for header detection, resources, rewriting and chapters.

use super::super::*;
use super::*;

pub(crate) fn header(version: u32) -> Vec<u8> {
    let mut bytes = vec![0; 256];
    bytes[60..68].copy_from_slice(b"BOOKMOBI");
    bytes[76..78].copy_from_slice(&2_u16.to_be_bytes());
    bytes[78..82].copy_from_slice(&100_u32.to_be_bytes());
    bytes[86..90].copy_from_slice(&232_u32.to_be_bytes());
    bytes[100..102].copy_from_slice(&1_u16.to_be_bytes());
    bytes[104..108].copy_from_slice(&16_u32.to_be_bytes());
    bytes[108..110].copy_from_slice(&1_u16.to_be_bytes());
    bytes[110..112].copy_from_slice(&4_096_u16.to_be_bytes());
    bytes[116..120].copy_from_slice(b"MOBI");
    bytes[120..124].copy_from_slice(&116_u32.to_be_bytes());
    bytes[136..140].copy_from_slice(&version.to_be_bytes());
    bytes
}

#[test]
pub(crate) fn detects_kf8_from_the_mobi_header_not_only_the_extension() {
    let source = ImportSource {
        file_name: Some("renamed.mobi".into()),
        bytes: Arc::new(header(8)),
    };
    assert_eq!(KindleImporter.probe(&source).format, BookFormat::Azw3);
}

#[test]
pub(crate) fn rejects_declared_kindle_text_expansion_before_parser_allocation() {
    let mut bytes = header(8);
    bytes[104..108].copy_from_slice(&1_024_u32.to_be_bytes());
    let limits = ImportLimits {
        max_total_text_bytes: 1_023,
        ..ImportLimits::default()
    };
    let error = validate_mobi_header(&bytes, &limits)
        .expect_err("declared decompressed text above the limit must be rejected");
    assert!(
        error
            .to_string()
            .contains("decompressed text exceeds the total text safety limit")
    );
}

#[test]
pub(crate) fn rejects_possible_palmdoc_expansion_even_when_declared_length_is_small() {
    let mut bytes = header(8);
    bytes[100..102].copy_from_slice(&2_u16.to_be_bytes());
    let limits = ImportLimits {
        max_total_text_bytes: 119,
        ..ImportLimits::default()
    };
    let error = validate_mobi_header(&bytes, &limits)
        .expect_err("PalmDOC worst-case expansion above the limit must be rejected");
    assert!(
        error
            .to_string()
            .contains("text records can exceed the total text safety limit")
    );
}

#[test]
pub(crate) fn rejects_a_kindle_extension_without_palmdb_magic() {
    let source = ImportSource {
        file_name: Some("fake.azw3".into()),
        bytes: Arc::new(b"not a book".to_vec()),
    };
    assert_eq!(
        KindleImporter.probe(&source).confidence,
        ProbeConfidence::Extension
    );
    assert!(
        KindleImporter
            .import(&source, &ImportLimits::default())
            .is_err()
    );
}

/// Record 0 of a container whose MOBI header is followed by `tags`.
pub(crate) fn record0_with_exth(tags: &[(u32, &[u8])]) -> Vec<u8> {
    const HEADER_LEN: usize = 232;
    let mut exth = Vec::new();
    exth.extend_from_slice(b"EXTH");
    let payload_len = tags.iter().map(|(_, value)| 8 + value.len()).sum::<usize>();
    exth.extend_from_slice(&((12 + payload_len) as u32).to_be_bytes());
    exth.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    for (tag, value) in tags {
        exth.extend_from_slice(&tag.to_be_bytes());
        exth.extend_from_slice(&((8 + value.len()) as u32).to_be_bytes());
        exth.extend_from_slice(value);
    }

    let mut record0 = vec![0_u8; 16 + HEADER_LEN + exth.len()];
    record0[0..2].copy_from_slice(&1_u16.to_be_bytes());
    record0[4..8].copy_from_slice(&64_u32.to_be_bytes());
    record0[8..10].copy_from_slice(&2_u16.to_be_bytes());
    record0[10..12].copy_from_slice(&4_096_u16.to_be_bytes());
    record0[16..20].copy_from_slice(b"MOBI");
    record0[20..24].copy_from_slice(&(HEADER_LEN as u32).to_be_bytes());
    // First resource record: record 3.
    record0[108..112].copy_from_slice(&3_u32.to_be_bytes());
    record0[16 + HEADER_LEN..].copy_from_slice(&exth);
    record0
}

#[test]
pub(crate) fn a_short_exth_cover_record_is_rejected_instead_of_reading_the_next_record() {
    // Tag 201 with an empty payload (length 8) followed by another record: the
    // following record's bytes must not be read as the cover offset.
    let record0 = record0_with_exth(&[(EXTH_COVER_OFFSET_TAG, &[]), (3, &1_u32.to_be_bytes())]);
    assert_eq!(
        exth_cover_offset(&record0),
        None,
        "a 4-byte payload cannot live in a record shorter than 12 bytes"
    );
}

#[test]
pub(crate) fn resources_are_numbered_from_the_first_resource_record() {
    let record0 = record0_with_exth(&[(EXTH_COVER_OFFSET_TAG, &2_u32.to_be_bytes())]);
    let container = build_palmdb(&[
        record0,
        b"text record one".to_vec(),
        b"text record two".to_vec(),
        b"\x89PNG\r\n\x1a\nfirst".to_vec(),
        b"\xFF\xD8\xFFsecond".to_vec(),
        b"\xFF\xD8\xFFcover".to_vec(),
        b"not an image".to_vec(),
    ]);
    let palm = PalmDb::parse(&container)
        .expect("the container parses")
        .expect("the record table is readable");
    let limits = ImportLimits::default();
    let mut budget =
        AssetBudget::with_original(&limits, container.len(), "test container").unwrap();
    let resources = collect_kindle_resources(&palm, &mut budget).expect("resources are read");

    assert_eq!(resources.len(), 3, "records 3, 4 and 5 hold images");
    assert_eq!(resources[0].number, 1);
    assert_eq!(resources[0].extension, "png");
    assert!(!resources[0].is_cover);
    assert_eq!(resources[1].number, 2);
    assert_eq!(resources[1].extension, "jpg");
    assert_eq!(resources[2].number, 3);
    assert!(resources[2].is_cover, "EXTH 201 offset 2 names record 5");
}

#[test]
pub(crate) fn resources_fall_back_to_the_record_after_the_text_records() {
    let mut record0 = vec![0_u8; 300];
    record0[0..2].copy_from_slice(&1_u16.to_be_bytes());
    record0[8..10].copy_from_slice(&2_u16.to_be_bytes());
    record0[16..20].copy_from_slice(b"MOBI");
    record0[20..24].copy_from_slice(&264_u32.to_be_bytes());
    record0[108..112].copy_from_slice(&u32::MAX.to_be_bytes());
    let container = build_palmdb(&[
        record0,
        b"text one".to_vec(),
        b"text two".to_vec(),
        b"\xFF\xD8\xFFpicture".to_vec(),
    ]);
    let palm = PalmDb::parse(&container)
        .expect("the container parses")
        .expect("the record table is readable");
    let limits = ImportLimits::default();
    let mut budget =
        AssetBudget::with_original(&limits, container.len(), "test container").unwrap();
    let resources = collect_kindle_resources(&palm, &mut budget).expect("resources are read");

    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].number, 1);
}

#[test]
pub(crate) fn kindle_embed_references_use_the_base32_resource_number() {
    let mut paths = HashMap::new();
    paths.insert(1_usize, "kindle-resource/0001.jpg".to_string());
    paths.insert(10_usize, "kindle-resource/0010.jpg".to_string());
    paths.insert(57_usize, "kindle-resource/0057.jpg".to_string());
    let html = concat!(
        r#"<img src="kindle:embed:0001?mime=image/jpg"/>"#,
        r#"<img src="kindle:embed:000A?mime=image/jpg"/>"#,
        r#"<img src="kindle:embed:001P?mime=image/jpg"/>"#,
        r#"<img src="kindle:embed:000B?mime=image/jpg"/>"#,
    );
    assert_eq!(
        rewrite_kindle_media_references(html, &paths),
        concat!(
            r#"<img src="kindle-resource/0001.jpg"/>"#,
            r#"<img src="kindle-resource/0010.jpg"/>"#,
            r#"<img src="kindle-resource/0057.jpg"/>"#,
            // 000B is not a retained resource: the reference is left alone
            // so the shared rewrite pass still drops the attribute.
            r#"<img src="kindle:embed:000B?mime=image/jpg"/>"#,
        )
    );
}

#[test]
pub(crate) fn legacy_ebook_rs_image_paths_map_back_to_the_resource_number() {
    let mut paths = HashMap::new();
    paths.insert(10_usize, "kindle-resource/0010.jpg".to_string());
    assert_eq!(
        rewrite_kindle_media_references(
            r#"<img src="images/img_0010.jpg?mime=image/jpg"/>"#,
            &paths
        ),
        r#"<img src="kindle-resource/0010.jpg"/>"#
    );
    assert_eq!(
        rewrite_kindle_media_references(r#"<img src="images/img_0099.jpg"/>"#, &paths),
        r#"<img src="images/img_0099.jpg"/>"#
    );
}

pub(crate) fn section(href: &str, html: &str) -> ebook_rs::Section {
    ebook_rs::Section {
        index: 0,
        idref: href.to_string(),
        href: href.to_string(),
        full_path: href.to_string(),
        raw_html: html.to_string(),
        processed_html: html.to_string(),
        plain_text: String::new(),
        plain_text_lower: String::new(),
        char_count: 0,
        viewport_width: None,
        viewport_height: None,
    }
}

#[test]
pub(crate) fn chapters_split_only_on_level_one_headings_that_carry_text() {
    let sections = vec![
        section(
            "section_0.html",
            "<html><body><p>front matter</p></body></html>",
        ),
        // A converted document puts its `_Toc…` bookmark in an empty `<h1>`.
        section("section_1.html", r#"<h1><a name="_Toc1"></a></h1>"#),
        section(
            "section_2.html",
            r#"<h1>Rust Programming Language</h1><h2>What is Rust?</h2>"#,
        ),
        section("section_3.html", "<h2>Installation</h2>"),
        section("section_4.html", "<h1>Loops</h1><p>body</p>"),
    ];
    let chapters = kindle_chapters(&sections);

    assert_eq!(chapters.len(), 3, "an empty h1 must not start a chapter");
    assert_eq!(chapters[0].title, "", "the front matter opens no chapter");
    assert_eq!(chapters[0].href, "section_0.html");
    assert!(chapters[0].html.contains("front matter"));
    assert!(
        chapters[0].html.contains(r#"<a name="_Toc1">"#),
        "the bookmark-only section stays with the front matter"
    );
    assert_eq!(chapters[1].title, "Rust Programming Language");
    assert!(chapters[1].html.contains("What is Rust?"));
    assert!(
        chapters[1].html.contains("Installation"),
        "h2 sections stay inside the chapter that owns them"
    );
    assert_eq!(chapters[2].title, "Loops");
}

#[test]
pub(crate) fn heading_titles_decode_entities_and_ignore_empty_headings() {
    assert_eq!(
        leading_heading(r#"<h1>&quot;if-else&quot;</h1>"#, "h1").as_deref(),
        Some("\"if-else\"")
    );
    assert_eq!(
        leading_heading("<h1>  A\n B </h1>", "h1").as_deref(),
        Some("A B")
    );
    assert_eq!(
        leading_heading(r#"<h1><a name="_Toc"></a></h1>"#, "h1"),
        None
    );
    assert_eq!(leading_heading("<h2>Not a chapter</h2>", "h1"), None);
}

#[test]
pub(crate) fn orphan_stylesheet_text_is_dropped() {
    let html = "<p>tail</p> \r\n\t\t\t@page Section1 { size:612pt 792pt; margin:72pt }\r\n\t\t\tdiv.Section1 { page:Section1 }\r\n\t\t";
    assert_eq!(strip_orphan_css(html), "<p>tail</p>");

    let prose = "<p>a &lt; b</p><p>done</p>";
    assert_eq!(strip_orphan_css(prose), prose);
}

#[test]
pub(crate) fn at_rule_inside_a_tag_is_body_copy() {
    // A CSS tutorial can put an at-rule in its own paragraph; the paragraph
    // is content, not a stylesheet flow.
    let paragraph = "<p>@media (max-width: 600px) { body { color: red } }</p>";
    assert_eq!(strip_orphan_css(paragraph), paragraph);

    let tutorial = "<h2>Media queries</h2><p>@page Section1 { size: 612pt }</p><p>done</p>";
    assert_eq!(strip_orphan_css(tutorial), tutorial);
}

#[test]
pub(crate) fn orphan_stylesheet_text_outside_tags_is_still_dropped() {
    let html = "<p>tail</p>@page Section1 { size:612pt }<p>head</p>";
    assert_eq!(strip_orphan_css(html), "<p>tail</p><p>head</p>");
}

#[test]
pub(crate) fn an_unclosed_ampersand_before_cjk_text_does_not_split_a_character() {
    // `&` 后面跟够长的中文：按字节长度截断扫描窗口时，第 34 字节正好落在「中」
    // 的中间。它没有配对的 `;`，应当原样保留，而不是让切片 panic 掉整次导入。
    let text = format!("&{}中", "a".repeat(32));
    assert_eq!(decode_entities(&text), text);
}

#[test]
pub(crate) fn chapter_headings_with_a_bare_ampersand_survive() {
    // 真实章节标题的形状：ASCII 书名 + `&` + 中文。扫描窗口的字节边界落在中文
    // 字符内部，导入必须照常拿到标题。
    let heading = "Tom & Jerry 汤姆和杰瑞历险记续篇";
    let html = format!("<h1>{heading}</h1>");
    assert_eq!(leading_heading(&html, "h1").as_deref(), Some(heading));
}
