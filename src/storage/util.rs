//! Digest, shard and link/reparse-point helpers.

use super::*;

pub(crate) fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn canonical_shard(prefix: &ObjectPath) -> Option<&str> {
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

pub(crate) fn ensure_blob_digest(key: &BlobKey, bytes: &[u8]) -> Result<()> {
    let actual = blake3::hash(bytes).to_hex();
    ensure!(
        actual.as_str() == key.digest(),
        "Blob 内容与键中的 BLAKE3 摘要不一致：{key}"
    );
    Ok(())
}

pub(crate) fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
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
