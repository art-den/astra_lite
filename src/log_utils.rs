use std::{path::*, fs};
use flexi_logger::*;

pub struct TimeLogger {
    start_time: std::time::Instant,
}

impl TimeLogger {
    pub fn start() -> TimeLogger {
        TimeLogger { start_time: std::time::Instant::now() }
    }

    pub fn log(self, text: &str) {
        let time = self.start_time.elapsed().as_secs_f64();
        log::debug!("BENCH {} time = {:.6} s", text, time);
    }
}

pub fn start_logger(log_path: &Path) -> anyhow::Result<()> {
    let custom_format_fun = |
        w:      &mut dyn std::io::Write,
        now:    &mut DeferredNow,
        record: &Record
    | -> Result<(), std::io::Error> {
        write!(
            w, "[{}] {} {}",
            now.format(TS_DASHES_BLANK_COLONS_DOT_BLANK),
            record.level(),
            record.args()
        )
    };

    Logger::try_with_str("trace")?
        .log_to_file(
            FileSpec::default()
                .directory(log_path)
                .basename(env!("CARGO_PKG_NAME"))
        )
        .format(custom_format_fun)
        .print_message()
        .start()?;

    Ok(())
}

pub fn cleanup_old_logs(log_path: &Path, days_to_save: usize) {
    let max_elapsed = days_to_save as u64 * 24 * 60 * 60;
    let Ok(dir_contents) = fs::read_dir(log_path) else { return; };
    for item in dir_contents.filter_map(|e| e.ok()) {
        let Ok(metadata) = item.metadata() else { continue; };
        if !metadata.is_file() { continue; }
        let path = item.path();
        let ext = path.extension().unwrap_or_default().to_str().unwrap_or_default();
        if !ext.eq_ignore_ascii_case("log") { continue; }
        let Ok(modified) = metadata.modified() else { continue; };
        let Ok(elapsed) = modified.elapsed() else { continue; };
        if elapsed.as_secs() > max_elapsed {
            _ = fs::remove_file(&path);
        }
    }
}