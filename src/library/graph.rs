//! Building and reading the persisted document graph.

use super::*;

pub(crate) struct PersistedGraph {
    pub(crate) book: db::books::BookRecord,
    pub(crate) source_blob: db::blobs::BlobRecord,
    pub(crate) additional_blobs: Vec<db::blobs::BlobRecord>,
    pub(crate) source: db::book_sources::BookSource,
    pub(crate) progress: db::progress::ReadingProgress,
    pub(crate) units: Vec<db::content_units::ContentUnit>,
    pub(crate) toc: Vec<db::toc_entries::TocEntry>,
    pub(crate) assets: Vec<db::assets::Asset>,
    pub(crate) asset_refs: Vec<db::asset_refs::AssetRef>,
    pub(crate) search_chunks: Vec<db::search_chunks::SearchChunk>,
    pub(crate) visual_pages: Vec<db::visual_pages::VisualPage>,
}

impl PersistedGraph {
    pub(crate) fn borrow(&self) -> db::transactions::DocumentGraph<'_> {
        db::transactions::DocumentGraph {
            book: &self.book,
            source_blob: &self.source_blob,
            additional_blobs: &self.additional_blobs,
            source: &self.source,
            progress: Some(&self.progress),
            content_units: &self.units,
            toc_entries: &self.toc,
            assets: &self.assets,
            asset_refs: &self.asset_refs,
            search_chunks: &self.search_chunks,
            visual_pages: &self.visual_pages,
        }
    }
}

pub(crate) fn build_persisted_graph(
    imported: ImportedBook,
    source_key: BlobKey,
    source_byte_len: u64,
    source_media_type: &str,
    source_name: Option<String>,
    source_kind: &str,
    asset_keys: HashMap<String, BlobKey>,
    previous: Option<&BookRecord>,
    previous_progress: Option<&db::progress::ReadingProgress>,
) -> Result<PersistedGraph> {
    let now = now_secs();
    let document = imported.document;
    let revision = previous
        .map(|record| record.revision + 1)
        .unwrap_or(document.revision.get());
    let source_id = deterministic_id(
        "source",
        format!("{}\0{revision}\0{source_key}", document.id).as_bytes(),
    );
    let mut blobs_by_key = HashMap::<String, db::blobs::BlobRecord>::new();
    let mut assets = Vec::new();
    for payload in &imported.assets {
        let key = asset_keys
            .get(&payload.metadata.id)
            .context("导入资源缺少对象键")?;
        blobs_by_key
            .entry(key.to_string())
            .or_insert_with(|| db::blobs::BlobRecord {
                object_key: key.to_string(),
                media_type: payload.metadata.media_type.clone(),
                byte_len: payload.metadata.byte_len,
                hash: payload.metadata.content_hash.clone(),
                created_at: now,
            });
        assets.push(db::assets::Asset {
            id: payload.metadata.id.clone(),
            book_id: document.id.clone(),
            source_id: source_id.clone(),
            object_key: key.to_string(),
            kind: asset_kind(&payload.metadata.roles).to_string(),
            href: format!("asset/{}", payload.metadata.id),
            media_type: payload.metadata.media_type.clone(),
            byte_len: payload.metadata.byte_len,
            width: None,
            height: None,
            created_at: now,
        });
    }
    let source_hash = source_key
        .as_str()
        .rsplit('/')
        .next()
        .context("无效的来源对象键")?
        .to_string();
    let source_blob = db::blobs::BlobRecord {
        object_key: source_key.to_string(),
        media_type: source_media_type.to_string(),
        byte_len: source_byte_len,
        hash: source_hash,
        created_at: now,
    };
    // The graph needs the exact source length; initial imports also have an
    // original asset with that key, so use its verified metadata when present.
    let source_blob = blobs_by_key
        .get(source_key.as_str())
        .cloned()
        .unwrap_or(source_blob);
    blobs_by_key.remove(source_key.as_str());

    let units = document
        .units
        .iter()
        .enumerate()
        .map(|(ordinal, unit)| {
            // An update publishes the per-chapter revisions its caller assigned.
            // A fresh import has no earlier chapter and starts every unit at the
            // document revision.
            let unit_revision = if previous.is_some() {
                unit.revision.get()
            } else {
                revision
            };
            db_unit(&document.id, &source_id, ordinal, unit, unit_revision, now)
        })
        .collect::<Result<Vec<_>>>()?;
    let href_by_unit = units
        .iter()
        .map(|unit| (unit.id.as_str(), unit.href.clone()))
        .collect::<HashMap<_, _>>();
    let mut toc = Vec::new();
    flatten_toc(
        &document.id,
        &source_id,
        &document.toc,
        None,
        0,
        &href_by_unit,
        now,
        &mut toc,
    )?;
    let mut asset_refs = Vec::new();
    for unit in &document.units {
        for (ordinal, asset_id) in unit.document.referenced_asset_ids().into_iter().enumerate() {
            let locator = DocumentLocator::unit(&document.id, &unit.id);
            asset_refs.push(db::asset_refs::AssetRef {
                id: deterministic_id(
                    "asset-ref",
                    format!("{}\0{asset_id}\0{ordinal}", unit.id).as_bytes(),
                ),
                content_unit_id: unit.id.clone(),
                asset_id: asset_id.to_string(),
                relation: "content".to_string(),
                ordinal,
                locator_json: serde_json::to_string(&locator)?,
                created_at: now,
            });
        }
    }
    let search_chunks = build_search_chunks(&document, &source_id, now)?;
    let cover = document
        .cover_asset_id
        .as_deref()
        .and_then(|id| assets.iter().find(|asset| asset.id == id));
    let format = if source_kind == "normalized" {
        "epub"
    } else {
        document_format(&document)
    }
    .to_string();
    let author = if document.authors.is_empty() {
        "未知作者".to_string()
    } else {
        document.authors.join("、")
    };
    let fallback_spine = previous
        .map(|record| record.last_spine)
        .unwrap_or(0)
        .min(units.len().saturating_sub(1));
    let progress_spine = previous_progress
        .and_then(|progress| progress.content_unit_id.as_deref())
        .and_then(|unit_id| units.iter().position(|unit| unit.id == unit_id))
        .unwrap_or(fallback_spine);
    let book = db::books::BookRecord {
        id: document.id.clone(),
        title: document.title.clone(),
        author,
        language: document.language.clone(),
        description: document.description.clone(),
        format: format.clone(),
        revision,
        source_object_key: source_key.to_string(),
        cover_asset_id: cover.map(|asset| asset.id.clone()),
        cover_object_key: cover.map(|asset| asset.object_key.clone()),
        cover_mime: cover.map(|asset| asset.media_type.clone()),
        added_at: previous.map(|record| record.added_at).unwrap_or(now),
        updated_at: now,
        last_spine: progress_spine,
        group_id: previous.and_then(|record| record.group_id.clone()),
    };
    let source = db::book_sources::BookSource {
        id: source_id,
        book_id: document.id,
        revision,
        format,
        source_kind: source_kind.to_string(),
        object_key: source_key.to_string(),
        source_name,
        created_at: now,
    };
    let progress = db::progress::ReadingProgress {
        book_id: book.id.clone(),
        source_revision: revision,
        content_unit_id: units.get(progress_spine).map(|unit| unit.id.clone()),
        spine_index: progress_spine,
        locator_json: units
            .get(progress_spine)
            .map(|unit| serde_json::to_string(&DocumentLocator::unit(&book.id, &unit.id)))
            .transpose()?
            .unwrap_or_else(|| "{}".to_string()),
        fraction: previous_progress
            .map(|progress| progress.fraction)
            .unwrap_or(0.0),
        updated_at: previous_progress
            .map(|progress| progress.updated_at)
            .unwrap_or(now),
    };
    Ok(PersistedGraph {
        book,
        source_blob,
        additional_blobs: blobs_by_key.into_values().collect(),
        source,
        progress,
        units,
        toc,
        assets,
        asset_refs,
        search_chunks,
        visual_pages: Vec::new(),
    })
}

pub(crate) fn db_unit(
    book_id: &str,
    source_id: &str,
    ordinal: usize,
    unit: &DocumentUnit,
    revision: u64,
    now: u64,
) -> Result<db::content_units::ContentUnit> {
    Ok(db::content_units::ContentUnit {
        id: unit.id.clone(),
        book_id: book_id.to_string(),
        source_id: source_id.to_string(),
        parent_id: None,
        ordinal,
        kind: unit_kind(unit.kind).to_string(),
        href: unit_href(unit),
        source_locator_json: serde_json::to_string(&unit.source_locator)?,
        title: Some(unit.title.clone()),
        media_type: Some("text/html".to_string()),
        source_text: Some(unit.source.clone()),
        block_json: serde_json::to_string(&unit.document)?,
        revision,
        created_at: now,
        updated_at: now,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn flatten_toc(
    book_id: &str,
    source_id: &str,
    nodes: &[TocNode],
    parent_id: Option<&str>,
    depth: usize,
    href_by_unit: &HashMap<&str, Option<String>>,
    now: u64,
    output: &mut Vec<db::toc_entries::TocEntry>,
) -> Result<()> {
    for node in nodes {
        let ordinal = output.len();
        let unit_id = node.target.unit_id().to_string();
        let block_id = match &node.target {
            TocTarget::Unit { .. } => None,
            TocTarget::Block { block_id, .. } => Some(block_id.clone()),
        };
        let locator = match block_id.as_deref() {
            Some(block) => DocumentLocator::block(book_id, &unit_id, block),
            None => DocumentLocator::unit(book_id, &unit_id),
        };
        output.push(db::toc_entries::TocEntry {
            id: node.id.clone(),
            book_id: book_id.to_string(),
            source_id: source_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            ordinal,
            depth,
            label: node.label.clone(),
            href: href_by_unit.get(unit_id.as_str()).cloned().flatten(),
            content_unit_id: unit_id,
            target_block_id: block_id,
            locator_json: serde_json::to_string(&locator)?,
            created_at: now,
        });
        flatten_toc(
            book_id,
            source_id,
            &node.children,
            Some(&node.id),
            depth + 1,
            href_by_unit,
            now,
            output,
        )?;
    }
    Ok(())
}

pub(crate) fn document_unit_from_row(row: db::content_units::ContentUnit) -> Result<DocumentUnit> {
    Ok(DocumentUnit {
        id: row.id,
        revision: Revision::new(row.revision),
        kind: parse_unit_kind(&row.kind)?,
        title: row.title.unwrap_or_else(|| "未命名单元".to_string()),
        source_locator: serde_json::from_str(&row.source_locator_json)?,
        source: row.source_text.unwrap_or_default(),
        document: serde_json::from_str(&row.block_json)?,
    })
}

pub(crate) fn restore_toc(rows: Vec<db::toc_entries::TocEntry>) -> Vec<TocNode> {
    pub(crate) fn children(
        parent: Option<&str>,
        rows: &[db::toc_entries::TocEntry],
    ) -> Vec<TocNode> {
        rows.iter()
            .filter(|row| row.parent_id.as_deref() == parent)
            .map(|row| {
                let target = row
                    .target_block_id
                    .as_ref()
                    .map(|block| TocTarget::block(&row.content_unit_id, block))
                    .unwrap_or_else(|| TocTarget::unit(&row.content_unit_id));
                let mut node = TocNode::new(&row.id, &row.label, target);
                node.children = children(Some(&row.id), rows);
                node
            })
            .collect()
    }
    children(None, &rows)
}
