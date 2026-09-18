//! The SQLite-backed book backend and its outline projection.

pub(super) use super::*;

#[derive(Clone, Debug)]
pub struct SqliteBookBackend {
    pub(crate) db_path: PathBuf,
}

impl SqliteBookBackend {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub(super) fn read_passages_sync(
        db_path: &Path,
        request: ReadPassagesRequest,
    ) -> Result<Vec<PassageRecord>> {
        let allowed = request.book_ids.into_iter().collect::<HashSet<_>>();
        if allowed.is_empty() {
            return Ok(Vec::new());
        }
        let conn = db::open_conn(db_path)?;
        let mut passages = Vec::new();
        for passage_id in request.passage_ids {
            let Some(chunk) = db::search_chunks::get(&conn, &passage_id)? else {
                continue;
            };
            if !allowed.contains(&chunk.book_id) {
                continue;
            }
            let Some(book) = db::books::get(&conn, &chunk.book_id)? else {
                continue;
            };
            let Some(source) = db::book_sources::get_revision(&conn, &book.id, book.revision)?
            else {
                continue;
            };
            if source.id != chunk.source_id {
                continue;
            }
            let Some(unit) = db::content_units::get(&conn, &chunk.content_unit_id)? else {
                continue;
            };
            if unit.book_id != chunk.book_id || unit.source_id != chunk.source_id {
                continue;
            }
            let locator = serde_json::from_str::<DocumentLocator>(&chunk.locator_json)
                .context("search passage locator is invalid")?;
            if locator.book_id != chunk.book_id || locator.unit_id != chunk.content_unit_id {
                continue;
            }
            if matches!(
                locator.source.as_ref(),
                Some(SourceLocator::OfficeRenderedPage { .. })
            ) {
                continue;
            }
            passages.push(PassageRecord {
                passage_id: chunk.id,
                book_id: chunk.book_id,
                book_title: book.title,
                unit_id: chunk.content_unit_id,
                unit_title: unit.title.unwrap_or(chunk.heading),
                document_revision: Revision::new(book.revision),
                unit_revision: Revision::new(unit.revision),
                text: chunk.body,
                locator,
                relevance: None,
            });
        }
        Ok(passages)
    }

    pub(super) fn outlines_sync(
        db_path: &Path,
        request: OutlineRequest,
    ) -> Result<Vec<BookOutlineRecord>> {
        let conn = db::open_conn(db_path)?;
        let mut output = Vec::new();
        for book_id in request.book_ids.into_iter().take(request.max_books) {
            let Some(book) = db::books::get(&conn, &book_id)? else {
                continue;
            };
            let Some(source) = db::book_sources::get_revision(&conn, &book.id, book.revision)?
            else {
                continue;
            };
            let rows = db::toc_entries::list_for_source(&conn, &source.id)?;
            let mut children = HashMap::<Option<String>, Vec<db::toc_entries::TocEntry>>::new();
            for row in rows {
                children.entry(row.parent_id.clone()).or_default().push(row);
            }
            for rows in children.values_mut() {
                rows.sort_by_key(|row| row.ordinal);
            }
            let nodes = build_outline_nodes(
                &book.id,
                None,
                0,
                request.max_depth,
                &children,
                &mut HashSet::new(),
            )?;
            output.push(BookOutlineRecord {
                book_id: book.id,
                book_title: book.title,
                nodes,
            });
        }
        Ok(output)
    }
}

impl BookBackend for SqliteBookBackend {
    fn read_passages(
        &self,
        request: ReadPassagesRequest,
    ) -> BoxFuture<'_, Result<Vec<PassageRecord>>> {
        let db_path = self.db_path.clone();
        async move {
            tokio::task::spawn_blocking(move || Self::read_passages_sync(&db_path, request))
                .await
                .context("passage database worker stopped")?
        }
        .boxed()
    }

    fn get_outline(
        &self,
        request: OutlineRequest,
    ) -> BoxFuture<'_, Result<Vec<BookOutlineRecord>>> {
        let db_path = self.db_path.clone();
        async move {
            tokio::task::spawn_blocking(move || Self::outlines_sync(&db_path, request))
                .await
                .context("outline database worker stopped")?
        }
        .boxed()
    }
}

pub(crate) fn build_outline_nodes(
    book_id: &str,
    parent_id: Option<&str>,
    depth: usize,
    max_depth: usize,
    rows: &HashMap<Option<String>, Vec<db::toc_entries::TocEntry>>,
    visiting: &mut HashSet<String>,
) -> Result<Vec<OutlineNodeRecord>> {
    if depth >= max_depth {
        return Ok(Vec::new());
    }
    let key = parent_id.map(str::to_string);
    let Some(entries) = rows.get(&key) else {
        return Ok(Vec::new());
    };
    let mut nodes = Vec::with_capacity(entries.len());
    for entry in entries {
        if !visiting.insert(entry.id.clone()) {
            continue;
        }
        let locator = serde_json::from_str::<DocumentLocator>(&entry.locator_json)
            .context("table-of-contents locator is invalid")?;
        if locator.book_id != book_id || locator.unit_id != entry.content_unit_id {
            visiting.remove(&entry.id);
            continue;
        }
        let children = build_outline_nodes(
            book_id,
            Some(&entry.id),
            depth + 1,
            max_depth,
            rows,
            visiting,
        )?;
        visiting.remove(&entry.id);
        nodes.push(OutlineNodeRecord {
            title: entry.label.clone(),
            locator,
            children,
        });
    }
    Ok(nodes)
}
