//! The UI-facing service: chapter handles, the per-chapter lock and the
//! async wrappers around the record store.

use super::*;

impl LearningService {
    pub fn new(data_directory: PathBuf, runtime: IoRuntime) -> Self {
        let stores: Arc<[Arc<Mutex<LearningStore>>; 10]> = Arc::new(std::array::from_fn(|index| {
            Arc::new(Mutex::new(
                LearningStore::for_chapter(data_directory.clone(), (index + 1) as u8)
                    .expect("the ten desktop chapters are registered"),
            ))
        }));
        Self {
            archive_directory: data_directory.join("archives"),
            runtime,
            store: Arc::clone(&stores[0]),
            stores,
            chapter: 1,
            course_root: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("courses/agent-foundations"),
        }
    }

    pub fn chapter(&self) -> u8 {
        self.chapter
    }

    pub fn course_id(&self) -> &'static str {
        chapter_course_id(self.chapter)
    }

    pub fn for_chapter(&self, chapter: u8) -> Result<Self> {
        validate_chapter(chapter)?;
        Ok(Self {
            runtime: self.runtime.clone(),
            store: Arc::clone(&self.stores[usize::from(chapter - 1)]),
            stores: Arc::clone(&self.stores),
            chapter,
            course_root: self.course_root.clone(),
            archive_directory: self.archive_directory.clone(),
        })
    }

    pub fn archive_directory(&self) -> PathBuf {
        self.archive_directory.clone()
    }

    pub async fn restore_dialog_directory(&self) -> Result<PathBuf> {
        let directory = self.archive_directory();
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    std::fs::create_dir_all(&directory).context("无法准备学习备份目录")?;
                    let directory =
                        std::fs::canonicalize(directory).context("无法定位学习备份目录")?;
                    Ok(crate::startup::shell_dialog_directory(&directory))
                })
                .await?
            })
            .await?
    }

    pub async fn snapshot(&self) -> Result<LearningSnapshot> {
        let store = Arc::clone(&self.store);
        let root = self.course_root.clone();
        let chapter = self.chapter;
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let store = store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?;
                    store.prepare_directory()?;
                    Ok(LearningSnapshot {
                        lessons: lessons_for(chapter)?,
                        workspace: store.workspace()?,
                        history: store.history()?,
                        environment: probe_environment(&root),
                    })
                })
                .await?
            })
            .await?
    }

    pub async fn save(&self, workspace: LearningWorkspace) -> Result<LearningWorkspace> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .save(workspace)
                })
                .await?
            })
            .await?
    }

    pub async fn export(&self, path: PathBuf) -> Result<()> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .export(&path)
                })
                .await?
            })
            .await?
    }

    pub async fn restore(&self, path: PathBuf) -> Result<LearningSnapshot> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .restore(&path)
                })
                .await?
            })
            .await??;
        self.snapshot().await
    }

    pub async fn load_report(&self, id: String) -> Result<LearningRunReport> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .report(&id)
                })
                .await?
            })
            .await?
    }

    pub async fn new_round(&self, workspace: LearningWorkspace) -> Result<LearningSnapshot> {
        let store = Arc::clone(&self.store);
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .new_round(workspace)
                })
                .await?
            })
            .await??;
        self.snapshot().await
    }

    pub async fn run(
        &self,
        workspace: LearningWorkspace,
        cancellation: Arc<AtomicBool>,
        events: async_channel::Sender<LearningTrace>,
    ) -> Result<LearningRunReport> {
        let store = Arc::clone(&self.store);
        let root = self.course_root.clone();
        let chapter = self.chapter;
        self.runtime
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let id = crate::learning_records::new_run_id()?;
                    let saved = store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .begin(workspace, &id)?;
                    let started = Instant::now();
                    let scope = RunScope { chapter, id: &id };
                    let report = match execute(&root, &saved, scope, &cancellation, &events) {
                        Ok(report) => report,
                        Err(error) => failed_report(
                            scope,
                            &saved,
                            "error",
                            "host_error",
                            &format!("{error:#}"),
                            started.elapsed(),
                        ),
                    };
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("学习记录锁不可用"))?
                        .finish(&id, report)
                })
                .await?
            })
            .await?
    }
}
