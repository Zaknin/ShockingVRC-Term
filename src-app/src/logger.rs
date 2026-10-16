use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_LOG_LINES: usize = 2000;

pub type LogBuffer = Arc<Mutex<VecDeque<String>>>;

struct TuiLogger {
    buffer: LogBuffer,
    level: LevelFilter,
    ice_diagnostics: bool,
}

fn format_timestamped_line(timestamp: &str, tag: &str, message: &str) -> String {
    format!("[{tag}] {timestamp} {message}")
}

fn local_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

impl Log for TuiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
            || (self.ice_diagnostics
                && metadata.level() == Level::Debug
                && (metadata.target().starts_with("shocking_vrc_rtc")
                    || metadata.target().starts_with("shocking_vrc_remote")
                    || metadata.target().starts_with("webrtc::peer_connection::transport::turn_relayer")
                    || metadata.target().starts_with("rtc_turn::client::relay")))
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let tag = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        let timestamp = local_timestamp();
        let line = format_timestamped_line(&timestamp, tag, &record.args().to_string());
        if let Ok(mut buf) = self.buffer.lock() {
            for sub in line.split('\n') {
                if sub.is_empty() {
                    continue;
                }
                if buf.back().is_some_and(|last| last == sub) {
                    continue;
                }
                buf.push_back(sub.to_string());
                while buf.len() > MAX_LOG_LINES {
                    buf.pop_front();
                }
            }
        }
    }

    fn flush(&self) {}
}

fn level_from_env(default_filter: &str) -> LevelFilter {
    let raw = std::env::var("RUST_LOG").unwrap_or_else(|_| default_filter.to_string());
    raw.split(',')
        .filter_map(|part| {
            let token = part.rsplit('=').next().unwrap_or(part).trim();
            token.parse::<LevelFilter>().ok()
        })
        .last()
        .unwrap_or(LevelFilter::Info)
}

pub fn init(default_filter: &str) -> LogBuffer {
    let buffer: LogBuffer = Arc::new(Mutex::new(VecDeque::with_capacity(256)));
    let level = level_from_env(default_filter);
    let ice_diagnostics = std::env::var("SHOCKINGVRC_ICE_DIAGNOSTICS")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));
    let logger = TuiLogger {
        buffer: Arc::clone(&buffer),
        level,
        ice_diagnostics,
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(if ice_diagnostics { level.max(LevelFilter::Debug) } else { level });
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::{LogBuffer, TuiLogger, format_timestamped_line};
    use log::{Level, LevelFilter, Log, Metadata};
    use std::{collections::VecDeque, sync::{Arc, Mutex}};

    fn debug_record(target: &'static str) -> Metadata<'static> {
        Metadata::builder().level(Level::Debug).target(target).build()
    }

    #[test]
    fn ice_diagnostics_enable_only_rtc_and_remote_debug_targets() {
        let logger = TuiLogger {
            buffer: Arc::new(Mutex::new(VecDeque::new())) as LogBuffer,
            level: LevelFilter::Info,
            ice_diagnostics: true,
        };

        assert!(logger.enabled(&debug_record("shocking_vrc_rtc::peer")));
        assert!(logger.enabled(&debug_record("shocking_vrc_remote::controller")));
        assert!(!logger.enabled(&debug_record("shocking_vrc_app::runtime")));
        assert!(logger.enabled(&debug_record("webrtc::peer_connection::transport::turn_relayer")));
        assert!(logger.enabled(&debug_record("rtc_turn::client::relay")));
    }

    #[test]
    fn ice_diagnostics_debug_is_suppressed_by_default() {
        let logger = TuiLogger {
            buffer: Arc::new(Mutex::new(VecDeque::new())) as LogBuffer,
            level: LevelFilter::Info,
            ice_diagnostics: false,
        };

        assert!(!logger.enabled(&debug_record("shocking_vrc_rtc::peer")));
    }

    #[test]
    fn host_log_lines_keep_level_prefix_before_local_timestamp() {
        assert_eq!(
            format_timestamped_line("2026-10-16 19:33:08.649", "WARN ", "[ble] scan error"),
            "[WARN ] 2026-10-16 19:33:08.649 [ble] scan error",
        );
    }

    #[test]
    fn local_log_timestamp_uses_iso_shape_without_timezone_suffix() {
        let timestamp = super::local_timestamp();

        assert_eq!(timestamp.len(), 23);
        assert_eq!(&timestamp[10..11], " ");
        assert_eq!(&timestamp[19..20], ".");
        assert!(timestamp[20..].chars().all(|digit| digit.is_ascii_digit()));
    }
}
