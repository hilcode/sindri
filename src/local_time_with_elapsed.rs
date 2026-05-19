use std::fmt;
use std::time::Instant;
use time::OffsetDateTime;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

pub struct LocalTimeWithElapsed {
    start: Instant,
}

impl LocalTimeWithElapsed {
    pub fn new(start: Instant) -> LocalTimeWithElapsed {
        LocalTimeWithElapsed { start }
    }
}

impl FormatTime for LocalTimeWithElapsed {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        let now: OffsetDateTime =
            OffsetDateTime::now_local().unwrap_or_else(|_| -> OffsetDateTime { OffsetDateTime::now_utc() });
        let elapsed_ms: u64 = self.start.elapsed().as_millis() as u64;
        let millis: u64 = elapsed_ms % 1000;
        let total_seconds: u64 = elapsed_ms / 1000;
        let seconds: u64 = total_seconds % 60;
        let minutes: u64 = total_seconds / 60;
        write!(
            writer,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03} [{minutes:04}:{seconds:02}.{millis:03}]",
            now.year(),
            now.month() as u8,
            now.day(),
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn format_time_produces_expected_structure() {
        let start: Instant = Instant::now();
        let formatter: LocalTimeWithElapsed = LocalTimeWithElapsed::new(start);
        let mut output: String = String::new();
        let mut writer: Writer<'_> = Writer::new(&mut output);
        formatter.format_time(&mut writer).unwrap();
        // Expected shape: "2026-05-18T14:30:00.123 [0000:00.001]"
        assert!(output.contains('T'), "missing date/time separator");
        assert!(output.contains(" ["), "missing elapsed section open");
        assert!(output.ends_with(']'), "missing elapsed section close");
    }

    #[test]
    fn elapsed_is_near_zero_when_measured_immediately() {
        let start: Instant = Instant::now();
        let formatter: LocalTimeWithElapsed = LocalTimeWithElapsed::new(start);
        let mut output: String = String::new();
        let mut writer: Writer<'_> = Writer::new(&mut output);
        formatter.format_time(&mut writer).unwrap();
        assert!(
            output.contains("[0000:00."),
            "elapsed should show zero minutes and seconds"
        );
    }
}
