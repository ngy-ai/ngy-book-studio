//! Checking that an existing object really belongs to this store.

use super::*;

impl LocalBlobStore {
    /// Returns `false` only when a canonical object path is absent. Any
    /// non-directory ancestor, symlink/reparse point, or root escape is an
    /// error rather than being treated as an absent object.
    pub(crate) async fn validate_existing_object(&self, key: &BlobKey) -> Result<bool> {
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

    pub(crate) async fn validate_root(&self) -> Result<()> {
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

    pub(crate) async fn validate_listed_file(&self, location: &ObjectPath) -> Result<()> {
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

    pub(crate) async fn validate_existing_directory(
        &self,
        path: &Path,
        label: &str,
    ) -> Result<bool> {
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

    pub(crate) async fn validate_existing_file(&self, path: &Path, label: &str) -> Result<bool> {
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

    pub(crate) fn ensure_lexically_within_root(&self, path: &Path, label: &str) -> Result<()> {
        ensure!(
            path != self.root && path.starts_with(&self.root),
            "{label}超出 Blob 根目录：{}",
            path.display()
        );
        Ok(())
    }

    pub(crate) async fn ensure_canonical_within_root(
        &self,
        path: &Path,
        label: &str,
    ) -> Result<()> {
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
