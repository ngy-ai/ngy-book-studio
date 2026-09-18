//! The shared database and services fixture for indexing tests.

use super::*;

pub(crate) struct Fixture {
    pub(crate) _temp: tempfile::TempDir,
    pub(crate) runtime: IoRuntime,
    pub(crate) db_path: PathBuf,
    pub(crate) blobs: Arc<LocalBlobStore>,
    pub(crate) book_id: String,
    pub(crate) source_id: String,
    pub(crate) revision: u64,
    pub(crate) unit_id: String,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let runtime = IoRuntime::new(2).unwrap();
        let mut library =
            LibraryStore::load_from_with_runtime(temp.path().to_path_buf(), runtime.clone())
                .unwrap();
        // These tests exercise queue/run semantics, so new derivative jobs
        // must be created queued instead of relying on the product default.
        library
            .background_job_auto_run_flag()
            .store(true, Ordering::Release);
        let book = library.create_book("Index test", "Author").unwrap();
        let db_path = temp.path().join(db::DATABASE_FILE);
        let conn = db::open_conn(&db_path).unwrap();
        let source = db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap();
        let unit = db::content_units::list_for_source(&conn, &source.id)
            .unwrap()
            .remove(0);
        let blobs = Arc::new(LocalBlobStore::new(temp.path().join("objects")).unwrap());
        Self {
            _temp: temp,
            runtime,
            db_path,
            blobs,
            book_id: book.id,
            source_id: source.id,
            revision: book.revision,
            unit_id: unit.id,
        }
    }

    pub(crate) fn coordinator(&self, provider: Arc<MockProvider>) -> Arc<IndexingCoordinator> {
        self.coordinator_with_providers(provider.clone(), provider)
    }

    /// Creates one more book in the same data directory so the queue holds
    /// two canonical embedding jobs for scheduling tests.
    pub(crate) fn add_second_book(&self) -> String {
        let data_dir = self
            .db_path
            .parent()
            .expect("fixture database lives in the temporary data directory")
            .to_path_buf();
        let mut library =
            LibraryStore::load_from_with_runtime(data_dir, self.runtime.clone()).unwrap();
        library
            .background_job_auto_run_flag()
            .store(true, Ordering::Release);
        let book = library.create_book("Second index test", "Author").unwrap();
        let conn = db::open_conn(&self.db_path).unwrap();
        db::book_sources::get_revision(&conn, &book.id, book.revision)
            .unwrap()
            .unwrap()
            .id
    }

    pub(crate) fn embedding_status_count(&self, status: db::index_jobs::IndexJobStatus) -> usize {
        let conn = db::open_conn(&self.db_path).unwrap();
        db::index_jobs::list_by_kind(&conn, "embedding")
            .unwrap()
            .into_iter()
            .filter(|job| job.status == status)
            .count()
    }

    pub(crate) fn wait_until(
        &self,
        description: &str,
        timeout: Duration,
        predicate: impl Fn(&Self) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        while !predicate(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn coordinator_with_providers(
        &self,
        embedding_provider: Arc<dyn OpenAiCompatibleProvider>,
        vision_provider: Arc<dyn OpenAiCompatibleProvider>,
    ) -> Arc<IndexingCoordinator> {
        let blobs: Arc<dyn BlobStore> = self.blobs.clone();
        IndexingCoordinator::start(
            self.runtime.handle(),
            &self.db_path,
            blobs,
            embedding_provider,
            vision_provider,
            test_model_config(
                "embed-test",
                "embedding-endpoint-a:embed-test",
                "vision-test",
                "vision-endpoint-a:vision-test",
            ),
        )
        .unwrap()
    }

    pub(crate) fn indexing_inner(&self) -> Arc<IndexingInner> {
        let provider: Arc<dyn OpenAiCompatibleProvider> = Arc::new(MockProvider::default());
        Arc::new(IndexingInner {
            runtime: self.runtime.handle(),
            db_path: self.db_path.clone(),
            blobs: self.blobs.clone(),
            models: RwLock::new(ModelServices {
                embedding_provider: provider.clone(),
                vision_provider: provider,
                config: test_model_config(
                    "embed-test",
                    "embedding-endpoint-a:embed-test",
                    "vision-test",
                    "vision-endpoint-a:vision-test",
                ),
                translation: None,
            }),
            scheduling: RwLock::new(JobScheduling::default()),
            transitions: AsyncMutex::new(()),
            wake: Notify::new(),
            reconcile_translations: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        })
    }

    pub(crate) fn wait_for_post_vision_embedding(
        &self,
        coordinator: &IndexingCoordinator,
        embedding_identity: &str,
        vision_identity: &str,
    ) {
        // Vision queues a separate embedding phase before it succeeds;
        // the canonical text embedding job can already be complete.
        let jobs = self
            .runtime
            .block_on(coordinator.jobs_for_book(&self.book_id))
            .unwrap();
        let post_vision = jobs
            .iter()
            .find(|job| {
                job.kind == "embedding"
                    && job.source_id.as_deref() == Some(self.source_id.as_str())
                    && serde_json::from_str::<JobCursor>(&job.cursor_json).is_ok_and(|cursor| {
                        cursor.execution_identity.as_deref() == Some(embedding_identity)
                            && cursor.input_execution_identity.as_deref() == Some(vision_identity)
                    })
            })
            .expect("successful vision must queue matching embedding work");
        self.runtime
            .block_on(coordinator.wait_for_state(
                &post_vision.id,
                IndexingJobStatus::Succeeded,
                TEST_PROGRESS_WAIT,
            ))
            .unwrap();
    }

    pub(crate) fn publish_test_visual_page(&self) {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let bytes = bytes.into_inner();
        let key = self.runtime.block_on(self.blobs.put(&bytes)).unwrap();
        let blob = db::blobs::BlobRecord {
            object_key: key.to_string(),
            media_type: "image/png".to_string(),
            byte_len: bytes.len() as u64,
            hash: blake3::hash(&bytes).to_hex().to_string(),
            created_at: unix_timestamp().unwrap(),
        };
        let page = db::visual_pages::VisualPage {
            id: "singleton-test-page".to_string(),
            book_id: self.book_id.clone(),
            source_id: self.source_id.clone(),
            content_unit_id: Some(self.unit_id.clone()),
            page_index: 0,
            object_key: key.to_string(),
            width: 2,
            height: 3,
            render_scale: 1.0,
            renderer: "test-renderer".to_string(),
            renderer_version: "1".to_string(),
            document_revision: self.revision,
            unit_revision: self.revision,
            profile_id: "test-profile".to_string(),
            fidelity: "structural".to_string(),
            locator_json: serde_json::to_string(
                &DocumentLocator::unit(&self.book_id, &self.unit_id)
                    .with_source(SourceLocator::created()),
            )
            .unwrap(),
            created_at: unix_timestamp().unwrap(),
        };
        db::transactions::replace_visual_pages(
            &mut db::open_conn(&self.db_path).unwrap(),
            &self.book_id,
            &self.source_id,
            self.revision,
            &[blob],
            &[page],
        )
        .unwrap();
    }
}

/// Puts one job in the terminal state a finished run leaves: succeeded with a
/// durable cursor at `next_ordinal`. Used by the retry tests, which care about
/// where the cursor starts rather than about how a run gets there.
pub(crate) fn finish_job_at(fixture: &Fixture, job_id: &str, next_ordinal: usize) {
    let conn = db::open_conn(&fixture.db_path).unwrap();
    let job = db::index_jobs::get(&conn, job_id).unwrap().unwrap();
    let mut cursor: JobCursor = serde_json::from_str(&job.cursor_json).unwrap();
    cursor.next_ordinal = next_ordinal;
    cursor.retry_ordinals.clear();
    db::index_jobs::update_state(
        &conn,
        job_id,
        db::index_jobs::IndexJobStatus::Succeeded,
        &cursor.encode().unwrap(),
        None,
        unix_timestamp().unwrap(),
        None,
        Some(unix_timestamp().unwrap()),
    )
    .unwrap();
}
