//! Strict XML checks on the actual editor projection, custom-protocol response,
//! and rich-text snapshot persistence. These tests do not execute ProseMirror
//! or replace acceptance in a real Windows WebView.

use super::*;
use gpui_component::wry::http::Request;
use moye_epub_editor::{
    document::{ContentUnit, SourceLocator, TocTarget},
    library::ImportOutcome,
};
use resvg::usvg::roxmltree;
use std::{collections::BTreeSet, path::Path};

const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const CHAPTER_TITLE: &str = "图文 & 音视频 <第一章>";
const UNIT_ID: &str = "unit-xhtml";

fn media_document(book_id: &str) -> (BookDocument, HashMap<String, Arc<Vec<u8>>>) {
    let mut assets = Vec::new();
    let mut asset_bytes = HashMap::new();
    for (role, mime, bytes) in [
        (
            AssetRole::ContentImage,
            "image/png",
            b"fixture-png".as_slice(),
        ),
        (AssetRole::Audio, "audio/ogg", b"OggS-fixture".as_slice()),
        (AssetRole::Video, "video/mp4", b"fixture-mp4".as_slice()),
    ] {
        let asset = AssetRef::from_bytes(role, mime, None, bytes);
        asset_bytes.insert(asset.id.clone(), Arc::new(bytes.to_vec()));
        assets.push(asset);
    }
    // Intentionally ordinary HTML: the original source has void elements and
    // bare Boolean attributes; RawHtml also carries a named HTML entity.
    let source = format!(
        "<p>正文 &amp; 特殊字符 &lt;保留&gt;<br>第二行</p><hr>\
         <figure><img src=\"moye-asset:{}\" alt=\"图像 &amp; &lt;说明&gt;\"></figure>\
         <audio controls src=\"moye-asset:{}\" title=\"音频标题\"></audio>\
         <video controls src=\"moye-asset:{}\" poster=\"moye-asset:{}\" title=\"视频标题\"></video>\
         <div class=\"original-fragment\">保留&nbsp;空格<br>原始片段</div>",
        assets[0].id, assets[1].id, assets[2].id, assets[0].id,
    );
    let parsed = parse_source_for_unit(SourceKind::Html, &source, UNIT_ID).unwrap();
    assert!(
        parsed
            .document
            .blocks
            .iter()
            .any(|block| matches!(block, Block::RawHtml { .. }))
    );
    let mut document = BookDocument::created(book_id, "XHTML 回归");
    document.units.push(
        ContentUnit::new(
            UNIT_ID,
            ContentUnitKind::Chapter,
            CHAPTER_TITLE,
            SourceKind::Html,
            source,
            parsed.document,
        )
        .with_source_locator(SourceLocator::created()),
    );
    document.toc.push(TocNode::new(
        "toc-xhtml",
        CHAPTER_TITLE,
        TocTarget::unit(UNIT_ID),
    ));
    document.assets = assets;
    document.validate().unwrap();
    (document, asset_bytes)
}

fn strict_xhtml<'a>(source: &'a str, expected_title: &str) -> roxmltree::Document<'a> {
    let xml = roxmltree::Document::parse(source)
        .unwrap_or_else(|error| panic!("editor response must be strict XHTML: {error}"));
    assert_eq!(xml.root_element().tag_name().name(), "html");
    assert_eq!(
        xml.root_element().tag_name().namespace(),
        Some(XHTML_NAMESPACE)
    );
    let title = xml
        .descendants()
        .find(|node| node.has_tag_name((XHTML_NAMESPACE, "title")))
        .unwrap();
    assert_eq!(title.text(), Some(expected_title));
    assert_eq!(
        xml.descendants()
            .filter(|node| node.has_tag_name((XHTML_NAMESPACE, "body")))
            .count(),
        1
    );
    xml
}

fn assert_media_xhtml(source: &str) {
    let xml = strict_xhtml(source, CHAPTER_TITLE);
    for tag in ["img", "br", "hr", "audio", "video"] {
        assert!(
            xml.descendants()
                .any(|node| node.has_tag_name((XHTML_NAMESPACE, tag))),
            "missing {tag}"
        );
    }
    for tag in ["audio", "video"] {
        let node = xml
            .descendants()
            .find(|node| node.has_tag_name((XHTML_NAMESPACE, tag)))
            .unwrap();
        assert!(node.attribute("controls").is_some(), "{tag} lost controls");
    }
    assert!(
        xml.descendants()
            .filter_map(|node| node.text())
            .any(|text| text.contains("保留\u{a0}空格"))
    );
}

fn source_request(
    state: &EditorWebState,
    unit_id: &str,
    href: &str,
    origin: Option<&str>,
) -> Request<()> {
    let url = format!(
        "epubeditor://content/{href}?mode=source&session_id={}&chapter_id={}&rev=0",
        urlencoding::encode(state.session_id()),
        urlencoding::encode(unit_id),
    );
    let mut request = Request::builder().uri(url);
    if let Some(origin) = origin {
        request = request.header("Origin", origin);
    }
    request.body(()).unwrap()
}

fn source_state(document: &BookDocument, chapter: &EditorChapter, unit_id: &str) -> EditorWebState {
    let state = EditorWebState::new(
        document.id.clone(),
        chapter.href.clone(),
        chapter.html.clone(),
    );
    state.set(
        0,
        unit_id.to_string(),
        chapter.href.clone(),
        chapter.html.clone(),
    );
    state.authorize_media(document).unwrap();
    state
}

fn rich_body(source: &str) -> String {
    let xml = roxmltree::Document::parse(source).unwrap();
    let body = xml
        .descendants()
        .find(|node| node.has_tag_name((XHTML_NAMESPACE, "body")))
        .unwrap();
    // The outer article belongs to the display shell. ProseMirror serializes
    // its editable children into a fresh body, without this shell wrapper.
    let content = body
        .children()
        .find(|node| node.has_tag_name((XHTML_NAMESPACE, "article")))
        .unwrap_or(body);
    let inner = content
        .children()
        .map(|node| &source[node.range()])
        .collect::<String>();
    format!("<body xmlns=\"{XHTML_NAMESPACE}\">{inner}</body>")
}

fn apply_rich_body(
    state: &EditorWebState,
    unit_id: &str,
    href: &str,
    body: String,
) -> EditorIpcUpdate {
    state
        .apply_message(EditorIpcMessage {
            session_id: state.session_id().to_string(),
            chapter_id: unit_id.to_string(),
            href: href.to_string(),
            revision: 0,
            request_id: Some(1),
            body: Some(body),
            selected_text: String::new(),
            too_large: false,
            ready: false,
        })
        .expect("matching rich-text IPC snapshot must be accepted")
}

fn referenced_ids(document: &BookDocument) -> BTreeSet<String> {
    document
        .units
        .iter()
        .flat_map(|unit| unit.document.referenced_asset_ids())
        .map(str::to_string)
        .collect()
}

#[test]
fn canonical_media_projection_is_strict_xhtml_and_preserves_original_html_source() {
    let (document, _) = media_document("book-xhtml");
    let original = document.clone();
    let legacy = serialize_source(&document.units[0].document, SourceKind::Html).unwrap();
    assert!(
        roxmltree::Document::parse(&editor_document_shell(CHAPTER_TITLE, &legacy)).is_err(),
        "fixture must exercise the ordinary HTML/XML boundary"
    );
    let chapters = editor_chapters_from_document(&document).unwrap();
    assert_eq!(chapters.len(), 1);
    assert_media_xhtml(&chapters[0].html);
    for asset in &document.assets {
        assert!(
            chapters[0]
                .html
                .contains(&format!("moye-asset:{}", asset.id))
        );
    }
    assert_eq!(document, original);
    assert_eq!(document.units[0].source_kind, SourceKind::Html);
}

#[test]
fn ordinary_html_source_preview_and_rich_source_response_are_strict_and_origin_scoped() {
    let (document, _) = media_document("book-xhtml");
    let chapters = editor_chapters_from_document(&document).unwrap();
    let unit = &document.units[0];
    let (preview, canonical_source, blocks) = preview_document_from_source(
        &chapters[0].html,
        &unit.title,
        &unit.id,
        unit.source_kind,
        &unit.source,
    )
    .unwrap();
    assert_media_xhtml(&preview);
    assert_eq!(
        canonical_source,
        serialize_source(&blocks, SourceKind::Html).unwrap()
    );
    assert_eq!(
        blocks
            .referenced_asset_ids()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        unit.document.referenced_asset_ids().into_iter().collect()
    );
    let state = source_state(
        &document,
        &EditorChapter {
            html: preview,
            ..chapters[0].clone()
        },
        &unit.id,
    );
    let response = editor_protocol_response(
        &state,
        &source_request(
            &state,
            &unit.id,
            &chapters[0].href,
            Some(EDITOR_SHELL_ORIGIN),
        ),
    );
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/xhtml+xml; charset=utf-8"
    );
    assert_eq!(
        response.headers()["content-security-policy"],
        EDITOR_CONTENT_CSP
    );
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        EDITOR_SHELL_ORIGIN
    );
    assert_eq!(response.headers()["vary"], "Origin");
    assert_media_xhtml(std::str::from_utf8(response.body()).unwrap());
    for origin in [
        None,
        Some("http://epubeditor.content"),
        Some("https://untrusted.invalid"),
    ] {
        assert_eq!(
            editor_protocol_response(
                &state,
                &source_request(&state, &unit.id, &chapters[0].href, origin)
            )
            .status(),
            403
        );
    }
    let shell_request = Request::builder()
        .uri(editor_custom_url(
            EditorTab::RichText,
            state.session_id(),
            &unit.id,
            0,
            &chapters[0].href,
        ))
        .body(())
        .unwrap();
    let shell = editor_protocol_response(&state, &shell_request);
    assert_eq!(shell.status(), 200);
    assert_eq!(shell.headers()["content-security-policy"], EDITOR_SHELL_CSP);
    assert!(shell.headers().get("access-control-allow-origin").is_none());
    assert_eq!(shell.body().as_ref(), EDITOR_TRUSTED_SHELL.as_bytes());
    roxmltree::Document::parse(std::str::from_utf8(shell.body()).unwrap()).unwrap();
}

#[test]
fn rich_text_ipc_save_and_reopen_preserve_html_kind_content_and_media_ids() {
    let directory = tempfile::tempdir().unwrap();
    let mut library = LibraryStore::load_from(directory.path().to_path_buf()).unwrap();
    let record = library.create_book("XHTML 持久化", "测试").unwrap();
    let (mut document, bytes) = media_document(&record.id);
    document.revision = library.document(&record.id).unwrap().revision;
    library.apply_document_with_assets(document, bytes).unwrap();
    let original = library.document(&record.id).unwrap();
    let chapters = editor_chapters_from_document(&original).unwrap();
    let state = source_state(&original, &chapters[0], UNIT_ID);
    let response = editor_protocol_response(
        &state,
        &source_request(
            &state,
            UNIT_ID,
            &chapters[0].href,
            Some(EDITOR_SHELL_ORIGIN),
        ),
    );
    assert_eq!(response.status(), 200);
    let served = std::str::from_utf8(response.body()).unwrap();
    assert_media_xhtml(served);
    let body = rich_body(served).replace("</body>", "<p>富文本新增正文 &amp; 保留</p></body>");
    let update = apply_rich_body(&state, UNIT_ID, &chapters[0].href, body);
    assert!(update.edited);
    assert_media_xhtml(&update.html);
    assert!(!update.html.contains("http://epubeditor.content"));
    let parsed = parse_rich_text_snapshot(&update.html, UNIT_ID).unwrap();
    let mut changed = original.clone();
    changed.units[0].source_kind = SourceKind::Html;
    changed.units[0].source = parsed.canonical_source;
    changed.units[0].document = parsed.document;
    assert_eq!(referenced_ids(&changed), referenced_ids(&original));
    assert_eq!(changed.assets, original.assets);
    library.apply_document(changed).unwrap();
    drop(library);
    let reopened = LibraryStore::load_from(directory.path().to_path_buf()).unwrap();
    let saved = reopened.document(&record.id).unwrap();
    assert_eq!(saved.units[0].source_kind, SourceKind::Html);
    assert_eq!(referenced_ids(&saved), referenced_ids(&original));
    assert_eq!(saved.assets, original.assets);
    assert_eq!(saved.toc, original.toc);
    let text = saved.units[0].plain_text();
    for expected in [
        "正文 & 特殊字符 <保留>",
        "第二行",
        "音频标题",
        "视频标题",
        "原始片段",
        "富文本新增正文 & 保留",
    ] {
        assert!(
            text.contains(expected),
            "missing persisted text: {expected}"
        );
    }
    assert!(
        !text.contains(CHAPTER_TITLE),
        "shell title must not become editable body text"
    );
    let reopened_chapters = editor_chapters_from_document(&saved).unwrap();
    assert_media_xhtml(&reopened_chapters[0].html);
}

fn audit_write(directory: &Path, index: usize, stage: &str, xhtml: &str, title: &str) {
    strict_xhtml(xhtml, title);
    std::fs::write(
        directory.join(format!("unit-{:02}-{stage}.xhtml", index + 1)),
        xhtml,
    )
    .unwrap();
}

#[test]
#[ignore = "requires MOYE_XHTML_SAMPLE and an empty, isolated MOYE_XHTML_AUDIT_DIR"]
fn external_sample_editor_xhtml_audit() {
    let sample = std::env::var_os("MOYE_XHTML_SAMPLE")
        .expect("set MOYE_XHTML_SAMPLE to an authorized EPUB sample");
    let directory = PathBuf::from(
        std::env::var_os("MOYE_XHTML_AUDIT_DIR")
            .expect("set MOYE_XHTML_AUDIT_DIR to a new isolated output directory"),
    );
    assert!(
        directory.is_absolute(),
        "the isolated audit output must be absolute"
    );
    if directory.exists() {
        assert!(
            std::fs::read_dir(&directory).unwrap().next().is_none(),
            "refusing to overwrite a nonempty audit directory"
        );
    }
    std::fs::create_dir_all(&directory).unwrap();
    let mut library = LibraryStore::load_from(directory.join("data")).unwrap();
    let record = match library.import(Path::new(&sample)).unwrap() {
        ImportOutcome::Added(record) => record,
        ImportOutcome::AlreadyExists(_) => {
            panic!("fresh isolated audit library must not contain the sample")
        }
    };
    let original = library.document(&record.id).unwrap();
    assert!(!original.units.is_empty());
    let chapters = editor_chapters_from_document(&original).unwrap();
    assert_eq!(chapters.len(), original.units.len());
    let mut normalized = original.clone();
    for (index, (unit, chapter)) in original.units.iter().zip(&chapters).enumerate() {
        audit_write(&directory, index, "projection", &chapter.html, &unit.title);
        let (preview, _, _) = preview_document_from_source(
            &chapter.html,
            &unit.title,
            &unit.id,
            unit.source_kind,
            &unit.source,
        )
        .unwrap();
        audit_write(&directory, index, "source-preview", &preview, &unit.title);
        let state = source_state(&original, chapter, &unit.id);
        let response = editor_protocol_response(
            &state,
            &source_request(&state, &unit.id, &chapter.href, Some(EDITOR_SHELL_ORIGIN)),
        );
        assert_eq!(response.status(), 200);
        let source = std::str::from_utf8(response.body()).unwrap();
        audit_write(&directory, index, "rich-source", source, &unit.title);
        let update = apply_rich_body(&state, &unit.id, &chapter.href, rich_body(source));
        audit_write(
            &directory,
            index,
            "rich-snapshot",
            &update.html,
            &unit.title,
        );
        let parsed = parse_rich_text_snapshot(&update.html, &unit.id).unwrap();
        normalized.units[index].source_kind = SourceKind::Html;
        normalized.units[index].source = parsed.canonical_source;
        normalized.units[index].document = parsed.document;
    }
    assert_eq!(referenced_ids(&normalized), referenced_ids(&original));
    assert_eq!(normalized.toc, original.toc);
    library.apply_document(normalized).unwrap();
    drop(library);
    let reopened = LibraryStore::load_from(directory.join("data")).unwrap();
    let saved = reopened.document(&record.id).unwrap();
    assert_eq!(saved.units.len(), original.units.len());
    assert_eq!(referenced_ids(&saved), referenced_ids(&original));
    assert_eq!(saved.toc, original.toc);
    for (index, (unit, chapter)) in saved
        .units
        .iter()
        .zip(editor_chapters_from_document(&saved).unwrap())
        .enumerate()
    {
        audit_write(&directory, index, "reopened", &chapter.html, &unit.title);
    }
    std::fs::write(directory.join("audit.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "book_id": saved.id,
        "units": saved.units.len(),
        "referenced_media": referenced_ids(&saved).len(),
        "original_revision": original.revision.get(),
        "saved_revision": saved.revision.get(),
        "strict_xhtml_stages": ["projection", "source-preview", "rich-source", "rich-snapshot", "reopened"],
        "gui_acceptance": false
    })).unwrap()).unwrap();
    eprintln!(
        "Audited {} units; isolated GUI data: {}",
        saved.units.len(),
        directory.join("data").display()
    );
}
