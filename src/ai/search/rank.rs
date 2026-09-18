//! Turning hits into ranked passages and fusing the two recall arms.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct RankedPassage {
    pub(crate) record: PassageRecord,
}

pub(crate) fn passage_from_document(
    document: search_chunks::SearchChunkDocument,
    relevance: f64,
) -> Result<RankedPassage> {
    let locator = serde_json::from_str::<DocumentLocator>(&document.chunk.locator_json)
        .context("search chunk locator is invalid")?;
    if locator.book_id != document.chunk.book_id
        || locator.unit_id != document.chunk.content_unit_id
    {
        bail!("search chunk locator does not match its book and content unit");
    }
    if matches!(
        locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    ) {
        bail!("preview-only Office rendered pages cannot be returned by search");
    }
    Ok(RankedPassage {
        record: PassageRecord {
            passage_id: document.chunk.id,
            book_id: document.chunk.book_id,
            book_title: document.book_title,
            unit_id: document.chunk.content_unit_id,
            unit_title: document.unit_title,
            document_revision: Revision::new(document.document_revision),
            unit_revision: Revision::new(document.unit_revision),
            text: document.chunk.body,
            locator,
            relevance: Some(relevance),
        },
    })
}

pub(crate) fn reciprocal_rank_fusion(
    keyword: Vec<RankedPassage>,
    semantic: Vec<RankedPassage>,
    limit: usize,
) -> Vec<PassageRecord> {
    struct FusionEntry {
        record: PassageRecord,
        score: f64,
        best_rank: usize,
    }

    let mut entries = HashMap::<String, FusionEntry>::new();
    for candidates in [keyword, semantic] {
        for (index, candidate) in candidates.into_iter().enumerate() {
            let rank = index + 1;
            let entry = entries
                .entry(candidate.record.passage_id.clone())
                .or_insert_with(|| FusionEntry {
                    record: candidate.record,
                    score: 0.0,
                    best_rank: rank,
                });
            entry.score += 1.0 / (RRF_K + rank as f64);
            entry.best_rank = entry.best_rank.min(rank);
        }
    }

    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.best_rank.cmp(&right.best_rank))
            .then_with(|| left.record.passage_id.cmp(&right.record.passage_id))
    });
    entries
        .into_iter()
        .take(limit)
        .map(|mut entry| {
            entry.record.relevance = Some(entry.score);
            entry.record
        })
        .collect()
}

pub(crate) fn normalize_scope(book_ids: Vec<String>) -> Result<Vec<String>> {
    if book_ids.len() > MAX_ALLOWED_BOOKS {
        bail!("book scope exceeds {MAX_ALLOWED_BOOKS} ids");
    }
    let mut seen = HashSet::with_capacity(book_ids.len());
    Ok(book_ids
        .into_iter()
        .filter(|book_id| !book_id.is_empty() && seen.insert(book_id.clone()))
        .collect())
}
