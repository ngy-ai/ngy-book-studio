//! Writing an export atomically, including the durability syncs.

use super::*;

/// Replaces `target` only after a complete same-directory temporary file has
/// been flushed and synchronized. `tempfile` uses `MoveFileExW` with replace
/// semantics on Windows and rename semantics on Unix.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with_syncs(
        target,
        bytes,
        std::fs::File::sync_all,
        sync_parent_directory,
    )
}

pub(crate) fn atomic_write_with_syncs<PreCommitSync, PostCommitSync>(
    target: &Path,
    bytes: &[u8],
    pre_commit_sync: PreCommitSync,
    post_commit_sync: PostCommitSync,
) -> Result<()>
where
    PreCommitSync: FnOnce(&std::fs::File) -> std::io::Result<()>,
    PostCommitSync: FnOnce(&Path) -> std::io::Result<()>,
{
    ensure!(!target.as_os_str().is_empty(), "导出目标路径不能为空");
    ensure!(target.file_name().is_some(), "导出目标必须是文件路径");
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure!(
        parent.is_dir(),
        "导出目标目录不存在或不是目录：{}",
        parent.display()
    );

    let mut staged = NamedTempFile::new_in(parent)
        .with_context(|| format!("无法在 {} 创建导出临时文件", parent.display()))?;
    staged.write_all(bytes).context("无法写入导出临时文件")?;
    staged.flush().context("无法刷新导出临时文件")?;
    pre_commit_sync(staged.as_file()).context("无法同步导出临时文件")?;
    let persisted = staged
        .persist(target)
        .map_err(|error| error.error)
        .with_context(|| format!("无法原子替换导出目标：{}", target.display()))?;

    // `persist` is the commit point. The complete data file was synchronized
    // above while it still had its temporary name, so no data-file sync that
    // can fail is allowed after this point: returning `Err` now would tell the
    // caller that the old target survived even though it has already been
    // replaced. A parent-directory sync only strengthens rename durability and
    // is therefore best-effort after commit.
    drop(persisted);
    if let Err(error) = post_commit_sync(parent) {
        tracing::warn!(
            target = %target.display(),
            directory = %parent.display(),
            %error,
            "导出文件已提交，但无法同步目标目录元数据"
        );
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

// Rust's standard library does not expose a portable directory fsync on
// Windows. The staged data itself is still fully synchronized before the
// atomic replacement; directory-entry durability remains an OS/filesystem
// property on this platform rather than something this helper can promise.
#[cfg(not(unix))]
pub(crate) fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}
