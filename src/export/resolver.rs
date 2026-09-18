//! The asset resolver and the document exporter traits.

use super::*;

/// Resolves an opaque domain asset ID to immutable bytes.
///
/// Implementations may use SQLite plus [`crate::storage::BlobStore`], memory,
/// or a future remote backend. Paths and object-store keys remain outside the
/// document/export boundary.
pub trait AssetResolver: Send + Sync {
    fn resolve(&self, asset_id: &str) -> Result<Vec<u8>>;
}

impl<F> AssetResolver for F
where
    F: Fn(&str) -> Result<Vec<u8>> + Send + Sync,
{
    fn resolve(&self, asset_id: &str) -> Result<Vec<u8>> {
        self(asset_id)
    }
}

/// Narrow export boundary shared by library, reader, and editor workflows.
///
/// This operation is intentionally synchronous. Callers must run it on the
/// application I/O runtime or another worker, never in a GPUI render/update
/// borrow.
pub trait DocumentExporter: Send + Sync {
    fn export(
        &self,
        document: &BookDocument,
        format: ExportFormat,
        assets: &dyn AssetResolver,
        target: &Path,
    ) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinDocumentExporter;

/// Convenient name for the built-in stable exporter.
pub type DefaultDocumentExporter = BuiltinDocumentExporter;
