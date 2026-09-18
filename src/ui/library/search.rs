//! Search modes and turning search passages into hits.

use super::*;

pub(crate) fn search_mode_index(mode: SearchMode) -> usize {
    match mode {
        SearchMode::Keyword => 0,
        SearchMode::Semantic => 1,
        SearchMode::Hybrid => 2,
    }
}

pub(crate) fn search_mode_from_index(index: usize) -> SearchMode {
    match index {
        0 => SearchMode::Keyword,
        1 => SearchMode::Semantic,
        _ => SearchMode::Hybrid,
    }
}

pub(crate) fn search_mode_label(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Keyword => "关键词",
        SearchMode::Semantic => "语义",
        SearchMode::Hybrid => "混合",
    }
}

pub(crate) fn is_current_search_completion(
    completed_generation: u64,
    current_generation: u64,
) -> bool {
    completed_generation == current_generation
}

pub(crate) fn should_start_library_search(close_requested: bool, query: &str) -> bool {
    !close_requested && !query.trim().is_empty()
}

pub(crate) fn should_sync_library_scope(close_requested: bool) -> bool {
    !close_requested
}

pub(crate) fn name_input_modal_can_close(pending_request: Option<u64>) -> bool {
    pending_request.is_none()
}

pub(crate) fn search_hit_from_passage(
    passage: PassageRecord,
    author: String,
    unit_index: usize,
) -> SearchHit {
    SearchHit {
        book_id: passage.book_id,
        book_title: passage.book_title,
        author,
        spine_index: Some(unit_index),
        chapter_title: Some(passage.unit_title),
        href: None,
        snippet: passage.text,
        relevance: passage.relevance.unwrap_or_default(),
    }
}

pub(crate) fn resolve_search_hits(
    library: &LibraryStore,
    passages: Vec<PassageRecord>,
) -> Result<Vec<SearchHit>> {
    let mut documents = HashMap::<String, BookDocument>::new();
    let mut hits = Vec::with_capacity(passages.len());
    for passage in passages {
        let book_id = passage.book_id.clone();
        if !documents.contains_key(&book_id) {
            documents.insert(
                book_id.clone(),
                library
                    .document(&book_id)
                    .with_context(|| format!("无法定位搜索结果对应的图书：{book_id}"))?,
            );
        }
        let document = &documents[&book_id];
        let unit_index = document
            .units
            .iter()
            .position(|unit| unit.id == passage.unit_id)
            .with_context(|| format!("搜索结果对应的内容单元已不存在：{}", passage.unit_id))?;
        let author = if document.authors.is_empty() {
            "未知作者".to_string()
        } else {
            document.authors.join("、")
        };
        hits.push(search_hit_from_passage(passage, author, unit_index));
    }
    Ok(hits)
}
