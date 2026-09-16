//! 日志落盘：tracing 的全局 subscriber 由 `ngy_utils_tracing` 安装（见 `src/main.rs`
//! 的 `install_logging`）。本模块只负责把日志目录固定到 `<数据目录>/logs/`、提供默认
//! 过滤串，并在打开日志目录前清理超过保留期的旧文件。
//!
//! 日志目录固定是 `<数据目录>/logs/`，不单独让用户选：数据目录已经由用户指定，
//! 再问一次只会增加启动成本。文件名是 `ngy-book-studio.<YYYY-MM-DD>.log`，按 **UTC**
//! 日期滚动（与控制台默认时间戳、后台任务日志的 UTC 约定一致），最多保留
//! [`LOG_RETENTION_DAYS`] 天，进程启动时清理更早的同类文件。

use std::{
    fs,
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// 数据目录下的日志子目录名。
pub const LOG_DIRECTORY: &str = "logs";
/// 日志文件前缀；`ngy_utils_tracing` 在 Production/Test 模式下按日滚动为
/// `<前缀>.<YYYY-MM-DD>`（可能带 `.log` 后缀）。
pub const LOG_PREFIX: &str = "ngy-book-studio";
/// 保留天数，含当天。
pub const LOG_RETENTION_DAYS: i64 = 7;
/// 未设置 `RUST_LOG` 时的默认过滤，控制台与文件一致。
pub const DEFAULT_LOG_FILTER: &str = "warn,ngy_ai=info,ngy_import=debug,ngy_reader=debug";

/// 数据目录对应的日志目录。
pub fn log_directory(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIRECTORY)
}

/// 建目录并清理超过保留期的日志文件，在调用方用 `ngy_utils_tracing::init` 安装文件层
/// 之前或之后调用皆可。目录建不出来不是致命错误：降级路径（控制台）仍可用。
pub fn prepare(data_dir: &Path) {
    let log_dir = log_directory(data_dir);
    if let Err(error) = fs::create_dir_all(&log_dir) {
        eprintln!(
            "[ngy-book-studio] 无法创建日志目录 {}：{error}；本进程可能无法写入文件日志",
            log_dir.display()
        );
        return;
    }
    if let Err(error) = prune_expired(&log_dir, current_day()) {
        eprintln!("[ngy-book-studio] 清理过期日志失败（不影响启动）：{error}");
    }
}

/// 清理超过保留期的日志文件。只删除本目录下名字以 [`LOG_PREFIX`] 开头、且日期部分可
/// 解析为合法 `YYYY-MM-DD` 的普通文件，其它文件一律不动。
fn prune_expired(log_dir: &Path, today: i64) -> io::Result<()> {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(name) => name,
            None => continue,
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

/// 解析本模块关心的文件名：`ngy-book-studio.<YYYY-MM-DD>` 或
/// `ngy-book-studio.<YYYY-MM-DD>.log`，返回对应 UTC 天数。
fn parse_log_file_day(name: &str) -> Option<i64> {
    let rest = name.strip_prefix(LOG_PREFIX)?.strip_prefix('.')?;
    let date = rest.strip_suffix(".log").unwrap_or(rest);
    parse_date(date)
}

/// 解析 `YYYY-MM-DD` 为自 1970-01-01 起的天数，非法格式返回 `None`。
fn parse_date(date: &str) -> Option<i64> {
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
#[cfg(test)]
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
    fn log_file_names_parse_only_our_prefix() {
        let day = days_from_civil(2026, 9, 14);
        assert_eq!(parse_log_file_day("ngy-book-studio.2026-09-14.log"), Some(day));
        assert_eq!(parse_log_file_day("ngy-book-studio.2026-09-14"), Some(day));
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
    fn prepare_creates_the_log_directory() {
        let temp = tempfile::tempdir().expect("临时目录");
        let data_dir = temp.path().join("书库");

        prepare(&data_dir);

        let log_dir = log_directory(&data_dir);
        assert!(log_dir.is_dir());
    }

    #[test]
    fn expired_logs_are_removed_and_other_files_are_kept() {
        let temp = tempfile::tempdir().expect("临时目录");
        let data_dir = temp.path().join("书库");
        let log_dir = log_directory(&data_dir);
        fs::create_dir_all(&log_dir).expect("创建日志目录");
        let today = current_day();
        let mut expected: Vec<String> = Vec::new();
        for offset in 0..LOG_RETENTION_DAYS {
            let name = format!("{LOG_PREFIX}.{}.log", civil_date(today - offset));
            fs::write(log_dir.join(&name), b"x").expect("写日志");
            expected.push(name);
        }
        let expired = format!("{LOG_PREFIX}.{}.log", civil_date(today - LOG_RETENTION_DAYS));
        fs::write(log_dir.join(&expired), b"x").expect("写日志");
        for foreign in ["other.log", "notes.txt"] {
            fs::write(log_dir.join(foreign), b"x").expect("写文件");
        }

        prepare(&data_dir);

        expected.push("notes.txt".into());
        expected.push("other.log".into());
        expected.sort();
        let mut names: Vec<String> = fs::read_dir(&log_dir)
            .expect("读取日志目录")
            .map(|entry| entry.expect("目录项").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, expected);
        assert!(!log_dir.join(&expired).exists(), "{expired}");
    }

    fn civil_date(day: i64) -> String {
        let (year, month, date) = civil_from_days(day);
        format!("{year:04}-{month:02}-{date:02}")
    }
}
