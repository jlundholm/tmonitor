use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static LOG_WRITE_ERROR: AtomicBool = AtomicBool::new(false);

struct FileLogger {
    writer: Mutex<BufWriter<std::fs::File>>,
    level: log::LevelFilter,
}

impl FileLogger {
    fn has_write_error() -> bool {
        LOG_WRITE_ERROR.load(Ordering::Relaxed)
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut writer = self.writer.lock().unwrap();
        if writeln!(
            writer,
            "[{}] [{}] {}",
            chrono_now(),
            record.level(),
            record.args()
        ).is_err() {
            LOG_WRITE_ERROR.store(true, Ordering::Relaxed);
        }
    }

    fn flush(&self) {
        if self.writer.lock().unwrap().flush().is_err() {
            LOG_WRITE_ERROR.store(true, Ordering::Relaxed);
        }
    }
}

/// Returns current UTC time as ISO 8601 string.
///
/// NOTE: Uses `SystemTime::now()` which returns the system clock time.
/// On Linux (where the hardware clock is typically UTC), this produces correct UTC timestamps.
/// On systems where the hardware clock is set to local time, timestamps may be incorrect.
/// For precise UTC timestamps on all platforms, consider using the `chrono` crate.
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = duration.as_secs();

    let days_since_epoch = total_secs / 86400;
    let time_secs = total_secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    let mut y = 1970i64;
    let mut remaining = days_since_epoch as i64;
    loop {
        let days_in_year = if is_leap_year(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }

    let days_in_months = if is_leap_year(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 1u32;
    for &dm in &days_in_months {
        if remaining < dm {
            break;
        }
        remaining -= dm;
        m += 1;
    }

    let d = (remaining + 1) as u32;

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hours, minutes, seconds)
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("failed to open log file: {0}")]
    Io(#[from] std::io::Error),
}

pub fn init(path: &Path, level: &str) -> Result<(), LoggingError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    let level_filter = match level {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };

    let logger = FileLogger {
        writer: Mutex::new(BufWriter::new(file)),
        level: level_filter,
    };

    log::set_boxed_logger(Box::new(logger))
        .map(|()| log::set_max_level(level_filter))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        .map_err(LoggingError::Io)?;

    Ok(())
}

pub fn has_write_error() -> bool {
    FileLogger::has_write_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;
    use std::io::Read;

    #[test]
    fn test_chrono_now_format() {
        let ts = chrono_now();
        assert_eq!(ts.len(), 20, "expected ISO 8601 format length");
        assert!(ts.ends_with('Z'), "expected UTC timezone");
        assert_eq!(&ts[4..5], "-", "expected ISO 8601 format YYYY-MM-DD");
        assert_eq!(&ts[7..8], "-", "expected ISO 8601 format YYYY-MM-DD");
        assert_eq!(&ts[10..11], "T", "expected ISO 8601 format T separator");
        assert_eq!(&ts[13..14], ":", "expected ISO 8601 time separator");
        assert_eq!(&ts[16..17], ":", "expected ISO 8601 time separator");
    }

    #[test]
    fn test_is_leap_year() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
    }

    #[test]
    fn test_file_logger_enabled() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let logger = FileLogger {
            writer: Mutex::new(BufWriter::new(file.reopen().unwrap())),
            level: log::LevelFilter::Warn,
        };

        let err_rec = log::Record::builder().args(format_args!("error")).level(log::Level::Error).build();
        let warn_rec = log::Record::builder().args(format_args!("warn")).level(log::Level::Warn).build();
        let info_rec = log::Record::builder().args(format_args!("info")).level(log::Level::Info).build();
        let dbg_rec = log::Record::builder().args(format_args!("debug")).level(log::Level::Debug).build();
        let err_meta = err_rec.metadata();
        let warn_meta = warn_rec.metadata();
        let info_meta = info_rec.metadata();
        let dbg_meta = dbg_rec.metadata();

        assert!(logger.enabled(&err_meta));
        assert!(logger.enabled(&warn_meta));
        assert!(!logger.enabled(&info_meta));
        assert!(!logger.enabled(&dbg_meta));
    }

    #[test]
    fn test_file_logger_writes_output() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let logger = FileLogger {
            writer: Mutex::new(BufWriter::new(file.reopen().unwrap())),
            level: log::LevelFilter::Info,
        };

        logger.log(&log::Record::builder().args(format_args!("test message")).level(log::Level::Info).build());

        drop(logger);

        let mut contents = String::new();
        std::fs::File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
        assert!(contents.contains("[INFO]"), "expected INFO level in log output");
        assert!(contents.contains("test message"), "expected message in log output");
    }

    #[test]
    fn test_file_logger_level_filter_excludes_lower() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let logger = FileLogger {
            writer: Mutex::new(BufWriter::new(file.reopen().unwrap())),
            level: log::LevelFilter::Warn,
        };

        logger.log(&log::Record::builder().args(format_args!("should appear")).level(log::Level::Warn).build());
        logger.log(&log::Record::builder().args(format_args!("should not appear")).level(log::Level::Debug).build());

        drop(logger);

        let mut contents = String::new();
        std::fs::File::open(file.path()).unwrap().read_to_string(&mut contents).unwrap();
        assert!(contents.contains("should appear"), "expected warn message");
        assert!(!contents.contains("should not appear"), "debug should have been filtered");
    }

    #[test]
    fn test_logging_init_creates_file_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");

        let result = init(&log_path, "debug");
        assert!(result.is_ok(), "init should succeed: {:?}", result.err());

        assert!(log_path.exists());

        log::debug!("debug test message");
        log::info!("info test message");
        log::logger().flush();

        let mut contents = String::new();
        std::fs::File::open(&log_path).unwrap().read_to_string(&mut contents).unwrap();
        assert!(contents.contains("debug test message"));
        assert!(contents.contains("info test message"));
        assert!(contents.contains("[DEBUG]"));
        assert!(contents.contains("[INFO]"));
    }
}
