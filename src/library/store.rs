//! The store handle: construction, projections and the shelf and progress
//! state it keeps in memory.

use super::*;

impl std::fmt::Debug for LibraryStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LibraryStore")
            .field("data_dir", &self.data_dir)
            .field("db_path", &self.db_path)
            .field("book_count", &self.books.len())
            .field("group_count", &self.groups.len())
            .finish()
    }
}

impl LibraryStore {
    pub fn default_data_dir() -> Result<PathBuf> {
        let project_dirs =
            ProjectDirs::from("dev", "ngy", "ngy_book_studio").context("无法确定应用数据目录")?;
        Ok(project_dirs.data_local_dir().to_path_buf())
    }

    pub fn load() -> Result<Self> {
        Self::load_from(Self::default_data_dir()?)
    }

    pub fn load_from(data_dir: PathBuf) -> Result<Self> {
        Self::load_from_with_runtime(data_dir, IoRuntime::default())
    }

    /// Opens a library using the process service runtime. AppServices uses
    /// this constructor so storage, parsing, indexing and AI do not each own a
    /// redundant Tokio worker pool.
    pub fn load_from_with_runtime(data_dir: PathBuf, io: IoRuntime) -> Result<Self> {
        let data_dir = absolute_path(data_dir)?;
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("无法创建数据目录：{}", data_dir.display()))?;
        let db_path = data_dir.join(db::DATABASE_FILE);
        let opened = db::open_or_recreate_with_state(&db_path)?;
        let object_root = data_dir.join(OBJECT_DIRECTORY);
        if opened.state != db::DatabaseOpenState::Current && object_root.exists() {
            ensure!(
                object_root.parent() == Some(data_dir.as_path()),
                "拒绝清理数据目录之外的对象存储"
            );
            if let Err(error) = fs::remove_dir_all(&object_root) {
                let cleanup_error = anyhow::Error::new(error)
                    .context(format!("无法清理旧对象目录：{}", object_root.display()));
                if let Err(rollback_error) =
                    db::discard_opened_database(opened.connection, &db_path)
                {
                    return Err(rollback_error).context(format!(
                        "对象目录重置失败（{cleanup_error:#}），且无法移除本次创建的空数据库"
                    ));
                }
                return Err(cleanup_error);
            }
        }
        let blob_store = Arc::new(LocalBlobStore::new(&object_root)?);
        let blob_publication = BlobPublicationLock::for_store(&blob_store)?;
        let startup_publication_guard = io.block_on(blob_publication.acquire());
        run_startup_blob_gc(&opened.connection, &blob_store, &io)?;
        // Must run before the in-memory projection is loaded below so a repaired
        // language is already part of the records every caller sees.
        run_startup_language_backfill(&opened.connection, &blob_store, &io)?;
        drop(startup_publication_guard);
        let books = db::books::list(&opened.connection)?;
        let groups = db::groups::list(&opened.connection)?;
        drop(opened.connection);

        let progress_incarnations = books
            .iter()
            .map(|book| (book.id.clone(), next_progress_incarnation()))
            .collect();
        let mut library = Self {
            data_dir,
            db_path,
            blob_store,
            blob_publication,
            io,
            registry: Arc::new(FormatRegistry::with_builtin_importers()),
            auto_run_background_jobs: Arc::new(AtomicBool::new(
                crate::services::DEFAULT_AUTO_RUN_BACKGROUND_JOBS,
            )),
            books,
            groups,
            cover_cache: HashMap::new(),
            progress_write_sequences: HashMap::new(),
            progress_incarnations,
            #[cfg(test)]
            fail_next_cover_cache_hydration: false,
        };
        let cover_ids = library
            .books
            .iter()
            .filter(|book| book.cover_object_key.is_some())
            .map(|book| book.id.clone())
            .collect::<Vec<_>>();
        for book_id in cover_ids {
            if let Err(error) = library.reload_cover(&book_id) {
                tracing::warn!(%book_id, %error, "忽略无法读取的封面对象");
            }
        }
        Ok(library)
    }

    pub fn books(&self) -> &[BookRecord] {
        &self.books
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn blob_store(&self) -> Arc<LocalBlobStore> {
        self.blob_store.clone()
    }

    /// Shared process-level switch consulted when a document revision commits.
    /// AppServices owns the user-facing persisted setting and updates this exact
    /// flag so cloned LibraryStore projections observe the latest value.
    pub(crate) fn background_job_auto_run_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.auto_run_background_jobs)
    }

    pub(crate) fn blob_publication_lock(&self) -> BlobPublicationLock {
        self.blob_publication.clone()
    }

    pub fn io_runtime(&self) -> IoRuntime {
        self.io.clone()
    }

    pub fn format_registry(&self) -> Arc<FormatRegistry> {
        self.registry.clone()
    }

    pub fn groups(&self) -> &[BookGroup] {
        &self.groups
    }

    pub fn book_record(&self, book_id: &str) -> Result<BookRecord> {
        let conn = db::open_conn(&self.db_path)?;
        db::books::get(&conn, book_id)?.with_context(|| format!("图书不存在：{book_id}"))
    }

    pub fn progress_incarnation(&self, book_id: &str) -> Option<u64> {
        self.progress_incarnations.get(book_id).copied()
    }

    pub fn refresh_book(&mut self, book_id: &str) -> Result<BookRecord> {
        let record = self.book_record(book_id)?;
        if let Some(position) = self.books.iter().position(|book| book.id == book_id) {
            self.books[position] = record.clone();
        } else {
            self.books.insert(0, record.clone());
            self.progress_incarnations
                .insert(book_id.to_string(), next_progress_incarnation());
        }
        self.reload_cover(book_id)?;
        Ok(record)
    }

    /// Applies a book/cover projection already loaded by a background service
    /// operation. This method performs no filesystem or SQLite work and is
    /// therefore safe to use when a GPUI completion callback updates its local
    /// view model. A delayed completion cannot replace a newer revision.
    pub fn apply_cached_book_update(
        &mut self,
        record: BookRecord,
        cover: Option<Arc<Vec<u8>>>,
    ) -> bool {
        let position = self.books.iter().position(|book| book.id == record.id);
        if position.is_some_and(|position| {
            let current = &self.books[position];
            current.revision > record.revision
                || (current.revision == record.revision && current.updated_at > record.updated_at)
        }) {
            return false;
        }
        let book_id = record.id.clone();
        if let Some(position) = position {
            self.books[position] = record;
        } else {
            self.books.insert(0, record);
            self.progress_incarnations
                .insert(book_id.clone(), next_progress_incarnation());
        }
        match cover {
            Some(bytes) => {
                self.cover_cache.insert(book_id, bytes);
            }
            None => {
                self.cover_cache.remove(&book_id);
            }
        }
        true
    }

    /// Merges a service-owned projection into this window-local clone without
    /// allowing an older full snapshot to roll back a newer editor result.
    /// Group membership and removals come from the authoritative snapshot;
    /// records already advanced to a later revision remain intact.
    pub fn merge_cached_projection(&mut self, snapshot: LibraryStore) {
        debug_assert_eq!(self.db_path, snapshot.db_path);

        let mut local_books = std::mem::take(&mut self.books)
            .into_iter()
            .map(|book| (book.id.clone(), book))
            .collect::<HashMap<_, _>>();
        let local_covers = std::mem::take(&mut self.cover_cache);
        let mut next_covers = HashMap::new();
        let mut next_books = Vec::with_capacity(snapshot.books.len());

        for incoming in snapshot.books {
            let book_id = incoming.id.clone();
            let local = local_books.remove(&book_id);
            let keep_local = local.as_ref().is_some_and(|current| {
                current.revision > incoming.revision
                    || (current.revision == incoming.revision
                        && current.updated_at > incoming.updated_at)
            });
            if keep_local {
                if let Some(cover) = local_covers.get(&book_id) {
                    next_covers.insert(book_id, Arc::clone(cover));
                }
                let mut local = local.expect("local book was checked above");
                // These fields are independent of the document revision. A
                // stale editor result may legitimately be newer in content
                // while the service snapshot is newer in shelf/progress state.
                local.group_id = incoming.group_id.clone();
                local.last_spine = incoming.last_spine;
                next_books.push(local);
            } else {
                if let Some(cover) = snapshot.cover_cache.get(&book_id) {
                    next_covers.insert(book_id, Arc::clone(cover));
                }
                next_books.push(incoming);
            }
        }

        self.books = next_books;
        self.groups = snapshot.groups;
        self.cover_cache = next_covers;
        self.progress_incarnations = snapshot.progress_incarnations;
        self.progress_write_sequences = snapshot.progress_write_sequences;
    }

    pub fn group(&self, id: &str) -> Option<&BookGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    pub fn child_groups(&self, parent_id: Option<&str>) -> Vec<&BookGroup> {
        self.groups
            .iter()
            .filter(|group| group.parent_id.as_deref() == parent_id)
            .collect()
    }

    pub fn group_path(&self, id: &str) -> Vec<BookGroup> {
        let mut path = Vec::new();
        let mut current = self.group(id).cloned();
        while let Some(group) = current {
            current = group
                .parent_id
                .as_deref()
                .and_then(|parent_id| self.group(parent_id).cloned());
            path.push(group);
            if path.len() > self.groups.len() {
                break;
            }
        }
        path.reverse();
        path
    }

    pub fn group_subtree_ids(&self, id: &str) -> Vec<String> {
        let mut ids = vec![id.to_string()];
        let mut pending = vec![id.to_string()];
        while let Some(current) = pending.pop() {
            for child in self.child_groups(Some(&current)) {
                ids.push(child.id.clone());
                pending.push(child.id.clone());
            }
        }
        ids
    }

    pub fn group_book_count(&self, group_id: &str) -> usize {
        self.books
            .iter()
            .filter(|book| book.group_id.as_deref() == Some(group_id))
            .count()
    }

    pub fn group_subtree_book_count(&self, group_id: &str) -> usize {
        let subtree = self.group_subtree_ids(group_id);
        let ids = subtree.iter().map(String::as_str).collect::<HashSet<_>>();
        self.books
            .iter()
            .filter(|book| book.group_id.as_deref().is_some_and(|id| ids.contains(id)))
            .count()
    }

    pub fn ungrouped_book_count(&self) -> usize {
        self.books
            .iter()
            .filter(|book| book.group_id.is_none())
            .count()
    }

    pub fn remove_book(&mut self, book_id: &str) -> Result<()> {
        let position = self
            .books
            .iter()
            .position(|book| book.id == book_id)
            .context("图书不存在")?;
        let mut conn = db::open_conn(&self.db_path)?;
        let unreferenced = db::transactions::delete_document(&mut conn, book_id)?;
        self.books.remove(position);
        self.cover_cache.remove(book_id);
        self.progress_incarnations.remove(book_id);
        self.progress_write_sequences
            .retain(|(candidate_id, _), _| candidate_id != book_id);
        self.schedule_blob_deletions(unreferenced);
        Ok(())
    }

    pub(crate) fn progress_action_is_stale(
        &self,
        id: &str,
        ordered_write: Option<(u64, u64)>,
    ) -> bool {
        let Some((incarnation, sequence)) = ordered_write else {
            return false;
        };
        self.progress_write_sequences
            .get(&(id.to_string(), incarnation))
            .is_some_and(|current| sequence <= *current)
    }

    pub(crate) fn accept_progress_action(&mut self, id: &str, ordered_write: Option<(u64, u64)>) {
        if let Some((incarnation, sequence)) = ordered_write {
            self.progress_write_sequences
                .insert((id.to_string(), incarnation), sequence);
        }
    }

    /// Persists progress against a stable canonical content unit. Fixed-layout
    /// readers use this path so a stored PDF page can be resolved through the
    /// unit's `SourceLocator::PdfPage` instead of relying on an ordinal alone.
    pub fn update_progress_at(
        &mut self,
        id: &str,
        last_spine: usize,
        content_unit_id: &str,
    ) -> Result<()> {
        self.update_progress_at_inner(id, last_spine, content_unit_id, None)
            .map(|_| ())
    }

    /// Ordered stable-locator variant used by concurrent Reader windows.
    pub fn update_progress_at_ordered(
        &mut self,
        id: &str,
        last_spine: usize,
        content_unit_id: &str,
        book_incarnation: u64,
        write_sequence: u64,
    ) -> Result<bool> {
        self.update_progress_at_inner(
            id,
            last_spine,
            content_unit_id,
            Some((book_incarnation, write_sequence)),
        )
    }

    pub(crate) fn update_progress_at_inner(
        &mut self,
        id: &str,
        _last_spine: usize,
        content_unit_id: &str,
        ordered_write: Option<(u64, u64)>,
    ) -> Result<bool> {
        if self.progress_action_is_stale(id, ordered_write) {
            return Ok(false);
        }
        if ordered_write
            .is_some_and(|(incarnation, _)| self.progress_incarnation(id) != Some(incarnation))
        {
            self.accept_progress_action(id, ordered_write);
            return Ok(false);
        }
        let Some(position) = self.books.iter().position(|book| book.id == id) else {
            self.accept_progress_action(id, ordered_write);
            return Ok(false);
        };
        let locator = DocumentLocator::unit(id, content_unit_id);
        let locator_json = serde_json::to_string(&locator)?;
        let mut conn = db::open_conn(&self.db_path)?;
        match db::transactions::update_current_unit_progress(
            &mut conn,
            id,
            content_unit_id,
            &locator_json,
            now_secs(),
        )? {
            db::transactions::CurrentProgressWrite::Updated {
                source_revision,
                spine_index,
            } => {
                self.accept_progress_action(id, ordered_write);
                // The caller's ordinal belongs to its opening snapshot. The
                // transaction resolves the stable unit in the current source.
                self.books[position].last_spine = spine_index;
                self.books[position].revision = source_revision;
                Ok(true)
            }
            db::transactions::CurrentProgressWrite::MissingBook if ordered_write.is_some() => {
                self.accept_progress_action(id, ordered_write);
                Ok(false)
            }
            db::transactions::CurrentProgressWrite::MissingBook => bail!("图书不存在"),
            db::transactions::CurrentProgressWrite::MissingUnit
            | db::transactions::CurrentProgressWrite::UnitNotCurrent
                if ordered_write.is_some() =>
            {
                // A Reader may finish closing after an editor has removed its
                // unit. Consume this obsolete action without replacing the
                // current progress or trapping the window in a retry loop.
                self.accept_progress_action(id, ordered_write);
                Ok(false)
            }
            db::transactions::CurrentProgressWrite::MissingUnit => bail!("阅读位置不存在"),
            db::transactions::CurrentProgressWrite::UnitNotCurrent => {
                bail!("阅读位置不属于指定图书的当前版本")
            }
        }
    }

    pub fn create_group(&mut self, name: &str, parent_id: Option<&str>) -> Result<BookGroup> {
        let name = validate_group_name(name)?;
        let group = BookGroup {
            id: self.new_group_id(&name, parent_id),
            name,
            parent_id: parent_id.map(str::to_owned),
            created_at: now_secs(),
        };
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::create_group(&mut conn, &group, MAX_GROUP_DEPTH)?;
        self.groups.push(group.clone());
        Ok(group)
    }

    pub fn rename_group(&mut self, id: &str, name: &str) -> Result<()> {
        let name = validate_group_name(name)?;
        let position = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .context("分组不存在")?;
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::rename_group(&mut conn, id, &name)?;
        self.groups[position].name = name;
        Ok(())
    }

    /// Renames one book. The title is stored in the canonical document as well
    /// as in the catalog row, so the rename publishes a new revision through
    /// the normal save path instead of writing `books.title` behind the
    /// document's back. A chapter whose body did not change keeps its own
    /// revision, and therefore its notes and its whole-book translation.
    pub fn rename_book(&mut self, book_id: &str, title: &str) -> Result<BookRecord> {
        let title = title.trim();
        ensure!(!title.is_empty(), "图书名称不能为空");
        let mut document = self.document(book_id)?;
        document.title = title.to_string();
        document.validate()?;
        self.apply_document(document)
    }

    /// Replaces a book's authors. An empty value clears them, which the catalog
    /// then reports as its placeholder; several authors written with `、`,
    /// `,` or `;` are split exactly like the importer splits them, so the value
    /// read back from the catalog round-trips unchanged.
    pub fn set_book_author(&mut self, book_id: &str, author: &str) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        document.authors = split_authors(author);
        document.validate()?;
        self.apply_document(document)
    }

    pub fn delete_group(&mut self, id: &str) -> Result<()> {
        ensure!(self.groups.iter().any(|group| group.id == id), "分组不存在");
        let cached = self
            .group_subtree_ids(id)
            .into_iter()
            .collect::<HashSet<_>>();
        let mut conn = db::open_conn(&self.db_path)?;
        let removed = db::transactions::delete_group_tree(&mut conn, id)?
            .into_iter()
            .chain(cached)
            .collect::<HashSet<_>>();
        self.groups.retain(|group| !removed.contains(&group.id));
        for book in &mut self.books {
            if book
                .group_id
                .as_ref()
                .is_some_and(|group_id| removed.contains(group_id))
            {
                book.group_id = None;
            }
        }
        Ok(())
    }

    pub fn set_book_group(&mut self, book_id: &str, group_id: Option<&str>) -> Result<()> {
        let position = self
            .books
            .iter()
            .position(|book| book.id == book_id)
            .context("图书不存在")?;
        let conn = db::open_conn(&self.db_path)?;
        db::transactions::set_book_group(&conn, book_id, group_id)?;
        self.books[position].group_id = group_id.map(str::to_owned);
        Ok(())
    }

    pub(crate) fn new_group_id(&self, name: &str, parent_id: Option<&str>) -> String {
        deterministic_id(
            "group",
            format!("{name}\0{}\0{}", parent_id.unwrap_or("-"), now_nanos()).as_bytes(),
        )
    }
}
