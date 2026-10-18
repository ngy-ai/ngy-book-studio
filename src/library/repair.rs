//! 导入解析泄漏的就地修复（不删除图书、不推进版本、保留译文与批注）。
//!
//! 旧解析器会把每个 XHTML 的 `<head>/<title>` 文本当成正文首段，导致每章重复渲染
//! 标题。这里直接就地修改存储的内容单元，避免重新导入（重导入会丢掉已付费的译文）。

use std::collections::HashMap;
use std::io::Cursor;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use super::search::build_search_chunks;
use super::*;
use anyhow::{Result, bail};

use crate::annotations::{compact_text, reader_body_text};
use crate::db::{self};
use crate::document::{Block, BlockDocument, ContentUnit, SourceLocator};
use crate::markup::{parse_source_for_unit, serialize_source};
use crate::storage::BlobKey;
use rbook::Epub;

/// 单本书修复的结果摘要，供调用方记录与核对。
#[derive(Clone, Copy, Debug, Default)]
pub struct RepairReport {
    /// 被清理的内容单元数量。
    pub units: u64,
    /// 因起点落在被删文本内而被删除的批注数量。
    pub annotations_deleted: u64,
}

impl LibraryStore {
    /// 就地修复一本书导入时产生的 `<head>` 文本泄漏。返回被改动的单元与批注数量。
    ///
    /// 只改动 `content_units` 及其派生的 `translations`/`annotations` 偏移，**不**触碰
    /// `book_sources` 与 `books` 的外键关系。因此数据库不变量（尤其是
    /// `current_sources_are_valid` 要求的 `book_sources.object_key = books.source_object_key`）
    /// 保持不变，重启时不会被判定为“数据关系不符”而重建数据库。阅读器与导出都从
    /// `content_units` 重新生成，因此正文会变为干净。
    ///
    /// 不推进文档版本，因此笔记与整本译文保持可读。需要调用方在修复后重新打开图书以刷新
    /// 内存中的投影。
    pub fn repair_leaked_head_titles(&mut self, book_id: &str) -> Result<RepairReport> {
        let mut document = self.document(book_id)?;
        let book_title = document.title.clone();

        // 精确识别泄漏块：读取原书 EPUB 中每个单元的 <head><title>，与单元首块文本比对。
        let original_titles = self.original_unit_titles(book_id)?;

        // unit_id -> (block_json, source, old_block_id -> (new_block_id, ordinal), removed_len)
        let mut changed: HashMap<String, (String, String, HashMap<String, (String, u64)>, u64)> =
            HashMap::new();
        let mut report = RepairReport::default();

        for unit in document.units.iter_mut() {
            if !leaked_first_block(unit, &book_title, &original_titles) {
                continue;
            }
            let old_blocks = std::mem::take(&mut unit.document.blocks);
            let removed = old_blocks[0].clone();
            let kept: Vec<Block> = old_blocks[1..].to_vec();
            let cleaned_doc = BlockDocument::new(kept);
            let cleaned_source = serialize_source(&cleaned_doc)?;
            let reindexed = parse_source_for_unit(&cleaned_source, &unit.id)?;
            if reindexed.document.blocks.len() != cleaned_doc.blocks.len() {
                bail!("重排后块数量不一致，放弃修复单元 {}", unit.id);
            }
            let mut id_map = HashMap::new();
            for (index, old_block) in old_blocks.iter().enumerate().skip(1) {
                let new_block = &reindexed.document.blocks[index - 1];
                id_map.insert(
                    old_block.id().to_string(),
                    (new_block.id().to_string(), (index - 1) as u64),
                );
            }
            let removed_len =
                reader_body_text(&serialize_source(&BlockDocument::new(vec![removed]))?)
                    .encode_utf16()
                    .count() as u64;

            let new_document = reindexed.document;
            let new_source = serialize_source(&new_document)?;
            let new_block_json = serde_json::to_string(&new_document)?;
            unit.document = new_document;
            unit.source = new_source.clone();
            changed.insert(
                unit.id.clone(),
                (new_block_json, new_source, id_map, removed_len),
            );
            report.units += 1;
        }

        if changed.is_empty() {
            return Ok(report);
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 读取当前发布来源与被改单元的行：事务只接收数据，SQL 全部在 `src/db/`。
        let mut conn = db::open_conn(&self.db_path)?;
        let record = self.book_record(book_id)?;
        let source = db::book_sources::get_revision(&conn, book_id, record.revision)?
            .context("当前图书来源不存在")?;
        let mut repaired = Vec::with_capacity(changed.len());
        for (unit_id, (block_json, source_text, id_map, removed_len)) in &changed {
            let mut unit = db::content_units::get(&conn, unit_id)?.context("内容单元不存在")?;
            unit.block_json = block_json.clone();
            unit.source_text = Some(source_text.clone());
            let block_remap = id_map
                .iter()
                .map(|(old_id, (new_id, ordinal))| (old_id.clone(), new_id.clone(), *ordinal))
                .collect();
            repaired.push(db::transactions::RepairedUnit {
                unit,
                block_remap,
                removed_utf16_len: *removed_len,
            });
        }
        // 搜索分块按清理后的文档重建，与正文在同一个事务里替换：FTS 走触发器，
        // embedding 随旧块级联删除（都是可按内容重建的派生数据）。
        //
        // 渲染页单元的检索存在来自视觉分块，不在文档分块里：整份重建必须把它们
        // 排除，否则空正文的兜底分块会和视觉分块撞 (content_unit_id, ordinal)
        // 唯一索引（office 增强预览的书修复时报 UNIQUE 约束）。
        let mut chunk_source = document.clone();
        chunk_source.units.retain(|unit| {
            !matches!(
                unit.source_locator,
                Some(SourceLocator::OfficeRenderedPage { .. })
            )
        });
        let search_chunks = build_search_chunks(&chunk_source, &source.id, now)?;

        report.annotations_deleted = db::transactions::repair_leaked_head_titles(
            &mut conn,
            book_id,
            &source.id,
            &repaired,
            &search_chunks,
            now,
        )?;

        if let Some(position) = self.books.iter().position(|book| book.id == book_id) {
            self.books[position] = self.book_record(book_id)?;
        }
        Ok(report)
    }
}

/// 若单元首块是被旧解析器泄漏的 `<head>/<title>` 文本，返回 `true`。
///
/// 精确判定依据单元原始 XHTML 的 `<title>`（与首块压缩后文本相等即视为泄漏）；
/// 读不到原书时回退到启发式：首块文本等于章节文件名或书名。
fn leaked_first_block(
    unit: &ContentUnit,
    book_title: &str,
    original_titles: &HashMap<String, String>,
) -> bool {
    let first = match unit.document.blocks.first() {
        Some(block) => block,
        None => return false,
    };
    let compacted = compact_text(&first.plain_text());
    if compacted.is_empty() || compacted.len() > 256 {
        return false;
    }
    if let Some(SourceLocator::Epub { href }) = &unit.source_locator {
        if let Some(original_title) = original_titles.get(&href_key(href)) {
            if compact_text(original_title) == compacted {
                return true;
            }
        }
    }
    if let Some(SourceLocator::Epub { href }) = &unit.source_locator {
        let file_name = href
            .split(['?', '#'])
            .next()
            .and_then(|path| path.rsplit('/').next())
            .unwrap_or_default();
        if !file_name.is_empty() && compact_text(file_name) == compacted {
            return true;
        }
    }
    !book_title.is_empty() && compact_text(book_title) == compacted
}

/// 读取原书 EPUB 中每个可见 spine 条目的 `<head><title>`，按规范化 href 归并。
fn read_epub_titles(bytes: &[u8]) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    let Ok(epub) = Epub::read(Cursor::new(bytes.to_vec())) else {
        return titles;
    };
    for entry in epub
        .spine()
        .iter()
        .filter(|entry| entry.is_linear())
        .filter_map(|entry| entry.manifest_entry())
    {
        let href = entry.href().as_str().to_string();
        if let Ok(html) = entry.read_str() {
            let title = extract_html_title(&html);
            if !title.is_empty() {
                titles.insert(href_key(&href), title);
            }
        }
    }
    titles
}

/// 从 HTML 文本里取出 `<title>` 元素内容（小写定位，保留原大小写）。
fn extract_html_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<title>") else {
        return String::new();
    };
    let start = start + "<title>".len();
    let Some(end) = lower[start..].find("</title>") else {
        return String::new();
    };
    html[start..start + end].trim().to_string()
}

/// 规范化 href 用于匹配单元的定位符（与 importer 的 `href_key` 一致）。
fn href_key(href: &str) -> String {
    href.split(['?', '#'])
        .next()
        .unwrap_or(href)
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

impl LibraryStore {
    /// 取原书来源的 blob key，并读出各单元的 `<head><title>`。
    fn original_unit_titles(&self, book_id: &str) -> Result<HashMap<String, String>> {
        let conn = db::open_conn(&self.db_path)?;
        let key = db::book_sources::list_for_book(&conn, book_id)?
            .into_iter()
            .find(|source| source.source_kind == "original")
            .map(|source| source.object_key);
        let Some(key) = key else {
            return Ok(HashMap::new());
        };
        let Ok(blob_key) = BlobKey::from_str(&key) else {
            return Ok(HashMap::new());
        };
        let Ok(bytes) = self.io.block_on(self.blob_store.get(&blob_key)) else {
            return Ok(HashMap::new());
        };
        Ok(read_epub_titles(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::ai::translation::{StoredTranslation, TranslationSegment};
    use crate::annotations::{Annotation, AnnotationKind, TextAnchor};
    use crate::db;
    use crate::document::{Block, BlockDocument, Inline, deterministic_id};
    use rusqlite::params;
    use tempfile::tempdir;

    fn insert_unit_with_leak(store: &LibraryStore, book_id: &str, unit_id: &str, revision: u64) {
        let leaked = Block::Paragraph {
            id: deterministic_id(
                "block",
                format!("{unit_id}\0paragraph\0{idx}", idx = 0).as_bytes(),
            ),
            content: vec![Inline::text("Demo")],
        };
        let heading = Block::Heading {
            id: deterministic_id(
                "block",
                format!("{unit_id}\0heading\0{idx}", idx = 1).as_bytes(),
            ),
            level: 1,
            content: vec![Inline::text("第一章")],
        };
        let body = Block::Paragraph {
            id: deterministic_id(
                "block",
                format!("{unit_id}\0paragraph\0{idx}", idx = 2).as_bytes(),
            ),
            content: vec![Inline::text("正文内容")],
        };
        let document = BlockDocument::new(vec![leaked, heading.clone(), body.clone()]);
        let conn = db::open_conn(&store.db_path).unwrap();
        conn.execute(
            "UPDATE content_units SET block_json = ?1, source_text = ?2 WHERE id = ?3",
            params![
                serde_json::to_string(&document).unwrap(),
                crate::markup::serialize_source(&document).unwrap(),
                unit_id
            ],
        )
        .expect("测试：写回泄漏单元");

        // 译文放在标题（旧序号 1）与正文（旧序号 2）上。
        for (block, kind, index) in [(&heading, "heading", 1u64), (&body, "paragraph", 2u64)] {
            let mut translation = db::translations::NewTranslation {
                book_id: book_id.into(),
                content_unit_id: unit_id.into(),
                block_id: deterministic_id(
                    "block",
                    format!("{unit_id}\0{kind}\0{index}").as_bytes(),
                ),
                ordinal: index,
                document_revision: revision,
                unit_revision: revision,
                target_language: "zh-Hans".into(),
                source_language: Some("en".into()),
                model: "m".into(),
                source_text: "x".into(),
                translated_text: format!("译文-{index}"),
                created_at: 1,
                updated_at: 1,
            };
            translation.translated_text = serde_json::to_string(&StoredTranslation {
                execution_identity: "translation-v2:t".into(),
                segments: vec![TranslationSegment {
                    source: "x".into(),
                    translated: format!("译文-{index}"),
                }],
            })
            .unwrap();
            db::translations::upsert(&conn, &translation).expect("测试：写入译文");
            let _ = block;
        }

        // 批注落在正文块，起点远在被删文本之后。
        db::annotations::insert(
            &conn,
            &Annotation {
                id: "note-1".into(),
                book_id: book_id.into(),
                content_unit_id: unit_id.into(),
                document_revision: revision,
                unit_revision: revision,
                anchor: TextAnchor {
                    quote: "正文内容".into(),
                    start: 100,
                    end: 104,
                },
                kind: AnnotationKind::Highlight,
                comment: None,
                created_at: 1,
                updated_at: 1,
                stale: false,
            },
        )
        .expect("测试：写入批注");
    }

    #[test]
    fn repair_cleans_leaked_first_block_without_losing_translations_or_notes() {
        let temp = tempdir().unwrap();
        let mut store = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
        let book = store.create_book("Demo", "作者").unwrap();
        let unit = store.document(&book.id).unwrap().units[0].clone();
        let revision = unit.revision.get();
        insert_unit_with_leak(&store, &book.id, &unit.id, revision);

        let report = store.repair_leaked_head_titles(&book.id).unwrap();
        assert_eq!(report.units, 1);
        assert_eq!(report.annotations_deleted, 0);

        let units = store.document(&book.id).unwrap().units;
        assert_eq!(units.len(), 1);
        // 被泄漏的首块已删除，标题成为首块。
        assert!(matches!(units[0].document.blocks[0], Block::Heading { .. }));
        assert_eq!(units[0].document.blocks.len(), 2);

        // 文档版本未推进，笔记与整本译文保持可读。
        assert_eq!(store.book_record(&book.id).unwrap().revision, book.revision);

        // 标题译文按重排后的 id（序号 0）仍然可查。
        let new_heading_id =
            deterministic_id("block", format!("{0}\0heading\0{1}", unit.id, 0).as_bytes());
        let conn = db::open_conn(&store.db_path).unwrap();
        let rows = db::translations::list_for_unit(&conn, &unit.id, "zh-Hans").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|row| row.block_id == new_heading_id && row.translated_text.contains("译文-1")));

        // 批注偏移按被删文本长度（"Demo" = 4）前移。
        let notes = db::annotations::list(&conn, &book.id, Some(&unit.id)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].anchor.start, 100 - 4);
        assert_eq!(notes[0].anchor.end, 104 - 4);

        // 回归守卫：修复后数据库不变量必须成立，否则重启会被判定重建。
        assert!(db::schema::is_current(&conn).unwrap());
    }

    /// 修复必须连同搜索分块一起重建（否则 FTS 留着被删标题、定位错乱），但属于渲染页
    /// 的视觉分块要保留。回归 2026-10-18：修复路径曾完全不碰 search_chunks。
    #[test]
    fn repair_rebuilds_search_chunks_and_keeps_visual_ones() {
        let temp = tempdir().unwrap();
        let mut store = LibraryStore::load_from(temp.path().to_path_buf()).unwrap();
        let book = store.create_book("Demo", "作者").unwrap();
        let unit = store.document(&book.id).unwrap().units[0].clone();
        let revision = unit.revision.get();
        insert_unit_with_leak(&store, &book.id, &unit.id, revision);

        let conn = db::open_conn(&store.db_path).unwrap();
        let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap();
        // 一枚视觉分块：生产中它属于独立的渲染页单元（真实 content_units 行，
        // 与正文单元不共用 (content_unit_id, ordinal) 唯一索引），修复不得碰它。
        let rendered_unit_id = format!("{}-rendered", unit.id);
        db::content_units::insert(
            &conn,
            &db::content_units::ContentUnit {
                id: rendered_unit_id.clone(),
                book_id: book.id.clone(),
                source_id: source.id.clone(),
                parent_id: None,
                ordinal: 99,
                kind: "page".into(),
                href: None,
                source_locator_json: serde_json::to_string(&SourceLocator::office_rendered_page(1))
                    .unwrap(),
                title: None,
                media_type: None,
                source_text: None,
                block_json: serde_json::to_string(&BlockDocument::new(Vec::new())).unwrap(),
                revision,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();
        let visual = db::search_chunks::SearchChunk {
            id: "vision-chunk-page-1".into(),
            book_id: book.id.clone(),
            source_id: source.id.clone(),
            content_unit_id: rendered_unit_id.clone(),
            ordinal: 0,
            heading: String::new(),
            body: "渲染页文字".into(),
            token_count: 5,
            content_hash: "visual".into(),
            locator_json: serde_json::to_string(&DocumentLocator::unit(
                &book.id,
                &rendered_unit_id.clone(),
            ))
            .unwrap(),
            created_at: 1,
        };
        db::search_chunks::upsert_derived(&conn, &visual).unwrap();

        store.repair_leaked_head_titles(&book.id).unwrap();

        // 被删标题不再可被搜到，保留下来的文字可以。
        assert!(store.search_book(&book.id, "第一章", 10).unwrap().len() >= 1);
        assert!(
            store.search_book(&book.id, "Demo", 10).unwrap().is_empty(),
            "泄漏标题已删，不得仍能被搜到"
        );
        // 文档分块确实重建过，视觉分块原样保留。
        assert!(db::search_chunks::count_for_source(&conn, &source.id).unwrap() >= 2);
        assert!(
            db::search_chunks::get(&conn, "vision-chunk-page-1")
                .unwrap()
                .is_some()
        );
    }

    /// 仅在设置 `MOYE_REPAIR_DIR` 时运行，对真实数据目录就地修复并校验不变量。
    /// 正常 `cargo test` 不触发，避免误改真实书库。
    #[test]
    fn repair_preserves_invariants_on_real_data_dir() {
        let Ok(dir) = std::env::var("MOYE_REPAIR_DIR") else {
            return;
        };
        let mut store = LibraryStore::load_from(PathBuf::from(&dir)).unwrap();
        let targets: Vec<String> = store.books.iter().map(|b| b.id.clone()).collect();
        assert!(!targets.is_empty(), "未找到任何图书，无法验证修复");
        for book_id in &targets {
            let report = store.repair_leaked_head_titles(book_id).unwrap();
            eprintln!("repair {book_id}: {report:?}");
        }
        // 重新打开并校验数据库不变量，确保不会被判定重建。
        let conn = db::open_conn(&store.db_path).unwrap();
        assert!(
            db::schema::is_current(&conn).unwrap(),
            "修复后数据库不变量不成立，重启会被重建"
        );
        // 图书与译文仍然可读。
        assert_eq!(store.books.len(), targets.len());
        for book_id in &targets {
            let translations: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM translations WHERE book_id = ?1",
                    params![book_id],
                    |row| row.get(0),
                )
                .unwrap();
            let annotations: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM annotations WHERE book_id = ?1",
                    params![book_id],
                    |row| row.get(0),
                )
                .unwrap();
            eprintln!("book {book_id}: translations={translations} annotations={annotations}");
        }
    }
}
