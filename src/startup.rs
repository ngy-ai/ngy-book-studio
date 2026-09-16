//! 启动期的存储位置解析。
//!
//! 数据目录不再固定使用 `ProjectDirs` 的默认位置：首次运行（或上次选择已失效）时
//! 由用户在设置窗口里确认，选择结果记在**图书库之外**的引导配置 `bootstrap.json` 里。
//! 日志目录不是独立选项，它固定是数据目录下的 `logs/`（见 [`crate::logging`]）。
//!
//! 引导配置必须放在数据目录之外：`settings` 表本身就在图书库里，用它记录图书库的
//! 位置没有意义。配置存在但读不出来时按“没有记录”处理并重新询问，不静默吞掉。
//!
//! 本模块只负责「决定」与「落盘」，不碰界面：[`plan_launch`] 判断能不能直接用记住的
//! 目录，需要用户确认时返回 [`LaunchPlan::NeedsSetup`]，由启动流程开设置窗口，用户
//! 点确认后再调用 [`apply_data_dir`] 校验并记录。这样界面不必和文件系统校验交错在
//! 一个循环里，也便于单测。
//!
//! 开发构建里 `NGY_DATA_DIR` 优先于配置与设置窗口，用于测试与隔离环境；它不写配置。

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use directories::ProjectDirs;

/// 仅开发构建生效的数据目录覆盖，优先级最高。
pub const DATA_DIR_ENV: &str = "NGY_DATA_DIR";
/// 记录用户选择的引导配置文件名。
pub const BOOTSTRAP_FILE: &str = "bootstrap.json";
/// 校验目录可写时使用的探针文件名，写完立即删除。
const WRITE_PROBE_FILE: &str = ".ngy-book-studio-write-probe";

/// 数据目录的来源，用于日志与提示信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataDirSource {
    /// `NGY_DATA_DIR` 环境变量（仅开发构建）。
    Environment,
    /// 引导配置里记住的上一次选择。
    Remembered,
    /// 用户在设置窗口里确认，并已写入引导配置。
    Chosen,
}

impl DataDirSource {
    /// 写进启动日志的一句话，便于从日志确认这次用的是哪个目录。
    fn description(self) -> &'static str {
        match self {
            Self::Environment => "来自 NGY_DATA_DIR 环境变量",
            Self::Remembered => "来自上次记录的选择",
            Self::Chosen => "由用户在本次启动中选择",
        }
    }
}

/// 解析结果：数据目录及其来源。
#[derive(Debug, Clone)]
pub struct DataDirSelection {
    pub data_dir: PathBuf,
    pub source: DataDirSource,
}

/// 需要用户确认目录的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReselectReason {
    /// 首次运行，还没有任何记录。
    FirstRun,
    /// 记住了路径，但目录现在不可用。
    Unusable(String),
    /// 配置文件存在但读不出来。
    Unreadable(String),
    /// 上次更改数据目录留下的搬迁没能完成，旧目录的内容还在原处。
    MoveFailed { from: PathBuf, error: String },
}

/// 启动计划：要么已经确定数据目录，要么得先让用户确认。
#[derive(Debug, Clone)]
pub enum LaunchPlan {
    /// 目录已确定（环境变量或上次的选择），可以直接启动。
    Ready(DataDirSelection),
    /// 需要设置窗口；`suggestion` 是输入框的初始内容。
    NeedsSetup {
        reason: ReselectReason,
        suggestion: PathBuf,
    },
}

/// 应用启动路径：能直接用就用，否则要求用户设置。
pub fn plan_launch() -> Result<LaunchPlan> {
    let config_path = bootstrap_path()?;
    let suggestion = crate::library::LibraryStore::default_data_dir()?;
    plan_launch_with(&config_path, env_data_dir(), suggestion)
}

/// [`plan_launch`] 的可注入版本：配置文件、环境变量覆盖与推荐路径都由调用方给出。
fn plan_launch_with(
    config_path: &Path,
    env_override: Option<PathBuf>,
    suggested_data_dir: PathBuf,
) -> Result<LaunchPlan> {
    if let Some(data_dir) = env_override {
        ensure_data_dir_usable(&data_dir)
            .with_context(|| format!("{DATA_DIR_ENV} 指定的数据目录不可用"))?;
        return Ok(LaunchPlan::Ready(DataDirSelection {
            data_dir,
            source: DataDirSource::Environment,
        }));
    }

    match load_remembered_data_dir(config_path) {
        Ok(Some(data_dir)) => match ensure_data_dir_usable(&data_dir) {
            Ok(()) => Ok(LaunchPlan::Ready(DataDirSelection {
                data_dir,
                source: DataDirSource::Remembered,
            })),
            // 记住的目录不可用时，把上次的选择放进输入框，方便用户直接改。
            Err(error) => Ok(LaunchPlan::NeedsSetup {
                reason: ReselectReason::Unusable(format!("{error:#}")),
                suggestion: data_dir,
            }),
        },
        Ok(None) => Ok(LaunchPlan::NeedsSetup {
            reason: ReselectReason::FirstRun,
            suggestion: suggested_data_dir,
        }),
        // 配置读不出来不等于用户没选过：说清原因后重新询问，而不是继续用默认目录。
        Err(error) => Ok(LaunchPlan::NeedsSetup {
            reason: ReselectReason::Unreadable(format!("{error:#}")),
            suggestion: suggested_data_dir,
        }),
    }
}

/// 校验用户确认的数据目录并记住它。
///
/// 校验在写配置之前完成：选到只读目录或已拔出的移动盘时立刻失败，而不是把坏路径
/// 记下来、等打开图书库才发现。
pub fn apply_data_dir(config_path: &Path, data_dir: &Path) -> Result<DataDirSelection> {
    ensure_data_dir_usable(data_dir)?;
    save_remembered_data_dir(config_path, data_dir)?;
    Ok(DataDirSelection {
        data_dir: data_dir.to_path_buf(),
        source: DataDirSource::Chosen,
    })
}

/// 用户在系统配置里更改数据目录：校验新目录后记录，并留下待搬迁的旧目录。
///
/// 搬迁不在这里做：运行中的图书库、对象存储和日志都绑在旧目录上，就地搬走会让当前进程
/// 立刻失效。记录留在引导配置里，由下次启动在打开图书库**之前**完成
/// （[`complete_pending_move`]）。
pub fn apply_data_dir_change(
    config_path: &Path,
    data_dir: &Path,
    from: &Path,
    overwrite: bool,
) -> Result<DataDirSelection> {
    if same_path(from, data_dir) {
        bail!("旧数据目录与新数据目录是同一个位置");
    }
    ensure_data_dir_usable(data_dir)?;
    let pending = PendingMove {
        from: from.to_path_buf(),
        overwrite,
    };
    write_config(
        config_path,
        &BootstrapConfig::new(data_dir.to_path_buf(), Some(&pending)),
    )?;
    Ok(DataDirSelection {
        data_dir: data_dir.to_path_buf(),
        source: DataDirSource::Chosen,
    })
}

/// 完成待办的搬迁：把旧数据目录整体搬到 `data_dir`，成功后清除记录。
///
/// 必须在打开图书库之前调用 —— 当前进程占着旧目录里的库和日志时搬不动它。
/// `data_dir` 是本次启动真正要用的目录：它与配置里记的不一致时（开发构建的
/// `NGY_DATA_DIR` 会覆盖）什么都不做，否则隔离环境会把真实书库搬走。
///
/// 失败时记录保持不变，下次启动继续重试：清掉记录等于把旧数据丢在原地，而且用户看不到
/// 它还在那。
pub fn complete_pending_move(config_path: &Path, data_dir: &Path) -> Result<Option<PathBuf>> {
    let Some(config) = read_config(config_path)? else {
        return Ok(None);
    };
    if !same_path(&config.data_dir, data_dir) {
        return Ok(None);
    }
    let Some(pending) = config
        .pending_move()
        .filter(|pending| !same_path(&pending.from, &config.data_dir))
    else {
        return Ok(None);
    };

    move_data_dir(&pending, &config.data_dir)?;
    write_config(
        config_path,
        &BootstrapConfig::new(config.data_dir.clone(), None),
    )?;
    Ok(Some(pending.from))
}

/// 把 `pending.from` 的内容整体搬到 `to`。
///
/// 语义是「新目录成为旧目录的完整副本，旧目录清空」：先在同一个卷内重命名（原子、瞬时），
/// 跨卷时才退回复制 + 删除。目标目录必须是空的 —— 合并两个书库没有明确定义，与其猜，
/// 不如报错让用户另选一个空目录。
fn move_data_dir(pending: &PendingMove, to: &Path) -> Result<()> {
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
fn remove_library_data(data_dir: &Path) -> Result<()> {
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
fn copy_directory(from: &Path, to: &Path) -> Result<()> {
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

fn first_entry(directory: &Path) -> Result<Option<PathBuf>> {
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
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// `path` 是否在 `directory` 里面（或就是它）。
fn is_inside(path: &Path, directory: &Path) -> bool {
    match (fs::canonicalize(path), fs::canonicalize(directory)) {
        (Ok(path), Ok(directory)) => path.starts_with(directory),
        _ => false,
    }
}

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

/// 用户更改数据目录后留下的待办：下次打开图书库之前，把 `from` 整体搬到当前数据目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMove {
    pub from: PathBuf,
    /// 用户已经确认过目标目录里原有的图书库可以被覆盖。
    pub overwrite: bool,
}

/// 目录里是否已经有一个图书库。
pub fn holds_a_library(data_dir: &Path) -> bool {
    data_dir.join(crate::db::DATABASE_FILE).is_file()
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
fn corrupt_backup_path(config_path: &Path) -> PathBuf {
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

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BootstrapConfig {
    data_dir: PathBuf,
    /// 待搬迁的旧数据目录。搬迁成功后清除，因此它同时是「搬迁还没做完」的标记。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    move_from: Option<PathBuf>,
    /// 覆盖目标目录里原有图书库的授权。只在 `move_from` 存在时有意义。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    move_overwrite: bool,
}

impl BootstrapConfig {
    fn new(data_dir: PathBuf, pending: Option<&PendingMove>) -> Self {
        Self {
            data_dir,
            move_from: pending.map(|pending| pending.from.clone()),
            move_overwrite: pending.is_some_and(|pending| pending.overwrite),
        }
    }

    fn pending_move(&self) -> Option<PendingMove> {
        self.move_from.clone().map(|from| PendingMove {
            from,
            overwrite: self.move_overwrite,
        })
    }
}

/// 读引导配置。文件不存在返回 `Ok(None)`，内容读不出来返回 `Err`。
fn read_config(config_path: &Path) -> Result<Option<BootstrapConfig>> {
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
fn write_config(config_path: &Path, config: &BootstrapConfig) -> Result<()> {
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

/// 文件 API 接受标准化的 `\\?\` 路径，而 native-dialog 使用的 Shell 解析会拒绝它。
/// 只转换对话框里显示的位置，存储路径保持原样。
pub fn shell_dialog_directory(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            let mut directory = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
                Prefix::VerbatimUNC(server, share) => PathBuf::from(r"\\").join(server).join(share),
                _ => return path.to_path_buf(),
            };
            directory.extend(components);
            return directory;
        }
    }
    path.to_path_buf()
}

/// 系统目录选择器的初始位置。
///
/// native-dialog 把这里的路径交给 Windows Shell 解析（`SHCreateItemFromParsingName`），
/// 路径不存在时会直接失败并让对话框打不开 —— 而首次启动时推荐的数据目录恰恰还没建出来。
/// 所以只交出确实存在的目录：输入路径存在就用它，否则沿父目录上溯到最近的存在目录；
/// 都找不到（相对路径、含非法字符）时返回 `None`，让对话框用系统默认位置。
pub fn dialog_start_directory(path: Option<&Path>) -> Option<PathBuf> {
    let mut candidate = path?;
    loop {
        if candidate.is_absolute() && candidate.is_dir() {
            return Some(shell_dialog_directory(candidate));
        }
        candidate = candidate.parent()?;
    }
}

/// 启动日志里记录这次用了哪个数据目录及其来源。
pub fn describe(selection: &DataDirSelection) -> String {
    format!(
        "数据目录：{}（{}）",
        selection.data_dir.display(),
        selection.source.description()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(path: &Path, data_dir: &Path) {
        save_remembered_data_dir(path, data_dir).expect("写入引导配置");
    }

    fn ready(plan: LaunchPlan) -> DataDirSelection {
        match plan {
            LaunchPlan::Ready(selection) => selection,
            LaunchPlan::NeedsSetup { reason, .. } => panic!("预期无需设置，实际：{reason:?}"),
        }
    }

    fn needs_setup(plan: LaunchPlan) -> (ReselectReason, PathBuf) {
        match plan {
            LaunchPlan::NeedsSetup { reason, suggestion } => (reason, suggestion),
            LaunchPlan::Ready(selection) => {
                panic!("预期需要设置，实际用了 {}", selection.data_dir.display())
            }
        }
    }

    #[test]
    fn ensure_rejects_a_path_owned_by_a_file() {
        let temp = tempfile::tempdir().expect("临时目录");
        let file = temp.path().join("不是目录");
        fs::write(&file, b"x").expect("写文件");

        let error = ensure_data_dir_usable(&file).expect_err("文件不能当数据目录");
        assert!(
            format!("{error:#}").contains("无法创建数据目录"),
            "{error:#}"
        );
    }

    #[test]
    fn ensure_leaves_no_probe_behind() {
        let temp = tempfile::tempdir().expect("临时目录");
        let data_dir = temp.path().join("书库");

        ensure_data_dir_usable(&data_dir).expect("目录可用");

        assert!(data_dir.is_dir());
        let entries: Vec<_> = fs::read_dir(&data_dir)
            .expect("读取目录")
            .map(|entry| entry.expect("目录项").file_name())
            .collect();
        assert!(entries.is_empty(), "{entries:?}");
    }

    #[test]
    fn environment_override_wins_and_does_not_touch_the_config() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let remembered = temp.path().join("记住的");
        write_config(&config, &remembered);
        let overridden = temp.path().join("环境变量");

        let selection = ready(
            plan_launch_with(&config, Some(overridden.clone()), temp.path().join("建议"))
                .expect("环境变量优先"),
        );

        assert_eq!(selection.data_dir, overridden);
        assert_eq!(selection.source, DataDirSource::Environment);
        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读配置")
                .as_deref(),
            Some(remembered.as_path()),
            "环境变量不改写配置"
        );
    }

    #[test]
    fn environment_override_must_be_usable() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let file = temp.path().join("文件");
        fs::write(&file, b"x").expect("写文件");

        let error = plan_launch_with(&config, Some(file), temp.path().join("建议"))
            .expect_err("环境变量指向文件时失败");
        assert!(format!("{error:#}").contains(DATA_DIR_ENV), "{error:#}");
    }

    #[test]
    fn a_remembered_directory_needs_no_setup() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let remembered = temp.path().join("书库");
        fs::create_dir_all(&remembered).expect("创建书库");
        write_config(&config, &remembered);

        let selection = ready(
            plan_launch_with(&config, None, temp.path().join("建议")).expect("使用记住的目录"),
        );

        assert_eq!(selection.data_dir, remembered);
        assert_eq!(selection.source, DataDirSource::Remembered);
    }

    #[test]
    fn the_first_run_asks_for_setup_with_the_suggested_directory() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let suggested = temp.path().join("建议");

        let (reason, suggestion) =
            needs_setup(plan_launch_with(&config, None, suggested.clone()).expect("首次启动"));

        assert_eq!(reason, ReselectReason::FirstRun);
        assert_eq!(suggestion, suggested);
    }

    #[test]
    fn an_unusable_remembered_directory_is_offered_back_for_editing() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let gone = temp.path().join("已拔出的移动盘");
        // 记住的是文件而不是目录：与“移动盘拔掉”在可用性上等价。
        fs::write(&gone, b"x").expect("写文件");
        write_config(&config, &gone);

        let (reason, suggestion) = needs_setup(
            plan_launch_with(&config, None, temp.path().join("建议")).expect("重新选择"),
        );

        assert!(
            matches!(&reason, ReselectReason::Unusable(message) if message.contains("无法创建数据目录")),
            "{reason:?}"
        );
        assert_eq!(suggestion, gone, "上次的选择要作为输入框初值");
    }

    #[test]
    fn an_unreadable_config_asks_again_with_the_reason() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        fs::write(&config, b"{ not json").expect("写坏配置");
        let suggested = temp.path().join("建议");

        let (reason, suggestion) =
            needs_setup(plan_launch_with(&config, None, suggested.clone()).expect("坏配置后重选"));

        assert!(
            matches!(&reason, ReselectReason::Unreadable(message) if message.contains("无法解析")),
            "{reason:?}"
        );
        assert_eq!(suggestion, suggested);
    }

    #[test]
    fn confirming_a_directory_saves_it() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("nested").join("bootstrap.json");
        let chosen = temp.path().join("新书库");

        let selection = apply_data_dir(&config, &chosen).expect("确认新目录");

        assert_eq!(selection.data_dir, chosen);
        assert_eq!(selection.source, DataDirSource::Chosen);
        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读配置")
                .as_deref(),
            Some(chosen.as_path())
        );
    }

    #[test]
    fn confirming_an_unusable_directory_reports_and_saves_nothing() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let blocked = temp.path().join("文件");
        fs::write(&blocked, b"x").expect("写文件");

        let error = apply_data_dir(&config, &blocked).expect_err("文件不能当数据目录");

        assert!(
            format!("{error:#}").contains("无法创建数据目录"),
            "{error:#}"
        );
        assert_eq!(load_remembered_data_dir(&config).expect("读配置"), None);
    }

    #[test]
    fn forgetting_the_selection_removes_the_config() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        write_config(&config, &temp.path().join("书库"));

        forget_remembered_data_dir(&config).expect("清除记录");
        assert!(!config.exists());
        assert_eq!(load_remembered_data_dir(&config).expect("读配置"), None);
        forget_remembered_data_dir(&config).expect("重复清除不报错");
    }

    #[test]
    fn saving_replaces_the_previous_selection_atomically() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        write_config(&config, &temp.path().join("旧书库"));
        let new_dir = temp.path().join("新书库");
        write_config(&config, &new_dir);

        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读配置")
                .as_deref(),
            Some(new_dir.as_path())
        );
        assert!(!config.with_extension("json.tmp").exists());
    }

    #[test]
    fn input_text_is_trimmed_and_unquoted() {
        for (input, expected) in [
            (
                r#"  "C:\Users\学习资料\archives"  "#,
                r"C:\Users\学习资料\archives",
            ),
            (r#"“D:\墨页书库”"#, r"D:\墨页书库"),
            (r#"C:\墨页书库"#, r"C:\墨页书库"),
            ("  C:\\带 空格 的目录  ", "C:\\带 空格 的目录"),
        ] {
            assert_eq!(
                normalize_input_path(input),
                Some(PathBuf::from(expected)),
                "{input}"
            );
        }
        for blank in ["", "   ", "\"\"", "“”"] {
            assert_eq!(normalize_input_path(blank), None, "{blank}");
        }
    }

    // 文件 API 接受标准化的 `\\?\` 路径，native-dialog 的 Shell 解析会拒绝它，
    // 所以只在对话框位置里转换前缀。
    #[cfg(target_os = "windows")]
    #[test]
    fn shell_dialog_directory_converts_drive_and_unc_prefixes_only() {
        for (input, expected) in [
            (
                r"\\?\C:\Users\学习资料\archives",
                r"C:\Users\学习资料\archives",
            ),
            (
                r"\\?\UNC\server\share\learning\archives",
                r"\\server\share\learning\archives",
            ),
            (r"C:\Users\学习资料\archives", r"C:\Users\学习资料\archives"),
        ] {
            assert_eq!(
                shell_dialog_directory(Path::new(input)),
                PathBuf::from(expected)
            );
        }
    }

    // 首次启动时推荐的数据目录还没建出来，Shell 解析不存在的路径会让对话框直接打不开，
    // 所以初始位置只取存在的目录，取不到就不设置。
    #[test]
    fn dialog_start_directory_uses_the_closest_existing_directory() {
        let temp = tempfile::tempdir().expect("临时目录");
        let missing = temp.path().join("还没建的推荐位置").join("data");

        assert_eq!(
            dialog_start_directory(Some(missing.as_path())),
            Some(temp.path().to_path_buf())
        );
        assert_eq!(
            dialog_start_directory(Some(temp.path())),
            Some(temp.path().to_path_buf())
        );
        assert_eq!(dialog_start_directory(Some(Path::new("相对/路径"))), None);
        assert_eq!(dialog_start_directory(None), None);
    }

    /// 旧书库的样子：库文件加对象存储。
    fn seed_library(data_dir: &Path, marker: &[u8]) {
        fs::create_dir_all(data_dir.join("objects")).expect("建对象目录");
        fs::write(data_dir.join(crate::db::DATABASE_FILE), marker).expect("写库文件");
        fs::write(data_dir.join("objects").join("blob"), marker).expect("写对象");
    }

    #[test]
    fn changing_the_data_directory_moves_the_old_contents_on_the_next_launch() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let new = temp.path().join("新书库");
        seed_library(&old, "旧库".as_bytes());
        write_config(&config, &old);

        apply_data_dir_change(&config, &new, &old, false).expect("记录更改");
        // 记录更改本身不搬数据：当前进程还占着旧库。
        assert!(old.join(crate::db::DATABASE_FILE).is_file());
        assert_eq!(
            load_pending_move(&config).expect("读待办"),
            Some(PendingMove {
                from: old.clone(),
                overwrite: false,
            })
        );

        assert_eq!(
            complete_pending_move(&config, &new).expect("搬迁"),
            Some(old.clone())
        );
        assert_eq!(
            fs::read(new.join(crate::db::DATABASE_FILE)).expect("读新库"),
            "旧库".as_bytes()
        );
        assert!(new.join("objects").join("blob").is_file());
        assert!(!old.exists(), "旧目录应被清空");
        assert_eq!(load_pending_move(&config).expect("读待办"), None);
        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读配置")
                .as_deref(),
            Some(new.as_path())
        );

        // 搬完再启动不该重复搬迁。
        assert_eq!(
            complete_pending_move(&config, &new).expect("再搬一次"),
            None
        );
    }

    #[test]
    fn a_target_holding_a_library_needs_the_overwrite_permission() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let new = temp.path().join("新书库");
        seed_library(&old, "旧库".as_bytes());
        seed_library(&new, "别人的库".as_bytes());

        apply_data_dir_change(&config, &new, &old, false).expect("记录更改");
        let error = complete_pending_move(&config, &new).expect_err("没有覆盖授权时必须失败");
        assert!(
            format!("{error:#}").contains("已经是一个图书库"),
            "{error:#}"
        );
        // 两边都原样保留，记录也还在，下次启动可以重试。
        assert_eq!(
            fs::read(new.join(crate::db::DATABASE_FILE)).expect("读目标库"),
            "别人的库".as_bytes()
        );
        assert_eq!(
            fs::read(old.join(crate::db::DATABASE_FILE)).expect("读旧库"),
            "旧库".as_bytes()
        );
        assert!(load_pending_move(&config).expect("读待办").is_some());

        // 用户确认覆盖后重试。
        apply_data_dir_change(&config, &new, &old, true).expect("重新记录");
        complete_pending_move(&config, &new).expect("搬迁");
        assert_eq!(
            fs::read(new.join(crate::db::DATABASE_FILE)).expect("读新库"),
            "旧库".as_bytes()
        );
        assert!(!old.exists());
    }

    #[test]
    fn a_target_holding_other_files_is_refused_and_the_source_stays() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let new = temp.path().join("新书库");
        seed_library(&old, "旧库".as_bytes());
        fs::create_dir_all(&new).expect("建新目录");
        fs::write(new.join("别人的文件.txt"), b"x").expect("写别的文件");

        apply_data_dir_change(&config, &new, &old, false).expect("记录更改");
        let error = complete_pending_move(&config, &new).expect_err("非空目录必须拒绝");
        assert!(format!("{error:#}").contains("不是空目录"), "{error:#}");
        assert!(
            old.join(crate::db::DATABASE_FILE).is_file(),
            "源目录不能被搬走"
        );
        assert!(new.join("别人的文件.txt").is_file());
        assert!(load_pending_move(&config).expect("读待办").is_some());
    }

    #[test]
    fn a_vanished_source_directory_finishes_the_move() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let new = temp.path().join("新书库");
        seed_library(&old, "旧库".as_bytes());
        write_config(&config, &old);
        apply_data_dir_change(&config, &new, &old, false).expect("记录更改");

        fs::remove_dir_all(&old).expect("用户自己删掉了旧目录");

        assert_eq!(
            complete_pending_move(&config, &new).expect("搬迁"),
            Some(old.clone())
        );
        assert_eq!(load_pending_move(&config).expect("读待办"), None);
    }

    #[test]
    fn changing_the_directory_back_to_the_source_cancels_the_move() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let new = temp.path().join("新书库");
        seed_library(&old, "旧库".as_bytes());
        write_config(&config, &old);
        apply_data_dir_change(&config, &new, &old, false).expect("记录更改");

        // 改回原目录：没有东西要搬，待办要清掉，否则会要求目标目录为空。
        apply_data_dir(&config, &old).expect("改回原目录");
        assert_eq!(load_pending_move(&config).expect("读待办"), None);
        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读配置")
                .as_deref(),
            Some(old.as_path())
        );
    }

    #[test]
    fn a_pending_move_follows_another_directory_change() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let blocked = temp.path().join("放不下");
        let another = temp.path().join("换个位置");
        seed_library(&old, "旧库".as_bytes());
        write_config(&config, &old);
        apply_data_dir_change(&config, &blocked, &old, false).expect("记录更改");

        // 搬迁还没做完用户又选了别处：源目录要跟着走，否则旧数据会失去线索。
        apply_data_dir(&config, &another).expect("换个目录");
        assert_eq!(
            load_pending_move(&config).expect("读待办"),
            Some(PendingMove {
                from: old,
                overwrite: false,
            })
        );
        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读配置")
                .as_deref(),
            Some(another.as_path())
        );
    }

    // 坏掉的配置里可能还留着未完成搬迁的线索，只是解析不出来。静默覆盖会把线索永久抹掉；
    // 但为此让写配置失败会卡死启动（错误来自配置文件，用户改路径也修不好）。所以把原始
    // 字节备份到旁边，再照常写新配置：证据留在磁盘上，用户能继续用。每次损坏各留一份。
    #[test]
    fn a_corrupt_config_is_archived_beside_the_new_one_instead_of_blocking_startup() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let blocked = temp.path().join("放不下");
        let another = temp.path().join("另一个目录");
        seed_library(&old, "旧库".as_bytes());
        apply_data_dir_change(&config, &blocked, &old, false).expect("记录更改");
        // 记录本来在配置里，随后文件坏掉（写一半被中断，或被手工改坏）。
        let mut first_broken = fs::read(&config).expect("读配置");
        first_broken.extend_from_slice("\n{ 被改坏了".as_bytes());
        fs::write(&config, &first_broken).expect("写坏配置");
        assert!(load_pending_move(&config).is_err(), "坏配置读不出待办");

        // 坏配置不再让保存失败：启动能继续，用户换个目录重试就能通过。
        save_remembered_data_dir(&config, &another).expect("坏配置要备份而不是阻塞启动");

        assert_eq!(
            load_remembered_data_dir(&config)
                .expect("读新配置")
                .as_deref(),
            Some(another.as_path())
        );
        assert_eq!(
            load_pending_move(&config).expect("读新配置里的待办"),
            None,
            "坏文件里的待办恢复不了，新配置没有待办"
        );
        let first_backup = config.with_file_name("bootstrap.json.corrupt");
        assert_eq!(
            fs::read(&first_backup).expect("读备份"),
            first_broken,
            "原始字节要逐字节留在备份里"
        );

        // 再坏一次：新配置被改坏后重存，前一份备份不能被盖掉。
        let mut second_broken = fs::read(&config).expect("读配置");
        second_broken.extend_from_slice("\n{ 又坏了".as_bytes());
        fs::write(&config, &second_broken).expect("再次写坏配置");
        save_remembered_data_dir(&config, &blocked).expect("第二次坏配置也要能继续");

        assert_eq!(
            fs::read(&first_backup).expect("读第一份备份"),
            first_broken,
            "第一次损坏的备份不能被第二次覆盖"
        );
        assert_eq!(
            fs::read(config.with_file_name("bootstrap.json.corrupt-2")).expect("读第二份备份"),
            second_broken,
            "第二次损坏的原始字节也要留一份"
        );
    }

    #[test]
    fn a_move_is_skipped_when_this_launch_uses_another_directory() {
        let temp = tempfile::tempdir().expect("临时目录");
        let config = temp.path().join("bootstrap.json");
        let old = temp.path().join("旧书库");
        let new = temp.path().join("新书库");
        let isolated = temp.path().join("隔离目录");
        seed_library(&old, "旧库".as_bytes());
        write_config(&config, &old);
        apply_data_dir_change(&config, &new, &old, false).expect("记录更改");

        // 开发构建的 NGY_DATA_DIR 会用别的目录启动：不能把配置里的真实书库搬到那去。
        assert_eq!(
            complete_pending_move(&config, &isolated).expect("搬迁"),
            None
        );
        assert!(
            old.join(crate::db::DATABASE_FILE).is_file(),
            "源目录不能被搬走"
        );
        assert!(load_pending_move(&config).expect("读待办").is_some());
    }
}
