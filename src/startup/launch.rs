//! Deciding what a launch should do with the data directory, and applying
//! the user answer.

use super::*;

/// 应用启动路径：能直接用就用，否则要求用户设置。
pub fn plan_launch() -> Result<LaunchPlan> {
    let config_path = bootstrap_path()?;
    let suggestion = crate::library::LibraryStore::default_data_dir()?;
    plan_launch_with(&config_path, env_data_dir(), suggestion)
}

/// [`plan_launch`] 的可注入版本：配置文件、环境变量覆盖与推荐路径都由调用方给出。
pub(crate) fn plan_launch_with(
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

/// 启动日志里记录这次用了哪个数据目录及其来源。
pub fn describe(selection: &DataDirSelection) -> String {
    format!(
        "数据目录：{}（{}）",
        selection.data_dir.display(),
        selection.source.description()
    )
}
