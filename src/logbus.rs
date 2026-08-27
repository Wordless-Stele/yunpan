//! 全局日志总线：dufs 的 HTTP 访问日志、隧道客户端的连接日志都发到 `log`，
//! 这里截住存进环形缓冲，「日志」页轮询展示。
//!
//! `log::set_boxed_logger` 全进程只能设一次，所以 vendor dufs 时特意没带它的
//! `logger.rs`——谁都不抢，归这里管。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const CAPACITY: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub level: log::Level,
    pub time: String,
    pub text: String,
}

static BUF: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
/// 单调递增序号。界面每秒对一次号，变了才拷贝整个缓冲——不变时零开销。
static SEQ: AtomicU64 = AtomicU64::new(0);

struct BusLogger;

impl log::Log for BusLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = LogLine {
            level: record.level(),
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            text: record.args().to_string(),
        };
        // debug 构建同时打到控制台，方便 `dx serve` 时看
        #[cfg(debug_assertions)]
        eprintln!("[{}] {}", line.level, line.text);

        if let Ok(mut buf) = BUF.lock() {
            if buf.len() >= CAPACITY {
                buf.pop_front();
            }
            buf.push_back(line);
        }
        SEQ.fetch_add(1, Ordering::Relaxed);
    }

    fn flush(&self) {}
}

/// 进程启动时装一次。装失败（比如测试里装了别的 logger）不致命，忽略。
pub fn install() {
    if log::set_boxed_logger(Box::new(BusLogger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

pub fn seq() -> u64 {
    SEQ.load(Ordering::Relaxed)
}

/// 取一份快照（新→旧）。
pub fn snapshot() -> Vec<LogLine> {
    BUF.lock()
        .map(|buf| buf.iter().rev().cloned().collect())
        .unwrap_or_default()
}
