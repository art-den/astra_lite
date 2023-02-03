use std::path::*;
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
