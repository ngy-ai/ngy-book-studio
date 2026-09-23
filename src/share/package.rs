//! Reading and writing the `.ngyshare` ZIP container.

use std::{
    io::{Cursor, Read, Seek, Write},
    path::Path,
};

use anyhow::{Context as _, Result, ensure};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::{
    BOOK_ENTRY, CHATS_ENTRY, MANIFEST_ENTRY, MAX_SHARE_BOOK_BYTES, MAX_SHARE_ENTRIES,
    MAX_SHARE_PACKAGE_BYTES, MAX_SHARE_SECTION_BYTES, NOTES_ENTRY, TRANSLATIONS_ENTRY,
    manifest::ShareManifest,
    sections::{SharedChatThread, SharedNote, SharedTranslation},
};

/// One fully parsed share package. Sections are empty when the package did not
/// carry them, which is what an export with nothing to share produces.
#[derive(Clone, Debug)]
pub struct SharePackage {
    pub manifest: ShareManifest,
    pub book_epub: Vec<u8>,
    pub notes: Vec<SharedNote>,
    pub chats: Vec<SharedChatThread>,
    pub translations: Vec<SharedTranslation>,
}

/// Reads a package, refusing anything the manifest does not describe exactly.
pub fn read_share_package(bytes: &[u8]) -> Result<SharePackage> {
    ensure!(
        bytes.len() as u64 <= MAX_SHARE_PACKAGE_BYTES,
        "分享包超过 {} 字节上限",
        MAX_SHARE_PACKAGE_BYTES
    );
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).context("无法读取分享包（不是有效的压缩包）")?;
    ensure!(archive.len() <= MAX_SHARE_ENTRIES, "分享包包含过多条目");
    let manifest_bytes = read_entry(&mut archive, MANIFEST_ENTRY, MAX_SHARE_SECTION_BYTES)?
        .context("分享包缺少 manifest.json")?;
    let manifest: ShareManifest =
        serde_json::from_slice(&manifest_bytes).context("分享包清单无法解析")?;
    manifest.validate()?;
    let book_epub = read_entry(&mut archive, BOOK_ENTRY, MAX_SHARE_BOOK_BYTES)?
        .context("分享包缺少图书正文")?;
    ensure!(!book_epub.is_empty(), "分享包的图书正文为空");
    let notes: Vec<SharedNote> = read_section(&mut archive, NOTES_ENTRY)?;
    let chats: Vec<SharedChatThread> = read_section(&mut archive, CHATS_ENTRY)?;
    let translations: Vec<SharedTranslation> = read_section(&mut archive, TRANSLATIONS_ENTRY)?;
    ensure!(
        notes.len() == manifest.notes.count
            && chats.len() == manifest.chats.count
            && translations.len() == manifest.translations.count,
        "分享包条目数与清单不一致，文件可能已损坏"
    );
    Ok(SharePackage {
        manifest,
        book_epub,
        notes,
        chats,
        translations,
    })
}

/// Builds the ZIP container for one export.
pub fn write_share_package(
    manifest: &ShareManifest,
    book_epub: &[u8],
    notes: &[SharedNote],
    chats: &[SharedChatThread],
    translations: &[SharedTranslation],
) -> Result<Vec<u8>> {
    ensure!(!book_epub.is_empty(), "没有可写入分享包的图书正文");
    manifest.validate()?;
    ensure!(
        notes.len() == manifest.notes.count
            && chats.len() == manifest.chats.count
            && translations.len() == manifest.translations.count,
        "分享包清单与实际条目不一致"
    );
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let json_options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    // The book projection is itself a ZIP, so deflating it costs time and buys
    // almost nothing; the JSON sections compress well.
    let book_options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    write_json(&mut writer, MANIFEST_ENTRY, manifest, json_options)?;
    writer
        .start_file(BOOK_ENTRY, book_options)
        .context("无法写入分享包图书正文")?;
    writer
        .write_all(book_epub)
        .context("无法写入分享包图书正文")?;
    write_json(&mut writer, NOTES_ENTRY, &notes, json_options)?;
    write_json(&mut writer, CHATS_ENTRY, &chats, json_options)?;
    write_json(&mut writer, TRANSLATIONS_ENTRY, &translations, json_options)?;
    let cursor = writer.finish().context("无法完成分享包")?;
    Ok(cursor.into_inner())
}

/// Cheap signature test for a file the user picked: a ZIP whose `manifest.json`
/// names this format. Used before spending an import attempt on it.
pub fn is_share_package(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return false;
    };
    let Ok(mut entry) = archive.by_name(MANIFEST_ENTRY) else {
        return false;
    };
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(MAX_SHARE_SECTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_ok()
        && serde_json::from_slice::<ShareManifest>(&bytes)
            .is_ok_and(|manifest| manifest.format == super::SHARE_FORMAT)
}

fn write_json<T: serde::Serialize>(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    name: &str,
    value: &T,
    options: SimpleFileOptions,
) -> Result<()> {
    let bytes =
        serde_json::to_vec(value).with_context(|| format!("无法序列化分享包条目 {name}"))?;
    ensure!(
        bytes.len() as u64 <= MAX_SHARE_SECTION_BYTES,
        "分享包条目 {name} 超过大小上限"
    );
    writer
        .start_file(name, options)
        .with_context(|| format!("无法写入分享包条目 {name}"))?;
    writer
        .write_all(&bytes)
        .with_context(|| format!("无法写入分享包条目 {name}"))?;
    Ok(())
}

fn read_section<T: serde::de::DeserializeOwned, R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<Vec<T>> {
    let Some(bytes) = read_entry(archive, name, MAX_SHARE_SECTION_BYTES)? else {
        return Ok(Vec::new());
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&bytes).with_context(|| format!("分享包条目 {name} 无法解析"))
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Option<Vec<u8>>> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取分享包条目 {name}"));
        }
    };
    ensure!(entry.size() <= limit, "分享包条目 {name} 超过大小上限");
    let capacity = usize::try_from(entry.size().min(4 * 1024 * 1024)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("无法读取分享包条目 {name}"))?;
    ensure!(
        bytes.len() as u64 <= limit,
        "分享包条目 {name} 超过大小上限"
    );
    Ok(Some(bytes))
}
