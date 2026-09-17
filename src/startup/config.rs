//! The bootstrap file outside the library: reading, writing and the atomic
//! replace that never leaves a half-written config behind.

use super::*;

/// 仅开发构建生效的数据目录覆盖，优先级最高。
pub const DATA_DIR_ENV: &str = "NGY_DATA_DIR";

/// 记录用户选择的引导配置文件名。
pub const BOOTSTRAP_FILE: &str = "bootstrap.json";

/// 校验目录可写时使用的探针文件名，写完立即删除。
pub(crate) const WRITE_PROBE_FILE: &str = ".ngy-book-studio-write-probe";

/// 把输入框里的文本整理成路径：去掉首尾空白，以及成对包住路径的引号。
///
/// 用户常把资源管理器或聊天里的路径连同引号一起粘贴进来，这里统一剥掉。
pub fn normalize_input_path(raw: &str) -> Option<PathBuf> {
    let unquoted = raw
        .trim()
        .trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”'))
        .trim();
    if unquoted.is_empty() {
        return None;
    }
    Some(PathBuf::from(unquoted))
}

/// 引导配置的路径（`%APPDATA%\<组织>\<应用>\config\bootstrap.json`）。
pub fn bootstrap_path() -> Result<PathBuf> {
    let project_dirs =
        ProjectDirs::from("dev", "ngy", "ngy_book_studio").context("无法确定应用配置目录")?;
    Ok(project_dirs.config_dir().join(BOOTSTRAP_FILE))
}

/// 读回记住的数据目录。文件不存在返回 `Ok(None)`，内容无法解析返回 `Err`。
pub fn load_remembered_data_dir(config_path: &Path) -> Result<Option<PathBuf>> {
    Ok(read_config(config_path)?.map(|config| config.data_dir))
}

/// 读出待办的搬迁。源目录已经是当前数据目录时不算待办。
pub fn load_pending_move(config_path: &Path) -> Result<Option<PendingMove>> {
    let Some(config) = read_config(config_path)? else {
        return Ok(None);
    };
    Ok(config
        .pending_move()
        .filter(|pending| !same_path(&pending.from, &config.data_dir)))
}

/// 记住数据目录，保留配置里尚未完成的搬迁。
///
/// 用户可能在搬迁失败后换个目录再试，待搬的源目录要跟着留下来；把目录改回源目录
/// 本身就是放弃搬迁。
///
/// 配置读不出来时有两种做法都不对。静默覆盖会把坏文件里的线索永久抹掉：那份文件可能
/// 正记着没搬完的旧目录，旧数据留在原处而用户再也看不到它。把错误抛给调用方同样不行
/// ——调用链（`apply_data_dir` ← 设置窗口）把它当成「这个目录不能用」，让用户改路径重试，
/// 可错误来自配置文件而不是用户输入，怎么改都会失败，启动被永久卡住，用户只能自己去
/// 配置目录删文件。
///
/// 这里取第三种做法：坏文件的原始字节按字节备份到旁边的 `bootstrap.json.corrupt`
/// （做法与 `src/learning_records.rs` 恢复备份前先归档当前文件同源），然后照常写新配置。
/// 需要注意，`move_from` 就存在这个文件里，解析不出来时 [`load_pending_move`] 根本读不到
/// 它，所以「保留 pending」在任何修法下都不可能 —— 备份保住的只是原始字节这份证据，不是
/// 待办本身。备份失败（连原文件都挪不动）才向上返回错误。
pub fn save_remembered_data_dir(config_path: &Path, data_dir: &Path) -> Result<()> {
    let pending = match load_pending_move(config_path) {
        Ok(pending) => pending.filter(|pending| !same_path(&pending.from, data_dir)),
        Err(error) => {
            let backup = corrupt_backup_path(config_path);
            tracing::warn!(
                config = %config_path.display(),
                backup = %backup.display(),
                error = %format!("{error:#}"),
                "引导配置无法解析，未完成的搬迁记录无法恢复；原件备份后写入新的数据目录"
            );
            fs::rename(config_path, &backup).with_context(|| {
                format!(
                    "无法把读不出来的配置 {} 备份到 {}",
                    config_path.display(),
                    backup.display()
                )
            })?;
            None
        }
    };
    write_config(
        config_path,
        &BootstrapConfig::new(data_dir.to_path_buf(), pending.as_ref()),
    )
}

/// 坏配置的备份路径：在原文件名后加 `.corrupt`。
///
/// 该名字已被占用时改用 `.corrupt-2`、`.corrupt-3`…，绝不覆盖已有备份 —— 每次损坏的原始
/// 字节都要留一份，否则第二次损坏正好盖掉第一份，证据又丢了。用序号而不是时间戳，便于
/// 测试与人工查找。
pub(crate) fn corrupt_backup_path(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new(""));
    let name = config_path.file_name().map_or_else(
        || BOOTSTRAP_FILE.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let first = parent.join(format!("{name}.corrupt"));
    if !first.exists() {
        return first;
    }
    let mut index = 2;
    loop {
        let candidate = parent.join(format!("{name}.corrupt-{index}"));
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}

/// 清除记住的数据目录，下次启动重新询问。文件不存在视为成功。
pub fn forget_remembered_data_dir(config_path: &Path) -> Result<()> {
    match fs::remove_file(config_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("无法删除 {}", config_path.display())),
    }
}

/// 目录里是否已经有一个图书库。
pub fn holds_a_library(data_dir: &Path) -> bool {
    data_dir.join(crate::db::DATABASE_FILE).is_file()
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BootstrapConfig {
    pub(crate) data_dir: PathBuf,
    /// 待搬迁的旧数据目录。搬迁成功后清除，因此它同时是「搬迁还没做完」的标记。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) move_from: Option<PathBuf>,
    /// 覆盖目标目录里原有图书库的授权。只在 `move_from` 存在时有意义。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) move_overwrite: bool,
}

impl BootstrapConfig {
    pub(crate) fn new(data_dir: PathBuf, pending: Option<&PendingMove>) -> Self {
        Self {
            data_dir,
            move_from: pending.map(|pending| pending.from.clone()),
            move_overwrite: pending.is_some_and(|pending| pending.overwrite),
        }
    }

    pub(crate) fn pending_move(&self) -> Option<PendingMove> {
        self.move_from.clone().map(|from| PendingMove {
            from,
            overwrite: self.move_overwrite,
        })
    }
}

/// 读引导配置。文件不存在返回 `Ok(None)`，内容读不出来返回 `Err`。
pub(crate) fn read_config(config_path: &Path) -> Result<Option<BootstrapConfig>> {
    let payload = match fs::read(config_path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取 {}", config_path.display()));
        }
    };
    let config: BootstrapConfig = serde_json::from_slice(&payload)
        .with_context(|| format!("无法解析 {}", config_path.display()))?;
    Ok(Some(config))
}

/// 写引导配置。先写同目录下的临时文件再改名，避免半个文件被读回。
pub(crate) fn write_config(config_path: &Path, config: &BootstrapConfig) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建配置目录：{}", parent.display()))?;
    }
    let payload = serde_json::to_vec_pretty(config).context("无法序列化数据目录设置")?;
    let temporary = config_path.with_extension("json.tmp");
    fs::write(&temporary, payload).with_context(|| format!("无法写入 {}", temporary.display()))?;
    fs::rename(&temporary, config_path)
        .with_context(|| format!("无法替换 {}", config_path.display()))?;
    Ok(())
}

/// 确认目录可用：建出来、能写、能删。
///
/// 探针文件写完立即删除，不留在数据目录里。
pub fn ensure_data_dir_usable(data_dir: &Path) -> Result<()> {
    if data_dir.as_os_str().is_empty() {
        bail!("数据目录路径为空");
    }
    fs::create_dir_all(data_dir)
        .with_context(|| format!("无法创建数据目录：{}", data_dir.display()))?;
    let probe = data_dir.join(WRITE_PROBE_FILE);
    fs::write(&probe, b"").with_context(|| format!("数据目录不可写：{}", data_dir.display()))?;
    fs::remove_file(&probe).with_context(|| format!("无法删除写入探针：{}", probe.display()))?;
    Ok(())
}

/// 开发构建里的 `NGY_DATA_DIR`；发布构建不认这个变量，避免普通用户被环境变量改走目录。
pub fn env_data_dir() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        std::env::var_os(DATA_DIR_ENV).map(PathBuf::from)
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}
