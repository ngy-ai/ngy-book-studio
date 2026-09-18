//! Reading progress: the write queue and the task that persists it.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadingProgressWrite {
    Unit { spine_index: usize, unit_id: String },
}

pub(crate) fn reading_progress_for_locator(
    spine_index: usize,
    progress_locators: &[DocumentLocator],
) -> Option<ReadingProgressWrite> {
    progress_locators
        .get(spine_index)
        .map(|locator| ReadingProgressWrite::Unit {
            spine_index,
            unit_id: locator.unit_id.clone(),
        })
}

#[derive(Clone, Debug)]
pub(crate) struct ReadingProgressWriteEvent {
    pub(crate) progress: ReadingProgressWrite,
    pub(crate) error: Option<String>,
    pub(crate) projection: Option<ReadingProgressProjection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SequencedReadingProgressWrite {
    pub(crate) sequence: u64,
    pub(crate) progress: ReadingProgressWrite,
}

pub(crate) type ReadingProgressProjection = (u64, LibraryStore);
pub(crate) type ReadingProgressCompletion =
    std::result::Result<Option<ReadingProgressProjection>, String>;

pub(crate) static NEXT_READING_PROGRESS_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_reading_progress_sequence() -> Result<u64, String> {
    NEXT_READING_PROGRESS_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| "阅读进度动作序列已耗尽".to_string())
}

/// Serializes progress mutations through the process-level library service.
///
/// The worker drains queued positions to the most recent one between writes.
/// This keeps rapid page changes from producing an unbounded series of SQLite
/// mutations while preserving FIFO ordering around an already-active write.
pub(crate) struct ReadingProgressWriter {
    pub(crate) sender: async_channel::Sender<SequencedReadingProgressWrite>,
    pub(crate) completion: async_channel::Receiver<ReadingProgressCompletion>,
}

impl ReadingProgressWriter {
    pub(crate) fn start(
        book_id: String,
        book_incarnation: u64,
        services: Arc<AppServices>,
    ) -> (Self, async_channel::Receiver<ReadingProgressWriteEvent>) {
        let (sender, receiver) = async_channel::unbounded();
        let (event_sender, event_receiver) = async_channel::unbounded();
        let (completion_sender, completion_receiver) = async_channel::bounded(1);
        let runtime = services.runtime();
        let _worker = runtime.spawn(run_reading_progress_writer(
            book_id,
            book_incarnation,
            services,
            receiver,
            event_sender,
            completion_sender,
        ));
        (
            Self {
                sender,
                completion: completion_receiver,
            },
            event_receiver,
        )
    }

    pub(crate) fn enqueue(&self, progress: ReadingProgressWrite) -> Result<(), String> {
        let sequence = next_reading_progress_sequence()?;
        self.sender
            .try_send(SequencedReadingProgressWrite { sequence, progress })
            .map_err(|_| "阅读进度后台任务已停止".to_string())
    }

    /// Queues the final position and closes this window's input. The returned
    /// receiver resolves only after the runtime worker has drained the queue
    /// and the last SQLite mutation has completed. Window-close code must wait
    /// for it before removing the final GPUI window.
    pub(crate) fn finish(
        self,
        progress: ReadingProgressWrite,
    ) -> (
        Result<(), String>,
        async_channel::Receiver<ReadingProgressCompletion>,
    ) {
        let result = self.enqueue(progress);
        self.sender.close();
        (result, self.completion)
    }
}

pub(crate) async fn run_reading_progress_writer(
    book_id: String,
    book_incarnation: u64,
    services: Arc<AppServices>,
    receiver: async_channel::Receiver<SequencedReadingProgressWrite>,
    event_sender: async_channel::Sender<ReadingProgressWriteEvent>,
    completion_sender: async_channel::Sender<ReadingProgressCompletion>,
) {
    let mut final_result = Ok(None);
    while let Ok(mut write) = receiver.recv().await {
        while let Ok(latest) = receiver.try_recv() {
            write = latest;
        }
        let progress = write.progress;

        let operation_book_id = book_id.clone();
        let operation_progress = progress.clone();
        let operation_sequence = write.sequence;
        let task = services.spawn_library_projected(move |library| match operation_progress {
            ReadingProgressWrite::Unit {
                spine_index,
                unit_id,
            } => library
                .update_progress_at_ordered(
                    &operation_book_id,
                    spine_index,
                    &unit_id,
                    book_incarnation,
                    operation_sequence,
                )
                .map(|_| ()),
        });
        let (error, projection) = match task.await {
            Ok(Ok(mutation)) => (None, Some((mutation.generation, mutation.snapshot))),
            Ok(Err(error)) => (Some(format!("{error:#}")), None),
            Err(error) => (Some(format!("阅读进度任务异常停止：{error}")), None),
        };
        if let Some(error) = error.as_deref() {
            tracing::warn!(
                book_id = %book_id,
                ?progress,
                %error,
                "failed to persist reading progress"
            );
        }
        final_result = match &error {
            Some(error) => Err(error.clone()),
            None => Ok(projection.clone()),
        };
        let _ = event_sender.try_send(ReadingProgressWriteEvent {
            progress,
            error,
            projection,
        });
    }
    let _ = completion_sender.send(final_result).await;
}
