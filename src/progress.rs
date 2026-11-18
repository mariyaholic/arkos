//! Throttled scan progress on stderr.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_millis(500);

/// Prints `scanned N files, M dirs, X` on one overwritten line at most every 500 ms.
///
/// A reporter built with [`Reporter::auto`] is silent unless stderr is a terminal and the caller
/// is not producing JSON. Silent reporters cost one boolean check per tick.
pub struct Reporter {
    sink: Option<Box<dyn Write + Send>>,
    interval: Duration,
    last: Option<Instant>,
    printed: bool,
    /// Width of the line on screen, so the next line or the clear can overwrite all of it.
    width: usize,
    heartbeat: Option<crate::lock::Heartbeat>,
}

impl Reporter {
    /// A reporter that never prints.
    pub fn off() -> Self {
        Self {
            sink: None,
            interval: INTERVAL,
            last: None,
            printed: false,
            width: 0,
            heartbeat: None,
        }
    }

    /// Print to stderr when it is a terminal and `json` output is not requested.
    pub fn auto(json: bool) -> Self {
        if !json && std::io::stderr().is_terminal() {
            Self::to_writer(Box::new(std::io::stderr()), INTERVAL)
        } else {
            Self::off()
        }
    }

    /// Print to `sink`, at most once per `interval`.
    pub fn to_writer(sink: Box<dyn Write + Send>, interval: Duration) -> Self {
        Self {
            sink: Some(sink),
            interval,
            last: None,
            printed: false,
            width: 0,
            heartbeat: None,
        }
    }

    /// Beat the scan's lock sidecar from every tick.
    pub fn set_heartbeat(&mut self, heartbeat: crate::lock::Heartbeat) {
        self.heartbeat = Some(heartbeat);
    }

    /// Stop beating, once the scan no longer holds its lock.
    pub fn clear_heartbeat(&mut self) {
        self.heartbeat = None;
    }

    /// Report current totals; prints only when the interval has passed since the last line.
    pub fn tick(&mut self, files: u64, dirs: u64, bytes: u64) {
        if let Some(hb) = self.heartbeat.as_mut() {
            hb.beat(files);
        }
        if self.sink.is_none() {
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last {
            if now.duration_since(last) < self.interval {
                return;
            }
        }
        self.last = Some(now);
        let mut line = format!(
            "scanned {files} files, {dirs} dirs, {}",
            crate::fmt::format_size(bytes.min(i64::MAX as u64) as i64)
        );
        let shown = line.chars().count();
        // Pad with spaces over what a longer previous line left behind; no escape sequences, so
        // consoles without virtual terminal processing draw it the same way.
        if shown < self.width {
            line.push_str(&" ".repeat(self.width - shown));
        }
        let out = format!("\r{line}");
        if let Some(sink) = self.sink.as_mut() {
            let _ = sink.write_all(out.as_bytes());
            let _ = sink.flush();
            self.printed = true;
            self.width = self.width.max(shown);
        }
    }

    /// Clear the progress line, if one was printed.
    pub fn finish(&mut self) {
        if !self.printed {
            return;
        }
        if let Some(sink) = self.sink.as_mut() {
            let out = format!("\r{}\r", " ".repeat(self.width));
            let _ = sink.write_all(out.as_bytes());
            let _ = sink.flush();
        }
        self.width = 0;
        self.printed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn off_reporter_prints_nothing() {
        let mut r = Reporter::off();
        r.tick(1, 1, 1);
        r.finish();
    }

    #[test]
    fn ticks_inside_the_interval_are_dropped() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let mut r = Reporter::to_writer(Box::new(Shared(buf.clone())), Duration::from_secs(3600));
        r.tick(10, 2, 2048);
        r.tick(20, 4, 4096);
        let out = text(&buf);
        assert_eq!(out.matches("scanned").count(), 1);
        assert!(out.contains("scanned 10 files, 2 dirs, 2.0 KiB"));
    }

    #[test]
    fn zero_interval_prints_every_tick_and_finish_clears() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let mut r = Reporter::to_writer(Box::new(Shared(buf.clone())), Duration::ZERO);
        r.tick(1, 1, 0);
        r.tick(2, 1, 0);
        assert_eq!(text(&buf).matches("scanned").count(), 2);
        r.finish();
        let out = text(&buf);
        assert!(out.ends_with('\r'));
        assert!(!out.contains('\x1b'));
        let clear = out.rsplit("\r").nth(1).unwrap();
        assert!(clear.len() >= "scanned 2 files, 1 dirs, 0 B".len() && clear.trim().is_empty());
    }
}
