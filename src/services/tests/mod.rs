//! Shared fixtures and test doubles for the service tests.

mod jobs_test;

mod mutation_test;

mod settings_test;

mod settings_validation_test;

mod translation_test;

mod visual;

use super::*;
use crate::{
    ai::DEFAULT_OLLAMA_OPENAI_BASE_URL, ai::credentials::MemoryCredentialStore,
    library::ImportOutcome, preview::VisualJobState,
};
#[cfg(target_os = "windows")]
use crate::{
    office_com::{OfficeEnhanceOutput, OfficeEnhanceRequest, OfficeEnhancementKind, OfficeFuture},
    office_visual::OfficeEnhancedRenderer,
};
#[cfg(target_os = "windows")]
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use std::{
    future::Future,
    io::Cursor,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
    task::{Context as TaskContext, Poll, Wake, Waker},
    thread,
    time::Duration,
};

struct ThreadWake(thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls a future on this ordinary test thread without entering any Tokio
/// runtime. This mirrors GPUI's foreground executor closely enough to catch
/// service methods that accidentally depend on an ambient Tokio handle.
fn block_on_without_tokio<F: Future>(future: F) -> F::Output {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = TaskContext::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
struct FakeOfficeSlides;

#[cfg(target_os = "windows")]
impl OfficeEnhancer for FakeOfficeSlides {
    fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
        Box::pin(async move {
            ensure!(
                request.kind == OfficeEnhancementKind::PowerPointImages,
                "test expected PowerPoint enhancement"
            );
            let mut paths = Vec::new();
            for index in 1..=2 {
                let path = request.target.join(format!("Slide{index}.png"));
                std::fs::write(&path, tiny_office_slide_png(index as u8))?;
                paths.push(path);
            }
            Ok(OfficeEnhanceOutput::Images(paths))
        })
    }
}

#[cfg(target_os = "windows")]
struct GatedFakeOfficeSlides {
    started: async_channel::Sender<()>,
    release: async_channel::Receiver<()>,
}

#[cfg(target_os = "windows")]
impl OfficeEnhancer for GatedFakeOfficeSlides {
    fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
        Box::pin(async move {
            self.started.send(()).await?;
            self.release.recv().await?;
            FakeOfficeSlides.enhance(request).await
        })
    }
}

#[cfg(target_os = "windows")]
fn tiny_office_slide_png(channel: u8) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(4, 3, image::Rgba([channel, 2, 3, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

fn endpoint_fixture(id: &str, port: u16) -> EndpointSettings {
    EndpointSettings {
        id: id.into(),
        name: format!("Endpoint {id}"),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        remote_content_confirmed: false,
        allow_insecure_remote_http: false,
        confirmed_remote_endpoint: String::new(),
        request_timeout_secs: 30,
    }
}

fn routed_settings() -> ProviderSettings {
    let mut settings = ProviderSettings::default();
    settings.endpoint_routing.additional_endpoints = vec![
        endpoint_fixture("embed", 21435),
        endpoint_fixture("vision", 21436),
    ];
    settings.endpoint_routing.embedding_endpoint_id = "embed".into();
    settings.endpoint_routing.vision_endpoint_id = "vision".into();
    settings
}
