//! Reading and writing blobs on disk.

use super::*;

impl LocalBlobStore {
    pub(crate) async fn put_impl(&self, bytes: &[u8]) -> Result<BlobKey> {
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

    pub(crate) async fn get_impl(&self, key: &BlobKey) -> Result<Vec<u8>> {
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

    pub(crate) async fn get_range_impl(&self, key: &BlobKey, range: Range<u64>) -> Result<Vec<u8>> {
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

    pub(crate) async fn delete_impl(&self, key: &BlobKey) -> Result<()> {
        if !self.validate_existing_object(key).await? {
            return Ok(());
        }

        let location = key.object_path()?;
        match self.backend.delete(&location).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(error).with_context(|| format!("无法删除 Blob：{key}")),
        }
    }

    pub(crate) async fn exists_impl(&self, key: &BlobKey) -> Result<bool> {
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

    pub(crate) async fn list_keys(&self) -> Result<Vec<BlobKey>> {
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
}
