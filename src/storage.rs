use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    future::Future,
    ops::Range,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use anyhow::{Context as _, Result, ensure};
use object_store::{
    GetOptions, GetRange, ObjectStore as _, ObjectStoreExt as _, PutMode, PutOptions,
    local::LocalFileSystem, path::Path as ObjectPath,
};

const HASH_ALGORITHM: &str = "blake3";
const DIGEST_HEX_LEN: usize = 64;
const SHARD_HEX_LEN: usize = 2;

type PublicationGate = tokio::sync::Mutex<()>;

static PUBLICATION_GATES: OnceLock<StdMutex<HashMap<PathBuf, Weak<PublicationGate>>>> =
    OnceLock::new();

/// Serializes the two halves of publishing and reclaiming content-addressed
/// objects for one canonical store root.
///
/// A publisher holds this gate from the first immutable `put` through the
/// SQLite transaction that installs its references. A reclaimer holds it while
/// re-checking SQLite immediately before deleting bytes. The process registry
/// makes independently opened [`LocalBlobStore`] handles for the same root use
/// the same gate, while `Clone` remains a cheap `Arc` clone.
#[derive(Clone)]
pub(crate) struct BlobPublicationLock {
    gate: Arc<PublicationGate>,
    #[cfg(test)]
    next_acquire_observer: Arc<StdMutex<Option<std::sync::mpsc::Sender<()>>>>,
}

impl fmt::Debug for BlobPublicationLock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlobPublicationLock")
            .finish_non_exhaustive()
    }
}

impl BlobPublicationLock {
    pub(crate) fn for_store(store: &LocalBlobStore) -> Result<Self> {
        let registry = PUBLICATION_GATES.get_or_init(|| StdMutex::new(HashMap::new()));
        let mut registry = registry
            .lock()
            .map_err(|_| anyhow::anyhow!("Blob publication lock registry is poisoned"))?;
        registry.retain(|_, gate| gate.strong_count() != 0);
        let gate = match registry.get(&store.canonical_root).and_then(Weak::upgrade) {
            Some(gate) => gate,
            None => {
                let gate = Arc::new(PublicationGate::new(()));
                registry.insert(store.canonical_root.clone(), Arc::downgrade(&gate));
                gate
            }
        };
        Ok(Self {
            gate,
            #[cfg(test)]
            next_acquire_observer: Arc::new(StdMutex::new(None)),
        })
    }

    pub(crate) async fn acquire(&self) -> tokio::sync::OwnedMutexGuard<()> {
        #[cfg(test)]
        if let Ok(mut observer) = self.next_acquire_observer.lock()
            && let Some(observer) = observer.take()
        {
            let _ = observer.send(());
        }
        Arc::clone(&self.gate).lock_owned().await
    }

    #[cfg(test)]
    pub(crate) fn observe_next_acquire(&self, observer: std::sync::mpsc::Sender<()>) {
        *self
            .next_acquire_observer
            .lock()
            .expect("publication acquire observer lock is available") = Some(observer);
    }
}

/// Future returned by the object-safe [`BlobStore`] interface.
pub type BlobFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A canonical, backend-neutral content-addressed object key.
///
/// Keys use `blake3/ab/<64 lowercase hex characters>`. Keeping the key free of
/// platform path syntax makes it safe to use with both a local object store and
/// a future S3-compatible backend.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlobKey(String);

impl BlobKey {
    /// Computes the canonical BLAKE3 key for `bytes`.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = blake3::hash(bytes).to_hex().to_string();
        Self(format!(
            "{HASH_ALGORITHM}/{}/{}",
            &digest[..SHARD_HEX_LEN],
            digest
        ))
    }

    /// Parses an externally persisted key and rejects every non-canonical form.
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let mut segments = value.split('/');
        let algorithm = segments.next();
        let shard = segments.next();
        let digest = segments.next();

        ensure!(
            algorithm == Some(HASH_ALGORITHM)
                && shard.is_some()
                && digest.is_some()
                && segments.next().is_none(),
            "无效的 Blob 键：必须采用 {HASH_ALGORITHM}/ab/<digest> 格式"
        );

        let shard = shard.expect("checked above");
        let digest = digest.expect("checked above");
        ensure!(
            shard.len() == SHARD_HEX_LEN && is_lower_hex(shard),
            "无效的 Blob 键分片：必须是 2 位小写十六进制字符"
        );
        ensure!(
            digest.len() == DIGEST_HEX_LEN && is_lower_hex(digest),
            "无效的 Blob 摘要：必须是 64 位小写十六进制 BLAKE3 摘要"
        );
        ensure!(
            shard == &digest[..SHARD_HEX_LEN],
            "无效的 Blob 键：分片必须与摘要前两位一致"
        );

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn shard(&self) -> &str {
        self.0
            .split('/')
            .nth(1)
            .expect("BlobKey is canonical by construction")
    }

    fn digest(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map(|(_, digest)| digest)
            .expect("BlobKey is canonical by construction")
    }

    fn object_path(&self) -> Result<ObjectPath> {
        ObjectPath::parse(&self.0).with_context(|| format!("无法把 Blob 键转换为对象路径：{self}"))
    }
}

impl AsRef<str> for BlobKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for BlobKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for BlobKey {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl TryFrom<String> for BlobKey {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for BlobKey {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

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

/// Local durable implementation backed by [`LocalFileSystem`].
///
/// Object writes use `PutMode::Create`, which publishes a complete staged file
/// atomically and never overwrites an existing content-addressed object.
#[derive(Clone, Debug)]
pub struct LocalBlobStore {
    /// Filesystem spelling emitted by `LocalFileSystem` (for example `C:\...`).
    root: PathBuf,
    /// Canonical spelling used after following filesystem metadata (for
    /// example Windows may return `\\?\C:\...`).
    canonical_root: PathBuf,
    backend: LocalFileSystem,
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

    async fn put_impl(&self, bytes: &[u8]) -> Result<BlobKey> {
        let key = BlobKey::from_bytes(bytes);
        self.validate_existing_object(&key).await?;
        let location = key.object_path()?;
        let options = PutOptions {
            mode: PutMode::Create,
            ..PutOptions::default()
        };

        match self
            .backend
            .put_opts(&location, bytes.to_vec().into(), options)
            .await
        {
            Ok(_) => {
                ensure!(
                    self.validate_existing_object(&key).await?,
                    "对象存储报告写入成功，但 Blob 文件不存在：{key}"
                );
                Ok(key)
            }
            Err(object_store::Error::AlreadyExists { .. }) => {
                let existing = self
                    .get_impl(&key)
                    .await
                    .with_context(|| format!("同键 Blob 已存在但无法验证：{key}"))?;
                ensure!(
                    existing.as_slice() == bytes,
                    "同一 BLAKE3 Blob 键对应了不同内容：{key}"
                );
                Ok(key)
            }
            Err(error) => Err(error).with_context(|| format!("无法写入 Blob：{key}")),
        }
    }

    async fn get_impl(&self, key: &BlobKey) -> Result<Vec<u8>> {
        ensure!(
            self.validate_existing_object(key).await?,
            "Blob 不存在：{key}"
        );
        let location = key.object_path()?;
        let result = self
            .backend
            .get(&location)
            .await
            .with_context(|| format!("无法读取 Blob：{key}"))?;
        let bytes = result
            .bytes()
            .await
            .with_context(|| format!("无法接收 Blob 内容：{key}"))?
            .to_vec();
        ensure_blob_digest(key, &bytes)?;
        Ok(bytes)
    }

    async fn get_range_impl(&self, key: &BlobKey, range: Range<u64>) -> Result<Vec<u8>> {
        ensure!(
            range.start < range.end,
            "Blob 范围必须满足 start < end，实际为 {}..{}",
            range.start,
            range.end
        );
        ensure!(
            self.validate_existing_object(key).await?,
            "Blob 不存在：{key}"
        );

        let location = key.object_path()?;
        let metadata = self
            .backend
            .head(&location)
            .await
            .with_context(|| format!("无法读取 Blob 元数据：{key}"))?;
        ensure!(
            range.end <= metadata.size,
            "Blob 范围越界：请求 {}..{}，对象长度为 {}",
            range.start,
            range.end,
            metadata.size
        );

        let options = GetOptions::new().with_range(Some(GetRange::Bounded(range.clone())));
        let result = self
            .backend
            .get_opts(&location, options)
            .await
            .with_context(|| format!("无法读取 Blob 范围 {}..{}：{key}", range.start, range.end))?;
        let bytes = result
            .bytes()
            .await
            .with_context(|| format!("无法接收 Blob 范围 {}..{}：{key}", range.start, range.end))?;
        ensure!(
            bytes.len() as u64 == range.end - range.start,
            "Blob 范围读取长度不一致：请求 {} 字节，实际返回 {} 字节",
            range.end - range.start,
            bytes.len()
        );
        Ok(bytes.to_vec())
    }

    async fn delete_impl(&self, key: &BlobKey) -> Result<()> {
        if !self.validate_existing_object(key).await? {
            return Ok(());
        }

        let location = key.object_path()?;
        match self.backend.delete(&location).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(error).with_context(|| format!("无法删除 Blob：{key}")),
        }
    }

    async fn exists_impl(&self, key: &BlobKey) -> Result<bool> {
        if !self.validate_existing_object(key).await? {
            return Ok(false);
        }

        let location = key.object_path()?;
        match self.backend.head(&location).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(error).with_context(|| format!("无法检查 Blob：{key}")),
        }
    }

    async fn list_keys(&self) -> Result<Vec<BlobKey>> {
        self.validate_root().await?;
        let algorithm_prefix =
            ObjectPath::parse(HASH_ALGORITHM).context("无法创建 Blob 对象命名空间路径")?;
        let algorithm_path = self
            .backend
            .path_to_filesystem(&algorithm_prefix)
            .context("无法解析 Blob 对象命名空间目录")?;
        if !self
            .validate_existing_directory(&algorithm_path, "Blob 对象命名空间")
            .await?
        {
            return Ok(Vec::new());
        }

        let top_level = self
            .backend
            .list_with_delimiter(Some(&algorithm_prefix))
            .await
            .context("无法枚举 Blob 对象命名空间")?;

        // Validate even entries that do not match our layout. This makes a
        // malicious symlink visible without treating unknown regular files as
        // application-owned objects eligible for deletion.
        for object in &top_level.objects {
            self.validate_listed_file(&object.location).await?;
        }

        let mut keys = Vec::new();
        for shard_prefix in top_level.common_prefixes {
            let shard_path = self
                .backend
                .path_to_filesystem(&shard_prefix)
                .with_context(|| format!("无法解析 Blob 分片目录：{shard_prefix}"))?;
            self.validate_existing_directory(&shard_path, "Blob 分片目录")
                .await?;

            let Some(shard) = canonical_shard(&shard_prefix) else {
                continue;
            };
            let listing = self
                .backend
                .list_with_delimiter(Some(&shard_prefix))
                .await
                .with_context(|| format!("无法枚举 Blob 分片：{shard}"))?;

            for nested_prefix in listing.common_prefixes {
                let nested_path = self
                    .backend
                    .path_to_filesystem(&nested_prefix)
                    .with_context(|| format!("无法解析 Blob 子目录：{nested_prefix}"))?;
                self.validate_existing_directory(&nested_path, "Blob 非预期子目录")
                    .await?;
            }

            for object in listing.objects {
                self.validate_listed_file(&object.location).await?;
                let Ok(key) = BlobKey::parse(object.location.as_ref()) else {
                    continue;
                };
                if self.validate_existing_object(&key).await? {
                    keys.push(key);
                }
            }
        }

        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// Returns `false` only when a canonical object path is absent. Any
    /// non-directory ancestor, symlink/reparse point, or root escape is an
    /// error rather than being treated as an absent object.
    async fn validate_existing_object(&self, key: &BlobKey) -> Result<bool> {
        self.validate_root().await?;
        let algorithm_path = self.root.join(HASH_ALGORITHM);
        if !self
            .validate_existing_directory(&algorithm_path, "Blob 对象命名空间")
            .await?
        {
            return Ok(false);
        }

        let shard_path = algorithm_path.join(key.shard());
        if !self
            .validate_existing_directory(&shard_path, "Blob 分片目录")
            .await?
        {
            return Ok(false);
        }

        let location = key.object_path()?;
        let object_path = self
            .backend
            .path_to_filesystem(&location)
            .with_context(|| format!("无法解析 Blob 文件路径：{key}"))?;
        self.validate_existing_file(&object_path, "Blob 文件").await
    }

    async fn validate_root(&self) -> Result<()> {
        let metadata = tokio::fs::symlink_metadata(&self.root)
            .await
            .with_context(|| format!("无法检查 Blob 根目录：{}", self.root.display()))?;
        ensure!(
            !metadata_is_link_or_reparse_point(&metadata),
            "Blob 根目录不能是符号链接或重解析点：{}",
            self.root.display()
        );
        ensure!(
            metadata.is_dir(),
            "Blob 根路径不是目录：{}",
            self.root.display()
        );
        let canonical = tokio::fs::canonicalize(&self.root)
            .await
            .with_context(|| format!("无法规范化 Blob 根目录：{}", self.root.display()))?;
        ensure!(
            canonical == self.canonical_root,
            "Blob 根目录在初始化后发生了变化：{}",
            self.root.display()
        );
        Ok(())
    }

    async fn validate_listed_file(&self, location: &ObjectPath) -> Result<()> {
        let path = self
            .backend
            .path_to_filesystem(location)
            .with_context(|| format!("无法解析枚举到的 Blob 路径：{location}"))?;
        ensure!(
            self.validate_existing_file(&path, "枚举到的 Blob 文件")
                .await?,
            "枚举到的 Blob 文件在校验前已消失：{location}"
        );
        Ok(())
    }

    async fn validate_existing_directory(&self, path: &Path, label: &str) -> Result<bool> {
        self.ensure_lexically_within_root(path, label)?;
        let metadata = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("无法检查{label}：{}", path.display()));
            }
        };
        ensure!(
            !metadata_is_link_or_reparse_point(&metadata),
            "{label}不能是符号链接或重解析点：{}",
            path.display()
        );
        ensure!(metadata.is_dir(), "{label}不是目录：{}", path.display());
        self.ensure_canonical_within_root(path, label).await?;
        Ok(true)
    }

    async fn validate_existing_file(&self, path: &Path, label: &str) -> Result<bool> {
        self.ensure_lexically_within_root(path, label)?;
        let metadata = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("无法检查{label}：{}", path.display()));
            }
        };
        ensure!(
            !metadata_is_link_or_reparse_point(&metadata),
            "{label}不能是符号链接或重解析点：{}",
            path.display()
        );
        ensure!(
            metadata.is_file(),
            "{label}不是普通文件：{}",
            path.display()
        );
        self.ensure_canonical_within_root(path, label).await?;
        Ok(true)
    }

    fn ensure_lexically_within_root(&self, path: &Path, label: &str) -> Result<()> {
        ensure!(
            path != self.root && path.starts_with(&self.root),
            "{label}超出 Blob 根目录：{}",
            path.display()
        );
        Ok(())
    }

    async fn ensure_canonical_within_root(&self, path: &Path, label: &str) -> Result<()> {
        let canonical = tokio::fs::canonicalize(path)
            .await
            .with_context(|| format!("无法规范化{label}：{}", path.display()))?;
        ensure!(
            canonical != self.canonical_root && canonical.starts_with(&self.canonical_root),
            "{label}解析到了 Blob 根目录之外：{}",
            canonical.display()
        );
        Ok(())
    }
}

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

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_shard(prefix: &ObjectPath) -> Option<&str> {
    let mut parts = prefix.parts();
    let algorithm = parts.next()?;
    let shard = parts.next()?;
    if parts.next().is_some()
        || algorithm.as_ref() != HASH_ALGORITHM
        || shard.as_ref().len() != SHARD_HEX_LEN
        || !is_lower_hex(shard.as_ref())
    {
        return None;
    }
    prefix.filename()
}

fn ensure_blob_digest(key: &BlobKey, bytes: &[u8]) -> Result<()> {
    let actual = blake3::hash(bytes).to_hex();
    ensure!(
        actual.as_str() == key.digest(),
        "Blob 内容与键中的 BLAKE3 摘要不一致：{key}"
    );
    Ok(())
}

fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }

    #[cfg(not(target_os = "windows"))]
    false
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use tempfile::tempdir;

    use super::*;

    fn new_store() -> (tempfile::TempDir, LocalBlobStore) {
        let temporary = tempdir().expect("create temporary directory");
        let store =
            LocalBlobStore::new(temporary.path().join("objects")).expect("create local blob store");
        (temporary, store)
    }

    #[test]
    fn blob_key_parser_accepts_only_the_canonical_layout() {
        let digest = "a".repeat(DIGEST_HEX_LEN);
        let canonical = format!("blake3/aa/{digest}");
        assert_eq!(BlobKey::parse(&canonical).unwrap().as_str(), canonical);

        for invalid in [
            digest.clone(),
            format!("/blake3/aa/{digest}"),
            format!("blake3/../{digest}"),
            format!("blake3\\aa\\{digest}"),
            format!("blake3/ab/{digest}"),
            format!("blake3/aa/{}", "A".repeat(DIGEST_HEX_LEN)),
            format!("blake3/aa/{digest}/extra"),
            "C:/outside/blob".to_string(),
        ] {
            assert!(BlobKey::parse(&invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn local_store_rejects_relative_file_and_link_roots() {
        assert!(LocalBlobStore::new("relative/blob-root").is_err());

        let temporary = tempdir().unwrap();
        let file = temporary.path().join("not-a-directory");
        fs::write(&file, b"data").unwrap();
        assert!(LocalBlobStore::new(&file).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let target = temporary.path().join("target");
            fs::create_dir(&target).unwrap();
            let link = temporary.path().join("link");
            symlink(&target, &link).unwrap();
            assert!(LocalBlobStore::new(link).is_err());
        }
    }

    #[test]
    fn publication_lock_is_shared_by_independent_handles_for_one_root() {
        let (temporary, first_store) = new_store();
        let second_store = LocalBlobStore::new(temporary.path().join("objects")).unwrap();
        let first = BlobPublicationLock::for_store(&first_store).unwrap();
        let second = BlobPublicationLock::for_store(&second_store).unwrap();

        assert!(Arc::ptr_eq(&first.gate, &second.gate));
    }

    #[tokio::test]
    async fn trait_object_put_get_exists_and_delete_round_trip() {
        let (temporary, local) = new_store();
        let expected_root = fs::canonicalize(temporary.path().join("objects")).unwrap();
        assert_eq!(fs::canonicalize(local.root()).unwrap(), expected_root);

        let store: Arc<dyn BlobStore> = Arc::new(local);
        let bytes = b"content-addressed object";
        let key = store.put(bytes).await.unwrap();
        assert_eq!(key, BlobKey::from_bytes(bytes));
        assert!(store.exists(&key).await.unwrap());
        assert_eq!(store.get(&key).await.unwrap(), bytes);

        store.delete(&key).await.unwrap();
        store.delete(&key).await.unwrap();
        assert!(!store.exists(&key).await.unwrap());
    }

    #[tokio::test]
    async fn duplicate_put_is_atomic_and_idempotent() {
        let (_temporary, store) = new_store();
        let bytes = b"same immutable bytes";
        let (first, second) = tokio::join!(store.put(bytes), store.put(bytes));
        let first = first.unwrap();
        let second = second.unwrap();

        assert_eq!(first, second);
        assert_eq!(store.get(&first).await.unwrap(), bytes);
        assert!(store.list_orphans([&first]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicate_put_does_not_overwrite_corrupt_existing_data() {
        let (_temporary, store) = new_store();
        let original = b"original";
        let key = store.put(original).await.unwrap();
        let physical = store
            .backend
            .path_to_filesystem(&key.object_path().unwrap())
            .unwrap();
        tokio::fs::write(&physical, b"corrupt").await.unwrap();

        let error = store.put(original).await.unwrap_err().to_string();
        assert!(error.contains("无法验证"), "unexpected error: {error}");
        assert_eq!(tokio::fs::read(physical).await.unwrap(), b"corrupt");
    }

    #[tokio::test]
    async fn range_reads_are_strict_and_half_open() {
        let (_temporary, store) = new_store();
        let key = store.put(b"0123456789").await.unwrap();

        assert_eq!(store.get_range(&key, 2..7).await.unwrap(), b"23456");
        assert!(store.get_range(&key, 3..3).await.is_err());
        assert!(
            store
                .get_range(&key, std::ops::Range { start: 7, end: 3 })
                .await
                .is_err()
        );
        assert!(store.get_range(&key, 9..11).await.is_err());
        assert!(store.get_range(&key, 10..11).await.is_err());
    }

    #[tokio::test]
    async fn orphan_helpers_keep_referenced_and_unknown_files() {
        let (_temporary, store) = new_store();
        let retained = store.put(b"retained").await.unwrap();
        let orphan_a = store.put(b"orphan-a").await.unwrap();
        let orphan_b = store.put(b"orphan-b").await.unwrap();

        let unknown_directory = store.root().join(HASH_ALGORITHM).join("zz");
        tokio::fs::create_dir_all(&unknown_directory).await.unwrap();
        let unknown_file = unknown_directory.join("not-an-object");
        tokio::fs::write(&unknown_file, b"leave me alone")
            .await
            .unwrap();

        let mut expected = vec![orphan_a.clone(), orphan_b.clone()];
        expected.sort();
        assert_eq!(store.list_orphans([&retained]).await.unwrap(), expected);
        assert_eq!(store.delete_orphans([&retained]).await.unwrap(), expected);

        assert!(store.exists(&retained).await.unwrap());
        assert!(!store.exists(&orphan_a).await.unwrap());
        assert!(!store.exists(&orphan_b).await.unwrap());
        assert_eq!(
            tokio::fs::read(unknown_file).await.unwrap(),
            b"leave me alone"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_namespace_symlink_is_rejected_without_following_it() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let store = LocalBlobStore::new(temporary.path().join("objects")).unwrap();
        symlink(outside.path(), store.root().join(HASH_ALGORITHM)).unwrap();

        let error = store
            .put(b"must stay inside")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("符号链接"), "unexpected error: {error}");
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
