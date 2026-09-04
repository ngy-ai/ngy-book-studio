use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Seek},
    path::Path,
};

use anyhow::{Context as _, Result, bail};
use zip::ZipArchive;

const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MIN_COMPRESSION_RATIO_CHECK_BYTES: u64 = 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;

/// Validates in-memory EPUB bytes (used when opening a book stored in the
/// database). The archive contents were already checked against the entry
/// limits during import, so here we only enforce the overall size bound.
pub(crate) fn validate_epub_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        bail!("EPUB 数据超过 1 GiB 的安全上限");
    }
    Ok(())
}

/// Re-validates a newly generated in-memory EPUB before it replaces the
/// already-imported database blob.
pub(crate) fn validate_epub_archive_bytes(bytes: &[u8]) -> Result<()> {
    validate_epub_bytes(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("EPUB 不是有效的 ZIP 容器")?;
    validate_archive_entries(&mut archive)
}

pub(crate) fn validate_epub_archive(path: &Path) -> Result<()> {
    let archive_bytes = path
        .metadata()
        .with_context(|| format!("无法读取 EPUB 文件信息：{}", path.display()))?
        .len();
    if archive_bytes > MAX_ARCHIVE_BYTES {
        bail!("EPUB 文件超过 1 GiB 的安全上限");
    }

    let file = File::open(path).with_context(|| format!("无法读取 EPUB：{}", path.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(file)).context("EPUB 不是有效的 ZIP 容器")?;
    validate_archive_entries(&mut archive)
}

fn validate_archive_entries<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<()> {
    if archive.len() > MAX_ENTRIES {
        bail!("EPUB 包含过多文件（上限 {MAX_ENTRIES} 个）");
    }

    let mut total = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("无法检查 EPUB 中的第 {} 个文件", index + 1))?;
        if entry.encrypted() {
            bail!("EPUB 包含不支持的加密资源：{}", entry.name());
        }
        if entry.enclosed_name().is_none() {
            bail!("EPUB 包含不安全的资源路径：{}", entry.name());
        }
        let size = entry.size();
        if size > MAX_ENTRY_BYTES {
            bail!("EPUB 资源过大：{}（单个资源上限 128 MiB）", entry.name());
        }
        total = total.checked_add(size).context("EPUB 解压后大小溢出")?;
        if total > MAX_TOTAL_UNCOMPRESSED_BYTES {
            bail!("EPUB 解压后总大小超过 2 GiB 的安全上限");
        }
        if size >= MIN_COMPRESSION_RATIO_CHECK_BYTES
            && (entry.compressed_size() == 0
                || entry
                    .compressed_size()
                    .saturating_mul(MAX_COMPRESSION_RATIO)
                    < size)
        {
            bail!("EPUB 资源压缩比异常：{}", entry.name());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write as _};

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    #[test]
    fn validates_generated_epub_archives_in_memory() {
        assert!(validate_epub_archive_bytes(b"not a zip archive").is_err());

        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("mimetype", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        validate_epub_archive_bytes(&bytes).unwrap();
    }

    #[test]
    fn rejects_high_compression_ratio_before_resource_extraction() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        writer
            .start_file(
                "EPUB/bomb.txt",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&vec![0; 2 * 1024 * 1024]).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let error = validate_epub_archive_bytes(&bytes).unwrap_err();
        assert!(error.to_string().contains("压缩比"));
    }
}
