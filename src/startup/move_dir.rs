//! Moving the whole data directory to a new home.

use super::*;

/// 把 `pending.from` 的内容整体搬到 `to`。
///
/// 语义是「新目录成为旧目录的完整副本，旧目录清空」：先在同一个卷内重命名（原子、瞬时），
/// 跨卷时才退回复制 + 删除。目标目录必须是空的 —— 合并两个书库没有明确定义，与其猜，
/// 不如报错让用户另选一个空目录。
pub(crate) fn move_data_dir(pending: &PendingMove, to: &Path) -> Result<()> {
    let from = pending.from.as_path();
    if !from.exists() {
        // 旧目录已经被删掉或移走，没有可搬的东西，当作搬完。
        return Ok(());
    }
    fs::create_dir_all(to).with_context(|| format!("无法创建数据目录：{}", to.display()))?;
    if same_path(from, to) {
        return Ok(());
    }
    if is_inside(to, from) {
        bail!(
            "新数据目录 {} 在旧数据目录 {} 里面",
            to.display(),
            from.display()
        );
    }

    if holds_a_library(to) {
        if !pending.overwrite {
            bail!("{} 已经是一个图书库", to.display());
        }
        remove_library_data(to)?;
    }
    if let Some(entry) = first_entry(to)? {
        bail!(
            "{} 不是空目录（里面有 {}），请另选一个空目录",
            to.display(),
            entry.display()
        );
    }

    // 空目录要先删掉，否则重命名只会把源目录塞进它里面。
    fs::remove_dir(to).with_context(|| format!("无法准备数据目录：{}", to.display()))?;
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }

    // 跨卷时重命名会失败：复制过去，成功后再删源目录；中途失败就清掉半个副本。
    copy_directory(from, to).inspect_err(|_| {
        let _ = fs::remove_dir_all(to);
    })?;
    if let Err(error) = fs::remove_dir_all(from) {
        // 数据已经完整地复制过去了，删不掉源目录不该让整个搬迁失败：留下两份比丢掉好。
        // 但要让用户知道旧目录还在，可以在资源管理器里自己删。
        tracing::warn!(
            from = %from.display(),
            to = %to.display(),
            %error,
            "数据目录已复制到新位置，但无法删除原目录"
        );
    }
    Ok(())
}

/// 删掉目录里已有的图书库数据，用于用户确认过的覆盖。
///
/// 只删墨页自己的东西：库文件（含 `-wal`/`-shm`/`-journal`）、对象存储、学习记录和日志。
/// 其它文件一律不动 —— 搬迁随后会检查目录是否为空，还有别的东西时报错让用户另选。
pub(crate) fn remove_library_data(data_dir: &Path) -> Result<()> {
    let database = crate::db::DATABASE_FILE;
    let mut targets = vec![PathBuf::from(database)];
    for suffix in ["-wal", "-shm", "-journal"] {
        targets.push(PathBuf::from(format!("{database}{suffix}")));
    }
    for directory in ["objects", "learning", "logs"] {
        targets.push(PathBuf::from(directory));
    }

    for name in targets {
        let path = data_dir.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("无法读取 {}", path.display()));
            }
        };
        let removed = if metadata.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.with_context(|| format!("无法删除 {}", path.display()))?;
    }
    Ok(())
}

/// 递归复制目录。只处理普通文件和目录：数据目录里出现别的类型说明有东西不该在这里，
/// 与其静默跳过丢掉它，不如报错让人来看。
pub(crate) fn copy_directory(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("无法创建目录：{}", to.display()))?;
    let entries = fs::read_dir(from).with_context(|| format!("无法读取 {}", from.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("无法读取 {}", from.display()))?;
        let target = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .with_context(|| format!("无法读取 {}", entry.path().display()))?;
        if kind.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)
                .with_context(|| format!("无法复制到 {}", target.display()))?;
        } else {
            bail!("{} 不是普通文件或目录", entry.path().display());
        }
    }
    Ok(())
}

pub(crate) fn first_entry(directory: &Path) -> Result<Option<PathBuf>> {
    let mut entries =
        fs::read_dir(directory).with_context(|| format!("无法读取 {}", directory.display()))?;
    match entries.next() {
        Some(entry) => {
            let entry = entry.with_context(|| format!("无法读取 {}", directory.display()))?;
            Ok(Some(entry.path()))
        }
        None => Ok(None),
    }
}

/// 两个路径是否指向同一个位置。用户输入的形式（尾部分隔符、`\\?\` 前缀、相对与绝对）
/// 可能不同，所以先直接比，再在都能规范化时比规范形式。
pub(crate) fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// `path` 是否在 `directory` 里面（或就是它）。
pub(crate) fn is_inside(path: &Path, directory: &Path) -> bool {
    match (fs::canonicalize(path), fs::canonicalize(directory)) {
        (Ok(path), Ok(directory)) => path.starts_with(directory),
        _ => false,
    }
}
