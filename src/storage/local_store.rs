//! The blob store trait implementation for the local store.

use super::*;

impl BlobStore for LocalBlobStore {
    fn put<'a>(&'a self, bytes: &'a [u8]) -> BlobFuture<'a, BlobKey> {
        Box::pin(async move { self.put_impl(bytes).await })
    }

    fn get<'a>(&'a self, key: &'a BlobKey) -> BlobFuture<'a, Vec<u8>> {
        Box::pin(async move { self.get_impl(key).await })
    }

    fn get_range<'a>(&'a self, key: &'a BlobKey, range: Range<u64>) -> BlobFuture<'a, Vec<u8>> {
        Box::pin(async move { self.get_range_impl(key, range).await })
    }

    fn delete<'a>(&'a self, key: &'a BlobKey) -> BlobFuture<'a, ()> {
        Box::pin(async move { self.delete_impl(key).await })
    }

    fn exists<'a>(&'a self, key: &'a BlobKey) -> BlobFuture<'a, bool> {
        Box::pin(async move { self.exists_impl(key).await })
    }
}
