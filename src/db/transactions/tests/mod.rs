//! Shared fixtures for the transaction tests.

pub(super) use super::*;
pub(super) use crate::db::{
    book_search,
    chat_threads::ChatThread,
    connection,
    index_jobs::IndexJobStatus,
    settings::Setting,
    translations::{self, NewTranslation},
};

#[derive(Clone)]
struct Fixture {
    book: BookRecord,
    source_blob: BlobRecord,
    additional_blobs: Vec<BlobRecord>,
    source: BookSource,
    progress: ReadingProgress,
    units: Vec<ContentUnit>,
    toc: Vec<TocEntry>,
    assets: Vec<Asset>,
    refs: Vec<AssetRef>,
    chunks: Vec<SearchChunk>,
    pages: Vec<VisualPage>,
}

impl Fixture {
    fn new() -> Self {
        let source_blob = BlobRecord {
            object_key: "objects/source-1".to_string(),
            media_type: "application/epub+zip".to_string(),
            byte_len: 101,
            hash: "hash-source-1".to_string(),
            created_at: 1,
        };
        let cover_blob = BlobRecord {
            object_key: "objects/cover-1".to_string(),
            media_type: "image/png".to_string(),
            byte_len: 23,
            hash: "hash-cover-1".to_string(),
            created_at: 1,
        };
        let page_blob = BlobRecord {
            object_key: "objects/page-1".to_string(),
            media_type: "image/webp".to_string(),
            byte_len: 44,
            hash: "hash-page-1".to_string(),
            created_at: 1,
        };
        Self {
            book: BookRecord {
                id: "book-1".to_string(),
                title: "统一文档".to_string(),
                author: "作者".to_string(),
                language: Some("zh-CN".to_string()),
                description: Some("测试文档".to_string()),
                format: "epub".to_string(),
                revision: 1,
                source_object_key: source_blob.object_key.clone(),
                cover_asset_id: Some("asset-cover".to_string()),
                cover_object_key: Some(cover_blob.object_key.clone()),
                cover_mime: Some(cover_blob.media_type.clone()),
                added_at: 1,
                updated_at: 1,
                last_spine: 2,
                group_id: None,
            },
            source: BookSource {
                id: "source-1".to_string(),
                book_id: "book-1".to_string(),
                revision: 1,
                format: "epub".to_string(),
                source_kind: "original".to_string(),
                object_key: source_blob.object_key.clone(),
                source_name: Some("book.epub".to_string()),
                created_at: 1,
            },
            progress: ReadingProgress {
                book_id: "book-1".to_string(),
                source_revision: 1,
                content_unit_id: Some("unit-1".to_string()),
                spine_index: 2,
                locator_json: r#"{"spine":2}"#.to_string(),
                fraction: 0.4,
                updated_at: 2,
            },
            units: vec![ContentUnit {
                id: "unit-1".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                parent_id: None,
                ordinal: 2,
                kind: "chapter".to_string(),
                href: Some("EPUB/chapter.xhtml".to_string()),
                source_locator_json: r#"{"href":"EPUB/chapter.xhtml"}"#.to_string(),
                title: Some("第一章".to_string()),
                media_type: Some("text/html".to_string()),
                source_text: Some("<p>searchable document body</p>".to_string()),
                block_json: serde_json::to_string(&BlockDocument::new(vec![
                    crate::document::Block::paragraph("block-1", "searchable document body"),
                ]))
                .unwrap(),
                revision: 1,
                created_at: 1,
                updated_at: 1,
            }],
            toc: vec![TocEntry {
                id: "toc-1".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                parent_id: None,
                ordinal: 0,
                depth: 0,
                label: "第一章".to_string(),
                href: Some("EPUB/chapter.xhtml".to_string()),
                content_unit_id: "unit-1".to_string(),
                target_block_id: Some("block-1".to_string()),
                locator_json: r#"{"href":"EPUB/chapter.xhtml"}"#.to_string(),
                created_at: 1,
            }],
            assets: vec![Asset {
                id: "asset-cover".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                object_key: cover_blob.object_key.clone(),
                kind: "cover".to_string(),
                href: "EPUB/cover.png".to_string(),
                media_type: cover_blob.media_type.clone(),
                byte_len: cover_blob.byte_len,
                width: Some(120),
                height: Some(180),
                created_at: 1,
            }],
            refs: vec![AssetRef {
                id: "asset-ref-1".to_string(),
                content_unit_id: "unit-1".to_string(),
                asset_id: "asset-cover".to_string(),
                relation: "image".to_string(),
                ordinal: 0,
                locator_json: serde_json::to_string(&DocumentLocator::block(
                    "book-1", "unit-1", "block-1",
                ))
                .unwrap(),
                created_at: 1,
            }],
            chunks: vec![SearchChunk {
                id: "chunk-1".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                content_unit_id: "unit-1".to_string(),
                ordinal: 0,
                heading: "第一章".to_string(),
                body: "searchable document body".to_string(),
                token_count: 3,
                content_hash: "hash-chunk-1".to_string(),
                locator_json: serde_json::to_string(&DocumentLocator::block(
                    "book-1", "unit-1", "block-1",
                ))
                .unwrap(),
                created_at: 1,
            }],
            pages: vec![VisualPage {
                id: "page-1".to_string(),
                book_id: "book-1".to_string(),
                source_id: "source-1".to_string(),
                content_unit_id: Some("unit-1".to_string()),
                page_index: 0,
                object_key: page_blob.object_key.clone(),
                width: 800,
                height: 1200,
                render_scale: 1.0,
                renderer: "structural-svg".to_string(),
                renderer_version: "1".to_string(),
                document_revision: 1,
                unit_revision: 1,
                profile_id: "render-profile-test".to_string(),
                fidelity: "structural".to_string(),
                locator_json: r#"{"page":0}"#.to_string(),
                created_at: 1,
            }],
            source_blob,
            additional_blobs: vec![cover_blob, page_blob],
        }
    }

    fn graph(&self) -> DocumentGraph<'_> {
        DocumentGraph {
            book: &self.book,
            source_blob: &self.source_blob,
            additional_blobs: &self.additional_blobs,
            source: &self.source,
            progress: Some(&self.progress),
            content_units: &self.units,
            toc_entries: &self.toc,
            assets: &self.assets,
            asset_refs: &self.refs,
            search_chunks: &self.chunks,
            visual_pages: &self.pages,
        }
    }

    fn revision_two(&self) -> Self {
        let mut next = self.clone();
        next.source_blob = BlobRecord {
            object_key: "objects/source-2".to_string(),
            media_type: "application/epub+zip".to_string(),
            byte_len: 111,
            hash: "hash-source-2".to_string(),
            created_at: 3,
        };
        next.book.revision = 2;
        next.book.source_object_key = next.source_blob.object_key.clone();
        next.book.updated_at = 3;
        next.source.id = "source-2".to_string();
        next.source.revision = 2;
        next.source.source_kind = "normalized".to_string();
        next.source.object_key = next.source_blob.object_key.clone();
        next.source.created_at = 3;
        next.progress.source_revision = 2;
        next.progress.updated_at = 3;
        for unit in &mut next.units {
            unit.source_id = next.source.id.clone();
            unit.revision = 2;
            unit.updated_at = 3;
        }
        for entry in &mut next.toc {
            entry.source_id = next.source.id.clone();
        }
        for asset in &mut next.assets {
            asset.source_id = next.source.id.clone();
        }
        for chunk in &mut next.chunks {
            chunk.source_id = next.source.id.clone();
            chunk.body = "searchable revised document body".to_string();
            chunk.content_hash = "hash-chunk-2".to_string();
        }
        for page in &mut next.pages {
            page.source_id = next.source.id.clone();
            page.document_revision = 2;
            page.unit_revision = 2;
        }
        next
    }
}

fn open_database() -> (tempfile::TempDir, Connection) {
    let temp = tempfile::tempdir().unwrap();
    let conn = connection::open_or_recreate(&temp.path().join(connection::DATABASE_FILE)).unwrap();
    (temp, conn)
}

fn running_visual_job(conn: &Connection) -> VisualJobSpec {
    let job = index_jobs::get(conn, "visual-render:source-1")
        .unwrap()
        .unwrap();
    let (spec, completed_pages) = decode_persisted_visual_job(&job.cursor_json).unwrap();
    assert_eq!(completed_pages, 0);
    index_jobs::update_state(
        conn,
        &job.id,
        IndexJobStatus::Running,
        &job.cursor_json,
        None,
        10,
        Some(10),
        None,
    )
    .unwrap();
    spec
}

fn staged_visual_blob(suffix: &str, byte_len: u64) -> BlobRecord {
    BlobRecord {
        object_key: format!("objects/staged-{suffix}"),
        media_type: "image/png".to_string(),
        byte_len,
        hash: format!("hash-staged-{suffix}"),
        created_at: 10,
    }
}

fn staged_visual_page(
    fixture: &Fixture,
    spec: &VisualJobSpec,
    id: &str,
    page_index: usize,
    object_key: &str,
) -> VisualPage {
    VisualPage {
        id: id.to_string(),
        book_id: spec.book_id.clone(),
        source_id: spec.source_id.clone(),
        content_unit_id: Some(fixture.units[0].id.clone()),
        page_index,
        object_key: object_key.to_string(),
        width: 800,
        height: 1200,
        render_scale: spec.profile.scale(),
        renderer: spec.renderer.clone(),
        renderer_version: spec.renderer_version.clone(),
        document_revision: spec.document_revision.get(),
        unit_revision: fixture.units[0].revision,
        profile_id: spec.profile.stable_id(),
        fidelity: match spec.fidelity {
            crate::preview::RenderFidelity::Normalized => "normalized",
            crate::preview::RenderFidelity::Structural => "structural",
            crate::preview::RenderFidelity::OfficeEnhanced => "office_enhanced",
        }
        .to_string(),
        locator_json: serde_json::to_string(&DocumentLocator::unit(
            spec.book_id.clone(),
            fixture.units[0].id.clone(),
        ))
        .unwrap(),
        created_at: 10,
    }
}

mod document_test;

mod office_test;

mod translation_test;

mod visual_test;
