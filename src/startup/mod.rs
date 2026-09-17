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

mod config;

mod dialogs;

mod launch;

mod move_dir;

use config::*;
use move_dir::*;

// Re-exported so `crate::startup::*` keeps working exactly as before the
// split; the submodules reach each other through the private globs above.
pub use config::{
    BOOTSTRAP_FILE, DATA_DIR_ENV, bootstrap_path, ensure_data_dir_usable, env_data_dir,
    forget_remembered_data_dir, holds_a_library, load_pending_move, load_remembered_data_dir,
    normalize_input_path, save_remembered_data_dir,
};
pub use dialogs::{dialog_start_directory, shell_dialog_directory};
pub use launch::{
    apply_data_dir, apply_data_dir_change, complete_pending_move, describe, plan_launch,
};

#[cfg(test)]
mod tests;
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
    pub(crate) fn description(self) -> &'static str {
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

/// 用户更改数据目录后留下的待办：下次打开图书库之前，把 `from` 整体搬到当前数据目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMove {
    pub from: PathBuf,
    /// 用户已经确认过目标目录里原有的图书库可以被覆盖。
    pub overwrite: bool,
}
