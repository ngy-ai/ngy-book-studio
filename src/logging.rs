//! 日志落盘：`tracing` 输出同时写控制台和数据目录下的 `logs/`。
//!
//! 日志目录固定是 `<数据目录>/logs/`，不单独让用户选：数据目录已经由用户指定，
//! 再问一次只会增加启动成本。文件名是 `ngy-book-studio.<YYYY-MM-DD>.log`，按 **UTC**
//! 日期滚动（与控制台默认时间戳、后台任务日志的 UTC 约定一致），最多保留
//! [`LOG_RETENTION_DAYS`] 天，进程启动时清理更早的同类文件。
//!
//! 日志文件不可写不是启动失败的理由：降级为只写控制台，并由调用方给出可见提示。
//! 写文件失败不 panic —— GPUI 回调不可 unwind，日志路径上 panic 会直接终止进程。

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use tracing_subscriber::{
    EnvFilter, Registry, fmt, fmt::MakeWriter, layer::Layer as _, layer::SubscriberExt as _,
    util::SubscriberInitExt as _,
};

/// 数据目录下的日志子目录名。
pub const LOG_DIRECTORY: &str = "logs";
/// 日志文件主名；清理只认这个前缀加日期的文件。
const LOG_FILE_PREFIX: &str = "ngy-book-studio";
/// 保留天数，含当天。
const LOG_RETENTION_DAYS: i64 = 7;
/// 未设置 `RUST_LOG` 时的默认过滤，控制台与文件完全一致。
pub const DEFAULT_LOG_FILTER: &str = "warn,ngy_ai=info,ngy_import=debug,ngy_reader=debug";

/// 数据目录对应的日志目录。
pub fn log_directory(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIRECTORY)
}

/// 初始化日志：控制台 + `<data_dir>/logs/`。返回日志目录。
///
/// 只有创建日志目录、打开日志文件或安装 subscriber 失败时才返回错误，调用方应改用
/// [`init_console_only`] 继续启动（日志写不了不该拦住读书）。
pub fn init(data_dir: &Path) -> Result<PathBuf> {
    let (log_dir, writer) = prepare(data_dir)?;
    let file = fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_ansi(false)
        .with_writer(writer.sink())
        .with_filter(default_filter());
    Registry::default()
        .with(console_layer())
        .with(file)
        .try_init()
        .context("无法安装日志 subscriber")?;
    Ok(log_dir)
}

/// 降级路径：只写控制台。文件日志不可用时调用。
pub fn init_console_only() {
    if let Err(error) = Registry::default().with(console_layer()).try_init() {
        // 只有 subscriber 已经装好时才会走到这里，没有别的补救办法。
        tracing::debug!(%error, "console logging was already installed");
    }
}

fn console_layer() -> impl tracing_subscriber::layer::Layer<Registry> + Send + Sync + 'static {
    fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_filter(default_filter())
}

fn default_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER))
}

/// 建目录、清理过期日志、打开当天的文件。
fn prepare(data_dir: &Path) -> Result<(PathBuf, LogFileWriter)> {
    let log_dir = log_directory(data_dir);
    let writer = LogFileWriter::open(log_dir.clone())?;
    Ok((log_dir, writer))
}

/// 按 UTC 日期滚动的日志文件写入器。目录创建失败即整体失败，让调用方降级。
struct LogFileWriter {
    state: Arc<Mutex<LogState>>,
}

struct LogState {
    log_dir: PathBuf,
    /// 当前打开文件对应的 UTC 天数（自 Unix 纪元起）。
    open_day: Option<i64>,
    file: Option<File>,
    /// 写文件失败只在第一次直接告知 stderr，避免每一行都重复报错。
    reported_failure: bool,
    /// 当前时间的来源，测试用它控制跨天滚动。
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl LogFileWriter {
    fn open(log_dir: PathBuf) -> Result<Self> {
        Self::open_with_clock(log_dir, Box::new(current_day))
    }

    fn open_with_clock(log_dir: PathBuf, now: Box<dyn Fn() -> i64 + Send + Sync>) -> Result<Self> {
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("无法创建日志目录：{}", log_dir.display()))?;
        let today = now();
        prune_expired(&log_dir, today)
            .with_context(|| format!("无法清理过期日志：{}", log_dir.display()))?;
        let mut state = LogState {
            log_dir,
            open_day: None,
            file: None,
            reported_failure: false,
            now,
        };
        state.rotate(today);
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
        })
    }

    fn sink(&self) -> LogSink {
        LogSink {
            state: Arc::clone(&self.state),
        }
    }
}

impl LogState {
    /// 切到指定日期的文件。打不开时只保留控制台输出。
    fn rotate(&mut self, day: i64) {
        self.file = None;
        self.open_day = None;
        let path = self.log_dir.join(log_file_name(day));
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => {
                self.file = Some(file);
                self.open_day = Some(day);
                self.reported_failure = false;
            }
            Err(error) => self.report_failure(&path, &error),
        }
    }

    fn report_failure(&mut self, path: &Path, error: &io::Error) {
        if self.reported_failure {
            return;
        }
        self.reported_failure = true;
        eprintln!(
            "[ngy-book-studio] 无法写入日志文件 {}：{error}；本进程只输出到控制台",
            path.display()
        );
    }

    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let day = (self.now)();
        if self.open_day != Some(day) {
            self.rotate(day);
        }
        let Some(file) = self.file.as_mut() else {
            // 已经报过一次失败：丢掉这条文件副本，但让 tracing 认为写入成功。
            return Ok(buffer.len());
        };
        match file.write_all(buffer).and_then(|()| file.flush()) {
            Ok(()) => Ok(buffer.len()),
            Err(error) => {
                let path = self.open_day.map_or_else(
                    || self.log_dir.clone(),
                    |day| self.log_dir.join(log_file_name(day)),
                );
                self.report_failure(&path, &error);
                // 写坏的文件不再继续用，下一条写入会重新尝试打开。
                self.open_day = None;
                self.file = None;
                Ok(buffer.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// 传给 `tracing` 的写入端点；每次写入取一次锁，保证行不交错。
#[derive(Clone)]
struct LogSink {
    state: Arc<Mutex<LogState>>,
}

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = LogSinkGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogSinkGuard {
            // 日志路径绝不 panic：锁中毒时直接使用里面的状态。
            state: self.state.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }
}

struct LogSinkGuard<'a> {
    state: MutexGuard<'a, LogState>,
}

impl io::Write for LogSinkGuard<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.state.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state.flush()
    }
}

/// 清理超过保留期的日志文件。只删除本目录下名字符合
/// `ngy-book-studio.<YYYY-MM-DD>.log` 的普通文件，其它文件一律不动。
fn prune_expired(log_dir: &Path, today: i64) -> io::Result<()> {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(day) = parse_log_file_day(name) else {
            continue;
        };
        if today - day < LOG_RETENTION_DAYS || !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        match fs::remove_file(&path) {
            Ok(()) => tracing::debug!(path = %path.display(), "removed expired log file"),
            // 删不掉就留着：日志清理失败不该影响启动。
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "failed to remove expired log file");
            }
        }
    }
    Ok(())
}

fn log_file_name(day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    format!("{LOG_FILE_PREFIX}.{year:04}-{month:02}-{date:02}.log")
}

/// 解析本模块自己写出的文件名，只接受 `ngy-book-studio.<YYYY-MM-DD>.log`。
fn parse_log_file_day(name: &str) -> Option<i64> {
    let date = name
        .strip_prefix(LOG_FILE_PREFIX)?
        .strip_prefix('.')?
        .strip_suffix(".log")?;
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits = |slice: &[u8]| -> Option<i64> {
        let mut value = 0i64;
        for byte in slice {
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(byte - b'0');
        }
        Some(value)
    };
    let year = digits(&bytes[0..4])?;
    let month = digits(&bytes[5..7])?;
    let day = digits(&bytes[8..10])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month as u32, day as u32))
}

/// 当前 UTC 天数（自 Unix 纪元起）。
fn current_day() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default();
    seconds.div_euclid(86_400)
}

/// Howard Hinnant 的 `days_from_civil`：公历日期 → 自 1970-01-01 起的天数。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `days_from_civil` 的逆运算。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(day: i64) -> Box<dyn Fn() -> i64 + Send + Sync> {
        Box::new(move || day)
    }

    fn log_files(log_dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(log_dir)
            .expect("读取日志目录")
            .map(|entry| {
                entry
                    .expect("目录项")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn civil_dates_round_trip_through_day_numbers() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 1700000000 秒 = 2023-11-14T22:13:20Z
        assert_eq!(civil_from_days(1_700_000_000 / 86_400), (2023, 11, 14));
        for (year, month, day) in [
            (1969, 12, 31),
            (2000, 2, 29),
            (2024, 3, 1),
            (2026, 9, 14),
            (2100, 12, 31),
        ] {
            assert_eq!(
                civil_from_days(days_from_civil(year, month, day)),
                (year, month, day),
                "{year}-{month}-{day}"
            );
        }
    }

    #[test]
    fn log_file_names_parse_only_for_this_module() {
        let day = days_from_civil(2026, 9, 14);
        assert_eq!(log_file_name(day), "ngy-book-studio.2026-09-14.log");
        assert_eq!(
            parse_log_file_day("ngy-book-studio.2026-09-14.log"),
            Some(day)
        );
        for name in [
            "other.log",
            "ngy-book-studio.log",
            "ngy-book-studio.2026-09-14.txt",
            "ngy-book-studio.2026-13-01.log",
            "ngy-book-studio.2026-00-10.log",
            "ngy-book-studio.2026-09-00.log",
            "ngy-book-studio.26-09-14.log",
            "ngy-book-studio.2026-9-14.log",
            "ngy-book-studio.2026-09-14.log.bak",
        ] {
            assert_eq!(parse_log_file_day(name), None, "{name}");
        }
    }

    #[test]
    fn prepare_creates_the_log_directory_and_writes_there() {
        let temp = tempfile::tempdir().expect("临时目录");
        let data_dir = temp.path().join("书库");

        let (log_dir, writer) = prepare(&data_dir).expect("准备日志");

        assert_eq!(log_dir, data_dir.join(LOG_DIRECTORY));
        assert!(log_dir.is_dir());
        let today = current_day();
        assert_eq!(log_files(&log_dir), vec![log_file_name(today)]);
        writer
            .sink()
            .make_writer()
            .write_all(b"hello\n")
            .expect("写日志");
        assert_eq!(
            fs::read_to_string(log_dir.join(log_file_name(today))).expect("读日志"),
            "hello\n"
        );
    }

    #[test]
    fn the_writer_rolls_over_on_a_new_utc_day() {
        let temp = tempfile::tempdir().expect("临时目录");
        let log_dir = temp.path().join("logs");
        let day = days_from_civil(2026, 9, 14);
        let clock = Arc::new(Mutex::new(day));
        let writer = {
            let clock = Arc::clone(&clock);
            LogFileWriter::open_with_clock(
                log_dir.clone(),
                Box::new(move || *clock.lock().expect("时钟")),
            )
            .expect("打开日志")
        };

        writer
            .sink()
            .make_writer()
            .write_all(b"day one\n")
            .expect("写日志");
        *clock.lock().expect("时钟") = day + 1;
        writer
            .sink()
            .make_writer()
            .write_all(b"day two\n")
            .expect("写日志");

        assert_eq!(
            log_files(&log_dir),
            vec![
                log_file_name(day).to_string(),
                log_file_name(day + 1).to_string()
            ]
        );
        assert_eq!(
            fs::read_to_string(log_dir.join(log_file_name(day))).expect("读日志"),
            "day one\n"
        );
        assert_eq!(
            fs::read_to_string(log_dir.join(log_file_name(day + 1))).expect("读日志"),
            "day two\n"
        );
    }

    #[test]
    fn expired_logs_are_removed_and_other_files_are_kept() {
        let temp = tempfile::tempdir().expect("临时目录");
        let log_dir = temp.path().join("logs");
        fs::create_dir_all(&log_dir).expect("创建日志目录");
        let today = days_from_civil(2026, 9, 14);
        let mut expected = Vec::new();
        for offset in 0..LOG_RETENTION_DAYS {
            let name = log_file_name(today - offset);
            fs::write(log_dir.join(&name), b"x").expect("写日志");
            expected.push(name);
        }
        let expired = log_file_name(today - LOG_RETENTION_DAYS);
        fs::write(log_dir.join(&expired), b"x").expect("写日志");
        for foreign in ["other.log", "ngy-book-studio.2026-13-01.log", "notes.txt"] {
            fs::write(log_dir.join(foreign), b"x").expect("写文件");
        }
        // 同名目录不是文件，清理不动它。
        fs::create_dir_all(log_dir.join(log_file_name(today - 30))).expect("创建目录");

        let writer =
            LogFileWriter::open_with_clock(log_dir.clone(), clock(today)).expect("打开日志");
        drop(writer);

        expected.push("notes.txt".into());
        expected.push("ngy-book-studio.2026-13-01.log".into());
        expected.push("other.log".into());
        expected.push(log_file_name(today - 30));
        expected.sort();
        assert_eq!(log_files(&log_dir), expected);
        assert!(!log_dir.join(&expired).exists(), "{expired}");
    }

    #[test]
    fn an_unusable_log_directory_fails_preparation() {
        let temp = tempfile::tempdir().expect("临时目录");
        // 用一个文件占住 `logs`，创建日志目录必定失败。
        let log_dir = temp.path().join("logs");
        fs::write(&log_dir, b"x").expect("写文件");

        assert!(LogFileWriter::open(log_dir).is_err());
    }

    #[test]
    fn a_write_without_an_open_file_is_dropped_instead_of_failing() {
        let mut state = LogState {
            log_dir: PathBuf::from("不存在的日志目录"),
            open_day: None,
            file: None,
            reported_failure: false,
            now: clock(0),
        };

        assert_eq!(state.write(b"lost\n").expect("写入不报错"), 5);
        assert!(state.file.is_none(), "打不开就不留半个文件");
        assert!(state.reported_failure, "失败只报一次而不静默");
        state.flush().expect("没有文件时刷新也成功");
    }
}
