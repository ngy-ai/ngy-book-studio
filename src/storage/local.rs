//! The local on-disk blob store: construction and orphan sweeping.

use super::*;

/// Local durable implementation backed by [`LocalFileSystem`].
///
/// Object writes use `PutMode::Create`, which publishes a complete staged file
/// atomically and never overwrites an existing content-addressed object.
#[derive(Clone, Debug)]
pub struct LocalBlobStore {
    /// Filesystem spelling emitted by `LocalFileSystem` (for example `C:\...`).
    pub(crate) root: PathBuf,
    /// Canonical spelling used after following filesystem metadata (for
    /// example Windows may return `\\?\C:\...`).
    pub(crate) canonical_root: PathBuf,
    pub(crate) backend: LocalFileSystem,
}

impl LocalBlobStore {
    /// Creates a store rooted at an explicit absolute directory.
    ///
    /// This is the only synchronous filesystem operation: it prepares and
    /// canonicalizes the root required by `LocalFileSystem::new_with_prefix`.
    /// Blob I/O itself is asynchronous and runs through `object_store`/Tokio.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let requested_root = root.as_ref();
        ensure!(
            !requested_root.as_os_str().is_empty(),
            "Blob 根目录不能为空"
        );
        ensure!(
            requested_root.is_absolute(),
            "Blob 根目录必须是绝对路径：{}",
            requested_root.display()
        );

        fs::create_dir_all(requested_root)
            .with_context(|| format!("无法创建 Blob 根目录：{}", requested_root.display()))?;
        let metadata = fs::symlink_metadata(requested_root)
            .with_context(|| format!("无法检查 Blob 根目录：{}", requested_root.display()))?;
        ensure!(
            !metadata_is_link_or_reparse_point(&metadata),
            "Blob 根目录不能是符号链接或重解析点：{}",
            requested_root.display()
        );
        ensure!(
            metadata.is_dir(),
            "Blob 根路径不是目录：{}",
            requested_root.display()
        );

        let canonical_root = fs::canonicalize(requested_root)
            .with_context(|| format!("无法规范化 Blob 根目录：{}", requested_root.display()))?;
        let backend = LocalFileSystem::new_with_prefix(&canonical_root)
            .with_context(|| format!("无法初始化本地对象存储：{}", canonical_root.display()))?
            .with_automatic_cleanup(true)
            .with_fsync(true);
        // LocalFileSystem intentionally rejects the empty object path, so map
        // a harmless child and take its parent to obtain its native path
        // spelling without touching the filesystem.
        let root_probe =
            ObjectPath::parse("__ngy_root_probe__").context("无法创建本地对象存储根目录探针")?;
        let root = backend
            .path_to_filesystem(&root_probe)
            .context("无法解析本地对象存储根目录")?
            .parent()
            .context("本地对象存储根目录探针没有父目录")?
            .to_path_buf();

        Ok(Self {
            root,
            canonical_root,
            backend,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Enumerates canonical stored objects that are absent from `referenced`.
    ///
    /// Unknown files are deliberately left alone. Symlinks and reparse points
    /// inside the managed namespace are errors and are never followed.
    pub async fn list_orphans<'a>(
        &self,
        referenced: impl IntoIterator<Item = &'a BlobKey>,
    ) -> Result<Vec<BlobKey>> {
        let referenced = referenced.into_iter().cloned().collect::<HashSet<_>>();
        let mut orphans = self
            .list_keys()
            .await?
            .into_iter()
            .filter(|key| !referenced.contains(key))
            .collect::<Vec<_>>();
        orphans.sort();
        Ok(orphans)
    }

    /// Deletes objects absent from a caller-provided reference snapshot.
    ///
    /// The caller must serialize this garbage-collection pass with database
    /// reference updates so an object cannot become referenced between listing
    /// and deletion. The returned keys are deterministic and sorted.
    pub async fn delete_orphans<'a>(
        &self,
        referenced: impl IntoIterator<Item = &'a BlobKey>,
    ) -> Result<Vec<BlobKey>> {
        let orphans = self.list_orphans(referenced).await?;
        for key in &orphans {
            self.delete_impl(key)
                .await
                .with_context(|| format!("无法删除孤儿 Blob：{key}"))?;
        }
        Ok(orphans)
    }
}
