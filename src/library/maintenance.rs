//! Startup maintenance: reclaiming unreferenced objects and backfilling the
//! declared language an importer failed to record.

use super::*;

impl LibraryStore {
    pub(crate) fn schedule_blob_deletions(&self, blobs: Vec<db::blobs::BlobRecord>) {
        if blobs.is_empty() {
            return;
        }
        let store = Arc::clone(&self.blob_store);
        let db_path = self.db_path.clone();
        let publication = self.blob_publication.clone();
        self.io.spawn(async move {
            reclaim_unreferenced_blobs(&db_path, &store, &publication, blobs, "对象").await;
        });
    }
}

/// Reclaims stale candidates without trusting the snapshot that produced
/// them. Every candidate is serialized with publication and re-checked in a

/// Reclaims stale candidates without trusting the snapshot that produced
/// them. Every candidate is serialized with publication and re-checked in a
/// fresh connection immediately before its bytes are removed.
pub(crate) async fn reclaim_unreferenced_blobs(
    db_path: &Path,
    store: &LocalBlobStore,
    publication: &BlobPublicationLock,
    blobs: Vec<db::blobs::BlobRecord>,
    label: &'static str,
) {
    for blob in blobs {
        let publication_guard = publication.acquire().await;
        reclaim_unreferenced_blob_while_locked(db_path, store, &blob, label).await;
        drop(publication_guard);
    }
}

pub(crate) async fn reclaim_unreferenced_blob_while_locked(
    db_path: &Path,
    store: &LocalBlobStore,
    blob: &db::blobs::BlobRecord,
    label: &'static str,
) {
    let object_key = blob.object_key.clone();
    let check_path = db_path.to_path_buf();
    let still_unreferenced = match tokio::task::spawn_blocking(move || {
        db::blobs::is_unreferenced(&db::open_conn(&check_path)?, &object_key)
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            tracing::warn!(object_key = %blob.object_key, %error, "{label}回收前无法复核引用，保留对象");
            return;
        }
        Err(error) => {
            tracing::warn!(object_key = %blob.object_key, %error, "{label}引用复核任务异常退出，保留对象");
            return;
        }
    };
    if !still_unreferenced {
        return;
    }

    let key = match BlobKey::parse(&blob.object_key) {
        Ok(key) => key,
        Err(error) => {
            tracing::error!(object_key = %blob.object_key, %error, "忽略无效的{label}待回收对象键");
            return;
        }
    };
    if let Err(error) = store.delete(&key).await {
        tracing::warn!(object_key = %blob.object_key, %error, "异步删除未引用{label}失败，启动 GC 将重试");
        return;
    }

    let cleanup_path = db_path.to_path_buf();
    let object_key = blob.object_key.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = db::open_conn(&cleanup_path)?;
        db::blobs::delete_if_unreferenced(&conn, &object_key)?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "{label}已删除，但清理对象元数据失败，启动 GC 将重试")
        }
        Err(error) => tracing::warn!(%error, "{label}对象元数据清理任务异常退出"),
    }
}

/// Startup is the only point where a global object sweep is safe: no import
/// can have published bytes while its SQLite transaction is still pending.
/// Registered-but-unreferenced rows are removed after their bytes, then
/// content-addressed files left by an interrupted transaction are collected.
pub(crate) fn run_startup_blob_gc(
    conn: &rusqlite::Connection,
    store: &LocalBlobStore,
    io: &IoRuntime,
) -> Result<()> {
    for blob in db::blobs::list_unreferenced(conn)? {
        // A malformed key is a row we cannot act on, not a reason to refuse to
        // open the library: warn and let the next startup retry, same policy as
        // the language backfill below.
        let key = match BlobKey::parse(&blob.object_key) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(
                    object_key = %blob.object_key,
                    %error,
                    "启动 GC 忽略无法解析的未引用对象键，将保留后重试"
                );
                continue;
            }
        };
        match io.block_on(store.delete(&key)) {
            Ok(()) => {
                db::blobs::delete(conn, &blob.object_key)?;
            }
            Err(error) => {
                tracing::warn!(object_key = %blob.object_key, %error, "启动 GC 无法删除未引用对象，将保留后重试");
            }
        }
    }

    let registered = db::blobs::list(conn)?
        .into_iter()
        .filter_map(|blob| match BlobKey::parse(&blob.object_key) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::warn!(
                    object_key = %blob.object_key,
                    %error,
                    "启动 GC 忽略无法解析的对象键，其对象不参与孤儿扫描"
                );
                None
            }
        })
        .collect::<Vec<_>>();
    if let Err(error) = io.block_on(store.delete_orphans(&registered)) {
        tracing::warn!(%error, "启动 GC 无法完成孤儿对象扫描，将在下次启动重试");
    }
    Ok(())
}

/// Durable one-shot marker for the language backfill. A per-book condition is
/// not enough: a container that declares no language stays blank forever, so
/// re-reading its blob on every startup would be real I/O for no result.
pub(crate) const LANGUAGE_BACKFILL_SETTING: &str = "library.language_backfill.v1";

/// One-shot repair for a book whose declared language an import dropped. EPUB
/// was the offender: its importer ignored the OPF's `dc:language`, so every EPUB
/// looked like an unknown-language book and the translation scheduler queued
/// even a Chinese book for Chinese translation.
///
/// Only container metadata is read — never the content — and a language is only
/// ever filled in, never replaced, so a book a newer importer or the reader
/// already described is left alone. `revision` and `updated_at` stay untouched,
/// which keeps derived indexes, translations, annotations and library ordering
/// exactly as they were.
pub(crate) fn run_startup_language_backfill(
    conn: &rusqlite::Connection,
    store: &LocalBlobStore,
    io: &IoRuntime,
) -> Result<()> {
    if db::settings::get(conn, LANGUAGE_BACKFILL_SETTING)?.is_some() {
        return Ok(());
    }
    // A blob that cannot be read or parsed keeps the marker unwritten so the
    // next startup retries, instead of silently giving up on that book. An empty
    // library likewise leaves it unwritten: there was nothing to examine, so a
    // book appearing later must still get its chance.
    let mut incomplete = false;
    let books = db::books::list(conn)?;
    for book in books.iter() {
        if book
            .language
            .as_deref()
            .is_some_and(|language| !language.trim().is_empty())
        {
            continue;
        }
        // Checked before any read: a format that cannot declare a language would
        // otherwise cost a whole original-file read for a guaranteed `None`.
        if !crate::formats::declares_language(&book.format) {
            continue;
        }
        // A malformed key is a repair failure, not a reason to refuse to open the
        // library: warn, keep the marker unwritten, and let the next start retry.
        let key = match BlobKey::parse(&book.source_object_key) {
            Ok(key) => key,
            Err(error) => {
                incomplete = true;
                tracing::warn!(
                    book_id = %book.id,
                    %error,
                    "启动语言回填：原件对象键无法解析，保留待下次重试"
                );
                continue;
            }
        };
        let language = match io.block_on(store.get(&key)) {
            Ok(bytes) => match crate::formats::declared_language(&book.format, &bytes) {
                Ok(Some(language)) => language,
                // The container declares nothing; there is nothing to repair.
                Ok(None) => continue,
                Err(error) => {
                    incomplete = true;
                    tracing::warn!(
                        book_id = %book.id,
                        format = %book.format,
                        %error,
                        "启动语言回填：无法读出声明语言，保留待下次重试"
                    );
                    continue;
                }
            },
            Err(error) => {
                incomplete = true;
                tracing::warn!(
                    book_id = %book.id,
                    %error,
                    "启动语言回填：无法读取原件，保留待下次重试"
                );
                continue;
            }
        };
        match db::books::backfill_language(conn, &book.id, &language) {
            Ok(1) => tracing::info!(
                book_id = %book.id,
                %language,
                "启动语言回填：补上导入器漏掉的声明语言"
            ),
            Ok(_) => {}
            Err(error) => {
                incomplete = true;
                tracing::warn!(book_id = %book.id, %error, "启动语言回填：写入失败");
            }
        }
    }
    if !incomplete && !books.is_empty() {
        db::settings::upsert(
            conn,
            &db::settings::Setting {
                key: LANGUAGE_BACKFILL_SETTING.to_string(),
                value_json: "true".to_string(),
                updated_at: now_secs(),
            },
        )?;
    }
    Ok(())
}
