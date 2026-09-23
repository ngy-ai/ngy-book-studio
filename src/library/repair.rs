//! 一次性修复：清理导入时泄漏进正文的 `<head>`/`<title>` 文本。
//!
//! 旧版 `parse_html` 会把 XHTML `<head>` 里的 `<title>`（常被 pandoc 写成章节
//! 文件名）当作正文第一段，于是阅读视图顶部出现裸文件名、且导出层标题去重失效
//! 导致标题重复。这里就地修复单本书，不推进任何文档版本，因此笔记与整本译文
//! 仍然有效：删除首块后剩余块按位置重排（id 整体左移一位），译文行重映射到新
//! id，批注偏移按被删文本在 `reader_body_text` 中的 UTF-16 长度前移，最后用清理
//! 后的文档重生成规范化 EPUB 投影。

use std::collections::HashMap;
use std::io::Cursor;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use anyhow::{Result, bail};
use rusqlite::{params, TransactionBehavior};

use crate::annotations::{compact_text, reader_body_text};
use crate::db::{self, blobs::BlobRecord};
use crate::document::{Block, BlockDocument, ContentUnit, SourceLocator};
use crate::export::{BuiltinDocumentExporter, ExportFormat};
use crate::markup::{parse_source_for_unit, serialize_source};
use crate::storage::BlobKey;
use rusqlite::OptionalExtension;
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
    /// 不推进文档版本，因此笔记与整本译文保持可读。需要调用方在修复后重新打开图书
    /// 以刷新内存中的投影。
    pub fn repair_leaked_head_titles(&mut self, book_id: &str) -> Result<RepairReport> {
        let mut document = self.document(book_id)?;
        let book_title = document.title.clone();
        let current_revision = document.revision.get();
        let mut units = document.units;

        // 精确识别泄漏块：读取原书 EPUB 中每个单元的 <head><title>，与单元首块文本比对。
        let original_titles = self.original_unit_titles(book_id)?;

        // unit_id -> (block_json, source, old_block_id -> (new_block_id, ordinal), removed_len)
        let mut changed: HashMap<
            String,
            (String, String, HashMap<String, (String, u64)>, u64),
        > = HashMap::new();
        let mut report = RepairReport::default();

        for unit in units.iter_mut() {
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
            let removed_len = reader_body_text(&serialize_source(&BlockDocument::new(vec![
                removed,
            ]))?)
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

        document.units = units;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut conn = crate::db::open_conn(&self.db_path)?;
        let resolver = |asset_id: &str| self.asset_bytes(book_id, asset_id);
        let new_bytes =
            BuiltinDocumentExporter.export_bytes(&document, ExportFormat::Epub, &resolver)?;
        let new_key = self
            .io
            .block_on(self.blob_store.put(new_bytes.as_slice()))?;

        // book_sources.object_key 外键指向 blobs 表，必须先登记新对象元数据。
        db::blobs::ensure_present(
            &conn,
            &BlobRecord {
                object_key: new_key.as_str().to_string(),
                media_type: "application/epub+zip".into(),
                byte_len: new_bytes.len() as u64,
                hash: new_key.digest().to_string(),
                created_at: now as u64,
            },
        )?;

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (unit_id, (block_json, source, id_map, removed_len)) in &changed {
            tx.execute(
                "UPDATE content_units SET block_json = ?1, source_text = ?2, updated_at = ?3
                 WHERE id = ?4 AND book_id = ?5",
                params![block_json, source, now, unit_id, book_id],
            )
            .context("无法写回清理后的内容单元")?;
            for (old_id, (new_id, ordinal)) in id_map {
                tx.execute(
                    "UPDATE translations SET block_id = ?1, ordinal = ?2
                     WHERE book_id = ?3 AND content_unit_id = ?4 AND block_id = ?5",
                    params![new_id, *ordinal as i64, book_id, unit_id, old_id],
                )
                .context("无法重映射译文块标识")?;
            }
            if *removed_len > 0 {
                tx.execute(
                    "UPDATE annotations
                     SET start_offset = MAX(0, start_offset - ?1),
                         end_offset = MAX(0, end_offset - ?1)
                     WHERE book_id = ?2 AND content_unit_id = ?3 AND start_offset >= ?1",
                    params![*removed_len as i64, book_id, unit_id],
                )
                .context("无法前移批注偏移")?;
                let deleted = tx
                    .execute(
                        "DELETE FROM annotations
                         WHERE book_id = ?1 AND content_unit_id = ?2 AND start_offset < ?3",
                        params![book_id, unit_id, *removed_len as i64],
                    )
                    .context("无法清理落在被删文本内的批注")?;
                report.annotations_deleted += deleted as u64;
            }
        }
        tx.execute(
            "UPDATE book_sources SET object_key = ?1 WHERE book_id = ?2 AND revision = ?3",
            params![new_key.as_str(), book_id, current_revision as i64],
        )
        .context("无法更新规范化 EPUB 投影")?;
        tx.commit()?;

        if let Some(position) = self.books.iter().position(|book| book.id == book_id) {
            self.books[position] = self.book_record(book_id)?;
        }
        Ok(report)
    }
}

/// 若单元首块是被旧解析器泄漏的 `<head>`/`<title>` 文本，返回 `true`。
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
        let key: Option<String> = conn
            .query_row(
                "SELECT object_key FROM book_sources
                 WHERE book_id = ?1 AND source_kind = 'original'
                 ORDER BY revision LIMIT 1",
                params![book_id],
                |row| row.get(0),
            )
            .optional()?;
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
    use crate::ai::translation::{StoredTranslation, TranslationSegment};
    use crate::annotations::{Annotation, AnnotationKind, TextAnchor};
    use crate::db;
    use crate::document::{deterministic_id, Block, BlockDocument, Inline};
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
            translation.translated_text =
                serde_json::to_string(&StoredTranslation {
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
        assert_eq!(
            store.book_record(&book.id).unwrap().revision,
            book.revision
        );

        // 标题译文按重排后的 id（序号 0）仍然可查。
        let new_heading_id = deterministic_id(
            "block",
            format!("{0}\0heading\0{1}", unit.id, 0).as_bytes(),
        );
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
    }
}
