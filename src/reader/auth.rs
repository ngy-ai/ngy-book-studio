//! Authorizing which resources a reader session may load.

use super::*;

/// Immutable authorization produced from one persisted document revision and
/// its EPUB reader projection. A path is present only when its manifest bytes
/// exactly match an asset owned by that document.
#[derive(Clone, Debug, Default)]
pub struct ReaderResourceAuthorizations {
    pub(crate) book_id: String,
    pub(crate) assets_by_path: Arc<HashMap<String, AuthorizedReaderAsset>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedReaderAsset {
    pub asset_id: String,
    pub media_type: String,
    pub byte_len: u64,
    pub content_hash: String,
}

impl ReaderResourceAuthorizations {
    pub fn for_document(epub: &Epub, document: &BookDocument) -> Result<Self> {
        document.validate()?;
        let mut candidates = HashMap::<(&str, u64), Vec<&crate::document::AssetRef>>::new();
        for asset in document.assets.iter().filter(|asset| {
            !asset.roles.contains(&AssetRole::OriginalSource)
                && normalize_mime(&asset.media_type).is_some()
        }) {
            candidates
                .entry((asset.content_hash.as_str(), asset.byte_len))
                .or_default()
                .push(asset);
        }

        let mut assets_by_path = HashMap::new();
        for entry in epub.manifest().iter() {
            let href = entry.href().as_str();
            let request_path = if href.starts_with('/') {
                href.to_string()
            } else {
                format!("/{href}")
            };
            let Ok(path_key) = canonical_resource_path(&request_path) else {
                continue;
            };
            let safe_path = safe_resource_path(&request_path)?;
            let media_type = resource_mime(entry.media_type(), &safe_path);
            if is_html_mime(&media_type) {
                continue;
            }
            let bytes = match entry.read_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(href, %error, "cannot authorize unreadable EPUB resource");
                    continue;
                }
            };
            let content_hash = blake3::hash(&bytes).to_hex().to_string();
            let Some(matches) = candidates.get(&(content_hash.as_str(), bytes.len() as u64)) else {
                continue;
            };
            let Some(asset) = matches
                .iter()
                .copied()
                .find(|asset| normalize_mime(&asset.media_type).as_deref() == Some(&media_type))
            else {
                continue;
            };
            assets_by_path.insert(
                path_key,
                AuthorizedReaderAsset {
                    asset_id: asset.id.clone(),
                    media_type,
                    byte_len: asset.byte_len,
                    content_hash,
                },
            );
        }

        Ok(Self {
            book_id: document.id.clone(),
            assets_by_path: Arc::new(assets_by_path),
        })
    }

    pub fn book_id(&self) -> &str {
        &self.book_id
    }

    pub fn asset_for_path(&self, request_path: &str) -> Result<Option<AuthorizedReaderAsset>> {
        let path = canonical_resource_path(request_path)?;
        Ok(self.assets_by_path.get(&path).cloned())
    }
}
