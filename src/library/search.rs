//! Keyword search and the chunking that feeds it.

use super::*;

impl LibraryStore {
    pub fn search_library(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        search_index(&self.db_path, None, query, limit)
    }

    pub fn search_book(&self, book_id: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        ensure!(
            self.books.iter().any(|book| book.id == book_id),
            "图书不存在"
        );
        search_index(&self.db_path, Some(book_id), query, limit)
    }
}
pub(crate) fn build_search_chunks(
    document: &BookDocument,
    source_id: &str,
    now: u64,
) -> Result<Vec<db::search_chunks::SearchChunk>> {
    let mut result = Vec::new();
    for unit in &document.units {
        let mut ordinal = 0;
        for block in &unit.document.blocks {
            let text = block.plain_text();
            if text.trim().is_empty() {
                continue;
            }
            let ranges = character_chunks(&text, SEARCH_CHUNK_CHARACTERS, SEARCH_CHUNK_OVERLAP);
            for (start, end) in ranges {
                ensure!(
                    result.len() < MAX_SEARCH_CHUNKS_PER_DOCUMENT,
                    "文档搜索分块超过 {MAX_SEARCH_CHUNKS_PER_DOCUMENT} 个安全上限"
                );
                let body = text[start..end].to_string();
                let mut locator = DocumentLocator::text(
                    &document.id,
                    &unit.id,
                    block.id(),
                    start as u64,
                    end as u64,
                );
                if let Some(source) = unit.source_locator.clone() {
                    locator = locator.with_source(source);
                }
                result.push(db::search_chunks::SearchChunk {
                    id: deterministic_id(
                        "chunk",
                        format!("{}\0{}\0{}\0{}", unit.id, block.id(), start, end).as_bytes(),
                    ),
                    book_id: document.id.clone(),
                    source_id: source_id.to_string(),
                    content_unit_id: unit.id.clone(),
                    ordinal,
                    heading: unit.title.clone(),
                    token_count: body.chars().count().div_ceil(4),
                    content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
                    body,
                    locator_json: serde_json::to_string(&locator)?,
                    created_at: now,
                });
                ordinal += 1;
            }
        }
        if ordinal == 0 {
            ensure!(
                result.len() < MAX_SEARCH_CHUNKS_PER_DOCUMENT,
                "文档搜索分块超过 {MAX_SEARCH_CHUNKS_PER_DOCUMENT} 个安全上限"
            );
            let mut locator = DocumentLocator::unit(&document.id, &unit.id);
            if let Some(source) = unit.source_locator.clone() {
                locator = locator.with_source(source);
            }
            result.push(db::search_chunks::SearchChunk {
                id: deterministic_id("chunk", format!("{}\0empty", unit.id).as_bytes()),
                book_id: document.id.clone(),
                source_id: source_id.to_string(),
                content_unit_id: unit.id.clone(),
                ordinal: 0,
                heading: unit.title.clone(),
                body: String::new(),
                token_count: 0,
                content_hash: blake3::hash(b"").to_hex().to_string(),
                locator_json: serde_json::to_string(&locator)?,
                created_at: now,
            });
        }
    }
    Ok(result)
}

pub(crate) fn character_chunks(text: &str, size: usize, overlap: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let character_count = boundaries.len() - 1;
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < character_count {
        let end = (start + size).min(character_count);
        ranges.push((boundaries[start], boundaries[end]));
        if end == character_count {
            break;
        }
        start = end.saturating_sub(overlap.min(size.saturating_sub(1)));
    }
    ranges
}

pub(crate) fn search_index(
    db_path: &Path,
    book_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if limit == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    ensure!(
        query.chars().count() <= MAX_SEARCH_QUERY_CHARS,
        "搜索内容最多 {MAX_SEARCH_QUERY_CHARS} 个字符"
    );
    ensure!(
        !query
            .chars()
            .any(|character| character == '\0'
                || (character.is_control() && !character.is_whitespace())),
        "搜索内容包含无效控制字符"
    );
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    db::book_search::search(
        &db::open_conn(db_path)?,
        book_id,
        &terms,
        limit.min(MAX_SEARCH_RESULTS) as i64,
    )
}
