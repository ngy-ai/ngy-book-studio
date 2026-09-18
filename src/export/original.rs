//! Exporting the original file and verifying the assets it points at.

use super::*;

pub(crate) fn export_original(
    document: &BookDocument,
    assets: &dyn AssetResolver,
) -> Result<Vec<u8>> {
    let BookSource::Imported {
        original_asset_id, ..
    } = &document.source
    else {
        bail!("这本图书是应用内新建的，没有可导出的原文件；请选择 EPUB 或 PDF")
    };
    let metadata = document
        .find_asset(original_asset_id)
        .context("图书记录缺少原文件对象")?;
    ensure!(
        metadata.has_role(AssetRole::OriginalSource),
        "原文件对象的用途标记无效"
    );
    resolve_verified(metadata, assets)
}

pub(crate) fn resolve_verified(
    metadata: &AssetRef,
    resolver: &dyn AssetResolver,
) -> Result<Vec<u8>> {
    let bytes = resolver
        .resolve(&metadata.id)
        .with_context(|| format!("无法解析资源 {}", metadata.id))?;
    ensure!(
        bytes.len() as u64 == metadata.byte_len,
        "资源 {} 的长度与数据库记录不一致",
        metadata.id
    );
    let digest = blake3::hash(&bytes).to_hex().to_string();
    ensure!(
        digest.eq_ignore_ascii_case(&metadata.content_hash),
        "资源 {} 的 BLAKE3 摘要与数据库记录不一致",
        metadata.id
    );
    Ok(bytes)
}

pub(crate) struct EpubAsset<'a> {
    pub(crate) metadata: &'a AssetRef,
    pub(crate) manifest_id: String,
    pub(crate) href: String,
    pub(crate) bytes: Vec<u8>,
}
