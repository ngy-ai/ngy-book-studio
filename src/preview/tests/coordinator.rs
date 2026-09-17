//! Tests for the coordinator: pause, resume, cancel, retry and recovery.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_visual_job_rejects_missing_replaced_stopped_and_closed_queue() {
    let store = Arc::new(MemoryVisualJobStore::default());
    let source = Arc::new(MemoryDocumentSource(sample_document()));
    let sink = Arc::new(MemoryPageSink::new(Arc::clone(&store)));
    let renderer: Arc<dyn VisualRenderer> = Arc::new(SlowRenderer);
    let expected = spec("committed-job", renderer.as_ref());
    let mut coordinator = VisualJobCoordinator::new(
        Handle::current(),
        store.clone(),
        source,
        sink,
        vec![renderer],
    )
    .unwrap();

    let error = coordinator.schedule_committed(&expected).await.unwrap_err();
    assert!(error.to_string().contains("不存在"));

    // A same-ID record alone is insufficient: every part of the frozen
    // rendering identity must still agree, even after successful execution.
    for field in [
        "book", "source", "revision", "renderer", "version", "fidelity", "units", "profile",
    ] {
        let mut record = VisualJobRecord::queued(expected.clone(), 1);
        record.state = VisualJobState::Succeeded;
        match field {
            "book" => record.spec.book_id = "other-book".to_string(),
            "source" => record.spec.source_id = "other-source".to_string(),
            "revision" => record.spec.document_revision = Revision::new(4),
            "renderer" => record.spec.renderer = "other-renderer".to_string(),
            "version" => record.spec.renderer_version = "2".to_string(),
            "fidelity" => record.spec.fidelity = RenderFidelity::Normalized,
            "units" => record.spec.unit_ids = vec!["unit-1".to_string()],
            "profile" => record.spec.profile.viewport_width += 1,
            _ => unreachable!(),
        }
        store
            .jobs
            .lock()
            .unwrap()
            .insert(expected.id.clone(), record);
        let error = coordinator.schedule_committed(&expected).await.unwrap_err();
        assert!(error.to_string().contains("身份不一致"), "{field}: {error}");
    }

    for state in [
        VisualJobState::Paused,
        VisualJobState::Cancelled,
        VisualJobState::Failed,
    ] {
        let mut record = VisualJobRecord::queued(expected.clone(), 1);
        record.state = state;
        record.error = Some("injected renderer failure".to_string());
        store
            .jobs
            .lock()
            .unwrap()
            .insert(expected.id.clone(), record);
        let error = coordinator.schedule_committed(&expected).await.unwrap_err();
        assert!(error.to_string().contains("无法调度"), "{state:?}: {error}");
        assert!(error.to_string().contains("injected renderer failure"));
    }

    for pause in [false, true] {
        let mut record = VisualJobRecord::queued(expected.clone(), 1);
        record.state = VisualJobState::Running;
        record.pause_requested = pause;
        record.cancel_requested = !pause;
        store
            .jobs
            .lock()
            .unwrap()
            .insert(expected.id.clone(), record);
        let error = coordinator.schedule_committed(&expected).await.unwrap_err();
        assert!(error.to_string().contains("已请求暂停或取消"));
    }

    coordinator.sender.close();
    (&mut coordinator.worker).await.unwrap();
    for state in [
        VisualJobState::Queued,
        VisualJobState::Running,
        VisualJobState::Succeeded,
    ] {
        let mut record = VisualJobRecord::queued(expected.clone(), 1);
        record.state = state;
        store
            .jobs
            .lock()
            .unwrap()
            .insert(expected.id.clone(), record);
        let error = coordinator.schedule_committed(&expected).await.unwrap_err();
        assert!(
            error.to_string().contains("队列已经关闭"),
            "{state:?}: {error}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coordinator_supports_pause_resume_cancel_and_retry() {
    let store = Arc::new(MemoryVisualJobStore::default());
    let source = Arc::new(MemoryDocumentSource(sample_document()));
    let sink = Arc::new(MemoryPageSink::new(Arc::clone(&store)));
    let slow: Arc<dyn VisualRenderer> = Arc::new(SlowRenderer);
    let gated_impl = Arc::new(GatedRenderer::new());
    let gated: Arc<dyn VisualRenderer> = gated_impl.clone();
    let fail_once_impl = Arc::new(FailOnceRenderer {
        failed: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        emitted: Mutex::new(Vec::new()),
    });
    let fail_once: Arc<dyn VisualRenderer> = fail_once_impl.clone();
    let coordinator = VisualJobCoordinator::new(
        Handle::current(),
        store,
        source,
        sink.clone(),
        vec![slow.clone(), gated.clone(), fail_once.clone()],
    )
    .expect("create coordinator");

    gated_impl.permits.add_permits(1);
    coordinator
        .submit(spec("pause-job", gated.as_ref()))
        .await
        .expect("submit pause job");
    wait_for_completed_pages(&coordinator, "pause-job", 1).await;
    assert!(coordinator.pause("pause-job").await.expect("pause job"));
    coordinator
        .wait_for_state("pause-job", VisualJobState::Paused, Duration::from_secs(2))
        .await
        .expect("job pauses");
    gated_impl.permits.add_permits(2);
    assert!(coordinator.resume("pause-job").await.expect("resume job"));
    let succeeded = coordinator
        .wait_for_state(
            "pause-job",
            VisualJobState::Succeeded,
            Duration::from_secs(3),
        )
        .await
        .expect("resumed job succeeds");
    assert_eq!(succeeded.attempts, 2);
    assert_eq!(
        *gated_impl.emitted.lock().expect("gated renderer log"),
        vec![0, 1, 2],
        "resume must not emit the durable prefix again"
    );

    coordinator
        .submit(spec("paused-cancel-job", slow.as_ref()))
        .await
        .expect("submit paused cancellation job");
    coordinator
        .wait_for_state(
            "paused-cancel-job",
            VisualJobState::Running,
            Duration::from_secs(2),
        )
        .await
        .expect("paused cancellation job starts");
    assert!(
        coordinator
            .pause("paused-cancel-job")
            .await
            .expect("pause before cancellation")
    );
    coordinator
        .wait_for_state(
            "paused-cancel-job",
            VisualJobState::Paused,
            Duration::from_secs(2),
        )
        .await
        .expect("job reaches paused state");
    assert!(
        coordinator
            .cancel("paused-cancel-job")
            .await
            .expect("cancel paused job")
    );
    coordinator
        .wait_for_state(
            "paused-cancel-job",
            VisualJobState::Cancelled,
            Duration::from_secs(2),
        )
        .await
        .expect("paused job cancels without being resumed");

    coordinator
        .submit(spec("cancel-job", slow.as_ref()))
        .await
        .expect("submit cancel job");
    coordinator
        .wait_for_state(
            "cancel-job",
            VisualJobState::Running,
            Duration::from_secs(2),
        )
        .await
        .expect("cancel job starts");
    assert!(coordinator.cancel("cancel-job").await.expect("cancel job"));
    coordinator
        .wait_for_state(
            "cancel-job",
            VisualJobState::Cancelled,
            Duration::from_secs(2),
        )
        .await
        .expect("job cancels");

    coordinator
        .submit(spec("retry-job", fail_once.as_ref()))
        .await
        .expect("submit retry job");
    let failed = coordinator
        .wait_for_state("retry-job", VisualJobState::Failed, Duration::from_secs(2))
        .await
        .expect("first attempt fails");
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("intentional"))
    );
    assert!(coordinator.retry("retry-job").await.expect("retry job"));
    coordinator
        .wait_for_state(
            "retry-job",
            VisualJobState::Succeeded,
            Duration::from_secs(2),
        )
        .await
        .expect("retry succeeds");
    assert_eq!(fail_once_impl.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        *fail_once_impl.emitted.lock().expect("fail-once log"),
        vec![0, 0, 1, 2],
        "explicit retry must discard staging and start at page zero"
    );
    assert!(
        sink.published
            .lock()
            .expect("test sink")
            .contains_key("retry-job")
    );
    assert!(
        !sink
            .staging
            .lock()
            .expect("test staging")
            .contains_key("retry-job")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coordinator_recovers_running_job_without_reemitting_durable_prefix() {
    let store = Arc::new(MemoryVisualJobStore::default());
    let source = Arc::new(MemoryDocumentSource(sample_document()));
    let sink = Arc::new(MemoryPageSink::new(Arc::clone(&store)));
    let renderer_impl = Arc::new(GatedRenderer::new());
    let renderer: Arc<dyn VisualRenderer> = renderer_impl.clone();

    renderer_impl.permits.add_permits(1);
    let first = VisualJobCoordinator::new(
        Handle::current(),
        store.clone(),
        source.clone(),
        sink.clone(),
        vec![renderer.clone()],
    )
    .expect("create first coordinator");
    first
        .submit(spec("recover-job", renderer.as_ref()))
        .await
        .expect("submit recoverable job");
    wait_for_completed_pages(&first, "recover-job", 1).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(20)).await;

    renderer_impl.permits.add_permits(2);
    let recovered = VisualJobCoordinator::new(
        Handle::current(),
        store,
        source,
        sink.clone(),
        vec![renderer],
    )
    .expect("create recovered coordinator");
    let _ = recovered.recover("recover-job").await;
    recovered
        .wait_for_state(
            "recover-job",
            VisualJobState::Succeeded,
            Duration::from_secs(3),
        )
        .await
        .expect("recovered job succeeds");

    assert_eq!(
        *renderer_impl.emitted.lock().expect("recovery renderer log"),
        vec![0, 1, 2],
        "process recovery must not emit the durable prefix again"
    );
    let published = sink.published.lock().expect("published pages");
    assert_eq!(published["recover-job"].len(), 3);
    assert!(sink.staging.lock().expect("staged pages").is_empty());
}
