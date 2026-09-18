//! Validating the models and embedding responses before they are used.

use super::*;

pub(crate) fn validate_and_normalize_models(mut models: Vec<ModelInfo>) -> Result<Vec<ModelInfo>> {
    ensure!(
        models.len() <= MAX_MODEL_ENTRIES,
        "AI models response exceeds the {MAX_MODEL_ENTRIES}-entry limit"
    );
    ensure!(
        models.iter().all(|model| {
            !model.id.trim().is_empty()
                && model.id.len() <= MAX_MODEL_FIELD_BYTES
                && model
                    .owned_by
                    .as_ref()
                    .is_none_or(|owner| owner.len() <= MAX_MODEL_FIELD_BYTES)
        }),
        "AI models response contains an empty or oversized model field"
    );
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

pub(crate) fn validate_embedding_response(
    mut body: WireEmbeddingResponse,
    expected: usize,
) -> Result<EmbeddingBatch> {
    ensure!(
        body.data.len() <= MAX_EMBEDDING_COUNT,
        "embedding response exceeds the {MAX_EMBEDDING_COUNT}-vector limit"
    );
    body.data.sort_by_key(|entry| entry.index);
    ensure!(
        body.data.len() == expected
            && body
                .data
                .iter()
                .enumerate()
                .all(|(index, entry)| index == entry.index),
        "embedding response indices do not match the request"
    );
    let dimensions = body
        .data
        .first()
        .map(|entry| entry.embedding.len())
        .unwrap_or(0);
    ensure!(
        dimensions != 0 && dimensions <= MAX_EMBEDDING_DIMENSIONS,
        "embedding response dimensions must be between 1 and {MAX_EMBEDDING_DIMENSIONS}"
    );
    ensure!(
        body.data.iter().all(|entry| {
            entry.embedding.len() == dimensions
                && entry.embedding.iter().all(|value| value.is_finite())
        }),
        "embedding response contains invalid or inconsistent vectors"
    );
    Ok(EmbeddingBatch {
        model: body.model,
        vectors: body.data.into_iter().map(|entry| entry.embedding).collect(),
        usage: body.usage,
    })
}
