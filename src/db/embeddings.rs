use std::mem::size_of;

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Embedding {
    pub(crate) id: String,
    pub(crate) search_chunk_id: String,
    pub(crate) model: String,
    pub(crate) dimensions: usize,
    pub(crate) vector: Vec<u8>,
    pub(crate) created_at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VectorMatch {
    pub(crate) search_chunk_id: String,
    pub(crate) distance: f64,
}

pub(crate) fn get(conn: &Connection, embedding_id: &str) -> Result<Option<Embedding>> {
    conn.query_row(
        "SELECT id, search_chunk_id, model, dimensions, vector, created_at
         FROM embeddings WHERE id = ?1",
        [embedding_id],
        embedding_from_row,
    )
    .optional()
    .context("无法读取向量")
}

pub(crate) fn get_for_chunk_model(
    conn: &Connection,
    chunk_id: &str,
    model: &str,
) -> Result<Option<Embedding>> {
    conn.query_row(
        "SELECT id, search_chunk_id, model, dimensions, vector, created_at
         FROM embeddings WHERE search_chunk_id = ?1 AND model = ?2",
        params![chunk_id, model],
        embedding_from_row,
    )
    .optional()
    .context("无法读取搜索分块向量")
}

pub(crate) fn upsert(conn: &Connection, embedding: &Embedding) -> Result<usize> {
    conn.execute(
        "INSERT INTO embeddings (id, search_chunk_id, model, dimensions, vector, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(search_chunk_id, model) DO UPDATE SET
             id = excluded.id,
             dimensions = excluded.dimensions,
             vector = excluded.vector,
             created_at = excluded.created_at",
        params![
            embedding.id,
            embedding.search_chunk_id,
            embedding.model,
            embedding.dimensions as i64,
            embedding.vector,
            embedding.created_at as i64,
        ],
    )
    .context("无法写入向量")
}

pub(crate) fn upsert_f32(
    conn: &Connection,
    embedding_id: &str,
    search_chunk_id: &str,
    model: &str,
    vector: &[f32],
    created_at: u64,
) -> Result<usize> {
    if model.trim().is_empty() {
        bail!("向量模型不能为空");
    }
    let vector = encode_f32(vector)?;
    upsert(
        conn,
        &Embedding {
            id: embedding_id.to_string(),
            search_chunk_id: search_chunk_id.to_string(),
            model: model.to_string(),
            dimensions: vector.len() / size_of::<f32>(),
            vector,
            created_at,
        },
    )
}

pub(crate) fn get_f32_for_chunk_model(
    conn: &Connection,
    chunk_id: &str,
    model: &str,
) -> Result<Option<(Embedding, Vec<f32>)>> {
    get_for_chunk_model(conn, chunk_id, model)?
        .map(|embedding| {
            let vector = decode_f32(&embedding.vector, embedding.dimensions)?;
            Ok((embedding, vector))
        })
        .transpose()
}

/// Performs an exact cosine-distance scan over every matching stored vector.
/// The book scope is part of the SQL predicate; an empty scope never means
/// "all books".
pub(crate) fn exact_knn_scoped(
    conn: &Connection,
    model: &str,
    query_vector: &[f32],
    allowed_book_ids: &[String],
    limit: usize,
) -> Result<Vec<VectorMatch>> {
    exact_knn_scoped_for_execution(conn, model, None, query_vector, allowed_book_ids, limit)
}

/// The source's canonical job and vectors are read by one SQLite statement.
/// Reconfiguration replaces that cursor and invalidates vectors in one
/// transaction, so an old provider snapshot cannot search a new vector space.
pub(crate) fn exact_knn_scoped_for_execution(
    conn: &Connection,
    model: &str,
    execution_identity: Option<&str>,
    query_vector: &[f32],
    allowed_book_ids: &[String],
    limit: usize,
) -> Result<Vec<VectorMatch>> {
    if allowed_book_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    if model.trim().is_empty() {
        bail!("向量模型不能为空");
    }
    let query_vector = encode_f32(query_vector)?;
    let dimensions = query_vector.len() / size_of::<f32>();

    let mut values = vec![
        Value::Text(model.to_string()),
        Value::Integer(dimensions as i64),
        Value::Blob(query_vector),
    ];
    let execution_filter = if let Some(identity) = execution_identity {
        if identity.trim().is_empty() {
            bail!("向量执行身份不能为空");
        }
        values.push(Value::Text(identity.to_string()));
        format!(
            "AND EXISTS (
                 SELECT 1 FROM index_jobs j
                 WHERE j.id = 'embedding:' || c.source_id
                   AND j.kind = 'embedding'
                   AND j.source_id = c.source_id
                   AND j.book_id = c.book_id
                   AND CASE WHEN json_valid(j.cursor_json)
                            THEN json_extract(j.cursor_json, '$.model') = ?1
                             AND json_extract(j.cursor_json, '$.execution_identity') = ?{}
                            ELSE 0
                       END
             )",
            values.len()
        )
    } else {
        String::new()
    };
    let scope_parameters = allowed_book_ids
        .iter()
        .map(|book_id| {
            values.push(Value::Text(book_id.clone()));
            format!("?{}", values.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    values.push(Value::Integer(limit as i64));
    let limit_parameter = values.len();
    let sql = format!(
        "SELECT e.search_chunk_id,
                vec_distance_cosine(vec_f32(e.vector), vec_f32(?3)) AS distance
         FROM embeddings e
         JOIN search_chunks c ON c.id = e.search_chunk_id
         WHERE e.model = ?1
           AND e.dimensions = ?2
           AND c.book_id IN ({scope_parameters})
           {execution_filter}
           AND CASE WHEN json_valid(c.locator_json)
                    THEN COALESCE(json_extract(c.locator_json, '$.source.type'), '')
                         <> 'office_rendered_page'
                    ELSE 0
               END
         ORDER BY distance ASC, e.search_chunk_id ASC
         LIMIT ?{limit_parameter}"
    );
    let mut statement = conn.prepare(&sql).context("无法准备向量精确搜索")?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(VectorMatch {
                search_chunk_id: row.get(0)?,
                distance: row.get(1)?,
            })
        })
        .context("向量精确搜索失败")?;
    let matches = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("无法读取向量精确搜索结果")?;
    if matches.iter().any(|hit| !hit.distance.is_finite()) {
        bail!("向量精确搜索返回了非有限距离");
    }
    Ok(matches)
}

pub(crate) fn delete_for_chunk(conn: &Connection, chunk_id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM embeddings WHERE search_chunk_id = ?1",
        [chunk_id],
    )
    .context("无法删除搜索分块向量")
}

pub(crate) fn delete_for_source_model(
    conn: &Connection,
    source_id: &str,
    model: &str,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM embeddings
         WHERE model = ?2
           AND search_chunk_id IN (
               SELECT id FROM search_chunks WHERE source_id = ?1
           )",
        params![source_id, model],
    )
    .context("无法清除来源的过期模型向量")
}

fn embedding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Embedding> {
    Ok(Embedding {
        id: row.get(0)?,
        search_chunk_id: row.get(1)?,
        model: row.get(2)?,
        dimensions: row.get::<_, i64>(3)? as usize,
        vector: row.get(4)?,
        created_at: row.get::<_, i64>(5)? as u64,
    })
}

pub(crate) fn encode_f32(vector: &[f32]) -> Result<Vec<u8>> {
    if vector.is_empty() {
        bail!("向量不能为空");
    }
    if vector.iter().any(|value| !value.is_finite()) {
        bail!("向量包含非有限数值");
    }
    if vector.iter().all(|value| *value == 0.0) {
        bail!("余弦搜索不接受零向量");
    }

    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    Ok(bytes)
}

pub(crate) fn decode_f32(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>> {
    let expected_bytes = dimensions
        .checked_mul(size_of::<f32>())
        .context("向量维度过大")?;
    if dimensions == 0 || bytes.len() != expected_bytes {
        bail!(
            "向量字节长度与维度不匹配：期望 {expected_bytes}，实际 {}",
            bytes.len()
        );
    }
    let vector = bytes
        .chunks_exact(size_of::<f32>())
        .map(|chunk| f32::from_ne_bytes(chunk.try_into().expect("f32 chunk size is exact")))
        .collect::<Vec<_>>();
    if vector.iter().any(|value| !value.is_finite()) {
        bail!("存储向量包含非有限数值");
    }
    Ok(vector)
}
