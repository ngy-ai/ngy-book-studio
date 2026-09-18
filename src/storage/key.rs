//! The validated blob key and the boxed future the store returns.

use super::*;

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

    pub(crate) fn shard(&self) -> &str {
        self.0
            .split('/')
            .nth(1)
            .expect("BlobKey is canonical by construction")
    }

    pub(crate) fn digest(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map(|(_, digest)| digest)
            .expect("BlobKey is canonical by construction")
    }

    pub(crate) fn object_path(&self) -> Result<ObjectPath> {
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
