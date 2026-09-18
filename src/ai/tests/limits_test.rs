//! Tests for the model and embedding response limits.

use super::super::*;

#[test]
fn embedding_count_dimension_and_numeric_limits_are_enforced() {
    let exact_count = WireEmbeddingResponse {
        model: "embed".into(),
        data: (0..MAX_EMBEDDING_COUNT)
            .map(|index| WireEmbedding {
                index,
                embedding: vec![0.0],
            })
            .collect(),
        usage: None,
    };
    assert_eq!(
        validate_embedding_response(exact_count, MAX_EMBEDDING_COUNT)
            .unwrap()
            .vectors
            .len(),
        MAX_EMBEDDING_COUNT
    );

    let over_count = WireEmbeddingResponse {
        model: "embed".into(),
        data: (0..=MAX_EMBEDDING_COUNT)
            .map(|index| WireEmbedding {
                index,
                embedding: vec![0.0],
            })
            .collect(),
        usage: None,
    };
    assert!(
        validate_embedding_response(over_count, MAX_EMBEDDING_COUNT + 1)
            .unwrap_err()
            .to_string()
            .contains("vector limit")
    );

    let exact_dimensions = WireEmbeddingResponse {
        model: "embed".into(),
        data: vec![WireEmbedding {
            index: 0,
            embedding: vec![0.0; MAX_EMBEDDING_DIMENSIONS],
        }],
        usage: None,
    };
    assert_eq!(
        validate_embedding_response(exact_dimensions, 1)
            .unwrap()
            .vectors[0]
            .len(),
        MAX_EMBEDDING_DIMENSIONS
    );

    let over_dimensions = WireEmbeddingResponse {
        model: "embed".into(),
        data: vec![WireEmbedding {
            index: 0,
            embedding: vec![0.0; MAX_EMBEDDING_DIMENSIONS + 1],
        }],
        usage: None,
    };
    assert!(
        validate_embedding_response(over_dimensions, 1)
            .unwrap_err()
            .to_string()
            .contains("dimensions")
    );

    let non_finite = WireEmbeddingResponse {
        model: "embed".into(),
        data: vec![WireEmbedding {
            index: 0,
            embedding: vec![f32::NAN],
        }],
        usage: None,
    };
    assert!(
        validate_embedding_response(non_finite, 1)
            .unwrap_err()
            .to_string()
            .contains("invalid or inconsistent")
    );
}
