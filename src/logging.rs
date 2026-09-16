//! 日志落盘配置：全局 subscriber 由 `ngy_utils_tracing` 安装（见 `src/main.rs` 的
//! `install_logging`）。本模块只负责把日志目录固定到 `<数据目录>/logs/` 并提供默认过滤串；
//! 目录创建、按日滚动、保留最近 [`LOG_MAX_FILES`] 个文件（含当天）与过期清理都由
//! `ngy_utils_tracing` 在 `init` 时完成（并在后台定期执行），本项目不再重复实现。
//!
//! 日志目录固定是 `<数据目录>/logs/`，不单独让用户选：数据目录已经由用户指定，
//! 再问一次只会增加启动成本。文件名以 [`LOG_PREFIX`] 为前缀、按 **UTC** 日期滚动
//! （与控制台默认时间戳、后台任务日志的 UTC 约定一致）。

use std::path::{Path, PathBuf};

/// 数据目录下的日志子目录名。
pub const LOG_DIRECTORY: &str = "logs";
/// 日志文件前缀；`ngy_utils_tracing` 在 Production/Test 模式下按日滚动为
/// `<前缀>.<YYYY-MM-DD>`。
pub const LOG_PREFIX: &str = "ngy-book-studio";
/// 保留的日志文件数上限（含当天），由 `ngy_utils_tracing` 的保留策略执行。
pub const LOG_MAX_FILES: usize = 7;
/// 未设置 `RUST_LOG` 时的默认过滤，控制台与文件一致。
pub const DEFAULT_LOG_FILTER: &str = "warn,ngy_ai=info,ngy_import=debug,ngy_reader=debug";

/// 数据目录对应的日志目录。
pub fn log_directory(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIRECTORY)
}
