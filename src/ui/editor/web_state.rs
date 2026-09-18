//! The per-page state the protocol handlers read and the media snapshot they
//! serve.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct EditorMediaAsset {
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) content_hash: String,
    /// Only unsaved assets live in memory. Persisted assets retain their
    /// authorization metadata here and are streamed through AppServices.
    pub(crate) bytes: Option<Arc<Vec<u8>>>,
}

impl EditorMediaAsset {
    #[cfg(test)]
    pub(crate) fn checked(metadata: &AssetRef, bytes: Arc<Vec<u8>>) -> Result<Self> {
        metadata.validate().context("编辑器媒体元数据无效")?;
        ensure_editor_asset_id(&metadata.id)?;
        anyhow::ensure!(
            metadata.byte_len == bytes.len() as u64,
            "编辑器媒体长度与元数据不匹配"
        );
        anyhow::ensure!(
            metadata.content_hash == blake3::hash(bytes.as_slice()).to_hex().to_string(),
            "编辑器媒体内容哈希与元数据不匹配"
        );
        Ok(Self::prevalidated(metadata, bytes))
    }

    pub(crate) fn authorized(metadata: &AssetRef) -> Result<Self> {
        metadata.validate().context("编辑器媒体元数据无效")?;
        ensure_editor_asset_id(&metadata.id)?;
        Ok(Self {
            media_type: metadata.media_type.clone(),
            byte_len: metadata.byte_len,
            content_hash: metadata.content_hash.clone(),
            bytes: None,
        })
    }

    /// The caller created `metadata` from these exact bytes on a background
    /// task. Keep the cheap structural checks here so installing a freshly
    /// selected video never hashes a potentially large object on the GPUI
    /// thread for a second time.
    pub(crate) fn prevalidated(metadata: &AssetRef, bytes: Arc<Vec<u8>>) -> Self {
        debug_assert_eq!(metadata.byte_len, bytes.len() as u64);
        Self {
            media_type: metadata.media_type.clone(),
            byte_len: metadata.byte_len,
            content_hash: metadata.content_hash.clone(),
            bytes: Some(bytes),
        }
    }

    pub(crate) fn matches(&self, metadata: &AssetRef) -> bool {
        self.media_type == metadata.media_type
            && self.byte_len == metadata.byte_len
            && self.content_hash == metadata.content_hash
    }
}

#[derive(Clone)]
pub(crate) struct EditorMediaSnapshot {
    pub(crate) book_id: Arc<str>,
    pub(crate) assets: Arc<HashMap<String, EditorMediaAsset>>,
}

impl MediaBackend for EditorMediaSnapshot {
    fn metadata(&self, book_id: &str, asset_id: &str) -> Result<MediaMetadata> {
        anyhow::ensure!(book_id == self.book_id.as_ref(), "资源不属于指定图书");
        let asset = self.assets.get(asset_id).context("资源不存在")?;
        Ok(MediaMetadata {
            media_type: asset.media_type.clone(),
            byte_len: asset.byte_len,
        })
    }

    fn read(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
        anyhow::ensure!(book_id == self.book_id.as_ref(), "资源不属于指定图书");
        Ok(self
            .assets
            .get(asset_id)
            .context("资源不存在")?
            .bytes
            .as_ref()
            .context("持久媒体必须通过异步对象存储读取")?
            .as_ref()
            .clone())
    }

    fn read_range(
        &self,
        book_id: &str,
        asset_id: &str,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>> {
        anyhow::ensure!(book_id == self.book_id.as_ref(), "资源不属于指定图书");
        let bytes = self
            .assets
            .get(asset_id)
            .context("资源不存在")?
            .bytes
            .as_ref()
            .context("持久媒体必须通过异步对象存储读取")?;
        let start = usize::try_from(range.start).context("媒体范围起点过大")?;
        let end = usize::try_from(range.end).context("媒体范围终点过大")?;
        Ok(bytes.get(start..end).context("媒体范围越界")?.to_vec())
    }
}

/// Versioned chapter snapshots served by the editor custom protocol. Keeping
/// a short history prevents an in-flight navigation from receiving another
/// chapter's HTML when users switch tabs or chapters quickly.
#[derive(Clone)]
pub struct EditorWebState {
    pub(crate) book_id: Arc<str>,
    pub(crate) session_id: Arc<str>,
    pub(crate) pages: Arc<Mutex<EditorWebPages>>,
    pub(crate) media: Arc<Mutex<Arc<HashMap<String, EditorMediaAsset>>>>,
    /// Held across `responder.respond` and across the close transition. This
    /// makes the final open check atomic with respect to WebView release.
    pub(crate) protocol_open: Arc<Mutex<bool>>,
}

impl EditorWebState {
    pub(crate) fn new(book_id: String, href: String, html: String) -> Self {
        let session_sequence = NEXT_EDITOR_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("editor-session-{}-{session_sequence}", std::process::id());
        // `EditorApp::new` replaces this bootstrap identity with the stable
        // content-unit ID before the WebView is built. Keeping a valid,
        // deterministic placeholder also makes the protocol state safe to use
        // in focused tests without weakening the four-part IPC contract.
        let bootstrap_chapter_id = deterministic_id(
            "editor-bootstrap-unit",
            format!("{book_id}\0{href}").as_bytes(),
        );
        let state = Self {
            book_id: Arc::from(book_id),
            session_id: Arc::from(session_id),
            pages: Arc::new(Mutex::new(EditorWebPages::default())),
            media: Arc::new(Mutex::new(Arc::new(HashMap::new()))),
            protocol_open: Arc::new(Mutex::new(true)),
        };
        state.set(0, bootstrap_chapter_id, href, html);
        state
    }

    pub(crate) fn session_id(&self) -> &str {
        self.session_id.as_ref()
    }

    /// Installs only the immutable authorization metadata referenced by the
    /// canonical document. Persisted bytes remain in object storage until the
    /// asynchronous protocol handler requests a full object or exact range.
    pub(crate) fn authorize_media(&self, document: &BookDocument) -> Result<()> {
        self.ensure_document_owner(document)?;
        document.validate().context("编辑器文档无效")?;
        let mut assets = HashMap::new();
        for asset_id in editor_media_asset_ids(document) {
            let metadata = document
                .find_asset(&asset_id)
                .with_context(|| format!("编辑器媒体元数据不存在：{asset_id}"))?;
            assets.insert(asset_id, EditorMediaAsset::authorized(metadata)?);
        }
        *self.media.lock().unwrap() = Arc::new(assets);
        Ok(())
    }

    /// Atomically installs a freshly selected, already hashed object and drops
    /// every object no longer referenced by the current canonical document.
    /// This is memory-only and is safe to call while updating GPUI state.
    pub(crate) fn sync_media(
        &self,
        document: &BookDocument,
        prepared: Option<(&AssetRef, Arc<Vec<u8>>)>,
    ) -> Result<()> {
        self.ensure_document_owner(document)?;
        let current = self.media_snapshot().assets;
        let mut candidates = current.as_ref().clone();
        if let Some((metadata, bytes)) = prepared {
            metadata.validate().context("编辑器媒体元数据无效")?;
            ensure_editor_asset_id(&metadata.id)?;
            anyhow::ensure!(
                metadata.byte_len == bytes.len() as u64,
                "编辑器媒体长度与元数据不匹配"
            );
            let canonical = document
                .find_asset(&metadata.id)
                .context("新媒体不在当前图书文档中")?;
            anyhow::ensure!(
                canonical.media_type == metadata.media_type
                    && canonical.byte_len == metadata.byte_len
                    && canonical.content_hash == metadata.content_hash,
                "新媒体与当前图书文档不匹配"
            );
            candidates.insert(
                metadata.id.clone(),
                EditorMediaAsset::prevalidated(canonical, bytes),
            );
        }

        let mut retained = HashMap::new();
        for asset_id in editor_media_asset_ids(document) {
            ensure_editor_asset_id(&asset_id)?;
            let metadata = document
                .find_asset(&asset_id)
                .with_context(|| format!("编辑器媒体元数据不存在：{asset_id}"))?;
            metadata.validate().context("编辑器媒体元数据无效")?;
            let asset = match candidates.get(&asset_id) {
                Some(asset) if asset.matches(metadata) => asset.clone(),
                Some(_) => anyhow::bail!("编辑器媒体快照与文档不匹配"),
                None => EditorMediaAsset::authorized(metadata)?,
            };
            retained.insert(asset_id, asset);
        }
        *self.media.lock().unwrap() = Arc::new(retained);
        Ok(())
    }

    pub(crate) fn ensure_document_owner(&self, document: &BookDocument) -> Result<()> {
        anyhow::ensure!(
            document.id == self.book_id.as_ref(),
            "编辑器媒体快照不属于当前图书"
        );
        Ok(())
    }

    pub(crate) fn media_snapshot(&self) -> EditorMediaSnapshot {
        EditorMediaSnapshot {
            book_id: Arc::clone(&self.book_id),
            assets: Arc::clone(&self.media.lock().unwrap()),
        }
    }

    pub(crate) fn protocol_is_open(&self) -> bool {
        *self.protocol_open.lock().unwrap()
    }

    pub(crate) fn close_protocol(&self) {
        *self.protocol_open.lock().unwrap() = false;
    }

    pub(crate) fn set(&self, revision: u64, chapter_id: String, href: String, html: String) {
        let mut state = self.pages.lock().unwrap();
        state.pages.insert(
            revision,
            EditorWebPage {
                chapter_id,
                href,
                html,
            },
        );
        while state.pages.len() > EDITOR_PAGE_HISTORY {
            let Some(oldest) = state.pages.keys().next().copied() else {
                break;
            };
            state.pages.remove(&oldest);
        }
    }

    pub(crate) fn page(&self, revision: u64) -> Option<EditorWebPage> {
        self.pages.lock().unwrap().pages.get(&revision).cloned()
    }

    pub(crate) fn apply_message(&self, message: EditorIpcMessage) -> Option<EditorIpcUpdate> {
        if message.session_id != self.session_id.as_ref()
            || !valid_editor_ipc_id(&message.chapter_id)
        {
            return None;
        }
        let selected_text = normalize_editor_selection(&message.selected_text)?;
        let media = self.media_snapshot();
        let mut state = self.pages.lock().unwrap();
        let page = state.pages.get_mut(&message.revision)?;
        if page.chapter_id != message.chapter_id || !editor_hrefs_match(&page.href, &message.href) {
            return None;
        }
        let href = page.href.clone();
        if message.ready {
            return Some(EditorIpcUpdate {
                session_id: message.session_id,
                chapter_id: message.chapter_id,
                href,
                revision: message.revision,
                request_id: message.request_id,
                html: page.html.clone(),
                selected_text,
                too_large: false,
                ready: true,
                edited: false,
            });
        }
        if message.too_large
            || message
                .body
                .as_ref()
                .is_some_and(|body| body.len() > MAX_EDITOR_BODY_BYTES)
        {
            return Some(EditorIpcUpdate {
                session_id: message.session_id,
                chapter_id: message.chapter_id,
                href,
                revision: message.revision,
                request_id: message.request_id,
                html: page.html.clone(),
                selected_text,
                too_large: true,
                ready: false,
                edited: false,
            });
        }
        let body_was_some = message.body.is_some();
        let html = match message.body {
            Some(body) => {
                let body = canonicalize_editor_asset_urls(&body, &media.assets);
                replace_body_element(&page.html, &body)?
            }
            None => page.html.clone(),
        };
        page.html = html.clone();
        Some(EditorIpcUpdate {
            session_id: message.session_id,
            chapter_id: message.chapter_id,
            href,
            revision: message.revision,
            request_id: message.request_id,
            html,
            selected_text,
            too_large: false,
            ready: false,
            edited: body_was_some,
        })
    }
}
