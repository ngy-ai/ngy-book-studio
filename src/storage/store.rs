//! The blob store trait the rest of the app writes through.

use super::*;

/// Application-owned object storage boundary.
///
/// The boxed futures keep this trait object-safe without coupling callers to
/// `async_trait`, so application services can hold an `Arc<dyn BlobStore>` and
/// a later S3 implementation can use the same API.
pub trait BlobStore: Send + Sync {
    fn put<'a>(&'a self, bytes: &'a [u8]) -> BlobFuture<'a, BlobKey>;

    fn get<'a>(&'a self, key: &'a BlobKey) -> BlobFuture<'a, Vec<u8>>;

    /// Reads the strict half-open byte range `[start, end)`.
    ///
    /// Empty, reversed, and out-of-bounds ranges are rejected instead of being
    /// silently truncated by a backend.
    fn get_range<'a>(&'a self, key: &'a BlobKey, range: Range<u64>) -> BlobFuture<'a, Vec<u8>>;

    /// Deletes an object. Deleting a missing object is successful.
    fn delete<'a>(&'a self, key: &'a BlobKey) -> BlobFuture<'a, ()>;

    fn exists<'a>(&'a self, key: &'a BlobKey) -> BlobFuture<'a, bool>;
}
