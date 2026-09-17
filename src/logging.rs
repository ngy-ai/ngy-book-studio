//! 日志落盘配置：全局 subscriber 由 `ngy_utils_tracing` 安装（见 `src/main.rs` 的
//! `install_logging`）。本模块只负责把日志目录固定到 `<数据目录>/logs/` 并提供默认过滤串；
//! 目录创建、按日滚动、保留最近 [`LOG_MAX_FILES`] 个文件（含当天）与过期清理都由
//! `ngy_utils_tracing` 在 `init` 时完成（并在后台定期执行），本项目不再重复实现。
//!
//! 日志目录固定是 `<数据目录>/logs/`，不单独让用户选：数据目录已经由用户指定，
//! 再问一次只会增加启动成本。文件名以 [`LOG_PREFIX`] 为前缀、按 **UTC** 日期滚动
//! （与控制台默认时间戳、后台任务日志的 UTC 约定一致）。

use std::path::{Path, PathBuf};

/// `ngy_utils_tracing` 读取日志环境变量时使用的**必需**前缀（该库按字面拼接，故这里自带
/// 分隔符）：模式因此来自 `MOYE_APP_MODE`，不是无前缀的 `APP_MODE`，别的程序占用
/// `LOG_DIR`、`APP_MODE` 这类通用名也不会影响本项目的日志行为。`RUST_LOG` 是唯一例外，
/// 它保持无前缀。
pub const ENV_PREFIX: &str = "MOYE_";
/// 日志时间戳使用的 UTC 偏移，控制台层与文件层共用。显式写 `UTC` 而不依赖
/// `ngy_utils_tracing` 的默认值（0.6.0 起该默认值是 `+08:00`），因为本项目按 UTC 约定
/// 记录时间戳，与按 UTC 日期滚动的文件名一致。
pub const LOG_TIME_OFFSET: &str = "UTC";
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
