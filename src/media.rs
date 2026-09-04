//! Authorized media responses for the custom WebView protocol.

use std::ops::Range;

use anyhow::{Result, bail, ensure};

use crate::library::LibraryStore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaMetadata {
    pub media_type: String,
    pub byte_len: u64,
}

pub trait MediaBackend: Send + Sync {
    fn metadata(&self, book_id: &str, asset_id: &str) -> Result<MediaMetadata>;
    fn read(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>>;
    fn read_range(&self, book_id: &str, asset_id: &str, range: Range<u64>) -> Result<Vec<u8>>;
}

impl MediaBackend for LibraryStore {
    fn metadata(&self, book_id: &str, asset_id: &str) -> Result<MediaMetadata> {
        let (media_type, byte_len) = self.asset_metadata(book_id, asset_id)?;
        Ok(MediaMetadata {
            media_type,
            byte_len,
        })
    }

    fn read(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
        self.asset_bytes(book_id, asset_id)
    }

    fn read_range(&self, book_id: &str, asset_id: &str, range: Range<u64>) -> Result<Vec<u8>> {
        self.asset_range(book_id, asset_id, range)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaResponse {
    pub status: u16,
    pub media_type: String,
    pub accept_ranges: &'static str,
    pub content_length: u64,
    pub content_range: Option<String>,
    pub body: Vec<u8>,
}

impl MediaResponse {
    fn range_not_satisfiable(total: u64, media_type: String) -> Self {
        Self {
            status: 416,
            media_type,
            accept_ranges: "bytes",
            content_length: 0,
            content_range: Some(format!("bytes */{total}")),
            body: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MediaService<B> {
    backend: B,
}

impl<B> MediaService<B>
where
    B: MediaBackend,
{
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Serves one asset. The backend must perform the book/asset ownership
    /// check before any object read; callers must not translate that failure
    /// into another book lookup.
    pub fn serve(
        &self,
        book_id: &str,
        asset_id: &str,
        range_header: Option<&str>,
    ) -> Result<MediaResponse> {
        ensure!(!book_id.trim().is_empty(), "book ID is required");
        ensure!(!asset_id.trim().is_empty(), "asset ID is required");
        let metadata = self.backend.metadata(book_id, asset_id)?;
        let media_type = safe_media_type(&metadata.media_type);
        let Some(range_header) = range_header else {
            let body = self.backend.read(book_id, asset_id)?;
            ensure!(
                body.len() as u64 == metadata.byte_len,
                "media object length does not match metadata"
            );
            return Ok(MediaResponse {
                status: 200,
                media_type,
                accept_ranges: "bytes",
                content_length: metadata.byte_len,
                content_range: None,
                body,
            });
        };
        let range = match parse_single_range(range_header, metadata.byte_len) {
            Ok(range) => range,
            Err(_) => {
                return Ok(MediaResponse::range_not_satisfiable(
                    metadata.byte_len,
                    media_type,
                ));
            }
        };
        let body = self.backend.read_range(book_id, asset_id, range.clone())?;
        let expected = range.end - range.start;
        ensure!(
            body.len() as u64 == expected,
            "media range length does not match the requested interval"
        );
        Ok(MediaResponse {
            status: 206,
            media_type,
            accept_ranges: "bytes",
            content_length: expected,
            content_range: Some(format!(
                "bytes {}-{}/{}",
                range.start,
                range.end - 1,
                metadata.byte_len
            )),
            body,
        })
    }
}

fn parse_single_range(header: &str, total: u64) -> Result<Range<u64>> {
    let value = header.trim();
    let value = value
        .strip_prefix("bytes=")
        .ok_or_else(|| anyhow::anyhow!("only byte ranges are supported"))?;
    if total == 0 || value.contains(',') {
        bail!("empty objects and multiple ranges are not satisfiable");
    }
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("malformed byte range"))?;
    if start.is_empty() {
        let suffix = end.parse::<u64>()?;
        ensure!(suffix > 0, "suffix range must not be empty");
        let start = total.saturating_sub(suffix.min(total));
        return Ok(start..total);
    }
    let start = start.parse::<u64>()?;
    ensure!(start < total, "range starts past the object");
    let inclusive_end = if end.is_empty() {
        total - 1
    } else {
        end.parse::<u64>()?.min(total - 1)
    };
    ensure!(inclusive_end >= start, "range end precedes start");
    Ok(start..inclusive_end + 1)
}

fn safe_media_type(value: &str) -> String {
    let value = value.split(';').next().unwrap_or(value).trim();
    let Some((kind, subtype)) = value.split_once('/') else {
        return "application/octet-stream".to_string();
    };
    let valid_token = |token: &str| {
        !token.is_empty()
            && token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    if !value.is_ascii() || !valid_token(kind) || !valid_token(subtype) {
        return "application/octet-stream".to_string();
    }
    format!(
        "{}/{}",
        kind.to_ascii_lowercase(),
        subtype.to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use super::*;

    #[derive(Clone)]
    struct MemoryBackend {
        owner: String,
        values: Arc<HashMap<String, (String, Vec<u8>)>>,
    }

    impl MediaBackend for MemoryBackend {
        fn metadata(&self, book_id: &str, asset_id: &str) -> Result<MediaMetadata> {
            ensure!(book_id == self.owner, "forbidden");
            let (media_type, bytes) = self.values.get(asset_id).context("missing")?;
            Ok(MediaMetadata {
                media_type: media_type.clone(),
                byte_len: bytes.len() as u64,
            })
        }

        fn read(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
            ensure!(book_id == self.owner, "forbidden");
            Ok(self.values.get(asset_id).context("missing")?.1.clone())
        }

        fn read_range(&self, book_id: &str, asset_id: &str, range: Range<u64>) -> Result<Vec<u8>> {
            let bytes = self.read(book_id, asset_id)?;
            Ok(bytes[range.start as usize..range.end as usize].to_vec())
        }
    }

    use anyhow::{Context as _, ensure};

    fn service() -> MediaService<MemoryBackend> {
        MediaService::new(MemoryBackend {
            owner: "book-a".to_string(),
            values: Arc::new(HashMap::from([(
                "audio".to_string(),
                ("audio/mpeg".to_string(), b"0123456789".to_vec()),
            )])),
        })
    }

    #[test]
    fn full_and_partial_responses_have_http_range_metadata() {
        let full = service().serve("book-a", "audio", None).unwrap();
        assert_eq!((full.status, full.content_length), (200, 10));
        let partial = service()
            .serve("book-a", "audio", Some("bytes=2-5"))
            .unwrap();
        assert_eq!(partial.status, 206);
        assert_eq!(partial.body, b"2345");
        assert_eq!(partial.content_range.as_deref(), Some("bytes 2-5/10"));
    }

    #[test]
    fn suffix_open_and_unsatisfiable_ranges_are_handled() {
        assert_eq!(
            service()
                .serve("book-a", "audio", Some("bytes=-3"))
                .unwrap()
                .body,
            b"789"
        );
        assert_eq!(
            service()
                .serve("book-a", "audio", Some("bytes=8-"))
                .unwrap()
                .body,
            b"89"
        );
        let invalid = service()
            .serve("book-a", "audio", Some("bytes=20-30"))
            .unwrap();
        assert_eq!(invalid.status, 416);
        assert_eq!(invalid.content_range.as_deref(), Some("bytes */10"));
    }

    #[test]
    fn ownership_is_checked_before_reading() {
        assert!(service().serve("book-b", "audio", None).is_err());
    }

    #[test]
    fn response_media_type_is_a_safe_normalized_base_type() {
        let backend = MemoryBackend {
            owner: "book-a".to_string(),
            values: Arc::new(HashMap::from([
                (
                    "parameterized".to_string(),
                    ("Audio/MPEG; charset=binary".to_string(), b"data".to_vec()),
                ),
                (
                    "injected".to_string(),
                    ("audio/mpeg\r\nx-test: true".to_string(), b"data".to_vec()),
                ),
            ])),
        };
        let service = MediaService::new(backend);
        assert_eq!(
            service
                .serve("book-a", "parameterized", None)
                .unwrap()
                .media_type,
            "audio/mpeg"
        );
        assert_eq!(
            service
                .serve("book-a", "injected", None)
                .unwrap()
                .media_type,
            "application/octet-stream"
        );
    }
}
