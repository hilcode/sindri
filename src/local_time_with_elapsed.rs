use crate::types::BuildStart;
use std::time::Duration;
use time::OffsetDateTime;
use time::UtcOffset;

/// Renders a log-line prefix combining the local wall-clock time with the elapsed time since a
/// fixed start. The timezone offset is captured once (see [`crate::runtime::Bootstrap::into_runtime`])
/// so the prefix is reproducible and never has to consult the system timezone again.
pub struct LocalTimeWithElapsed {
    start: BuildStart,
    offset: UtcOffset,
}

impl LocalTimeWithElapsed {
    pub fn new(start: BuildStart, offset: UtcOffset) -> LocalTimeWithElapsed {
        LocalTimeWithElapsed { start, offset }
    }

    pub fn format(&self) -> String {
        let now: OffsetDateTime = OffsetDateTime::now_utc().to_offset(self.offset);
        let elapsed: Duration = self.start.elapsed();
        let total_seconds: u64 = elapsed.as_secs();
        let millis: u32 = elapsed.subsec_millis();
        let seconds: u64 = total_seconds % 60;
        let minutes: u64 = total_seconds / 60;
        format!(
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

    #[test]
    fn format_produces_expected_structure() {
        let formatter: LocalTimeWithElapsed = LocalTimeWithElapsed::new(BuildStart::now(), UtcOffset::UTC);
        let output: String = formatter.format();
        // Expected shape: "2026-05-18T14:30:00.123 [0000:00.001]"
        assert!(output.contains('T'), "missing date/time separator");
        assert!(output.contains(" ["), "missing elapsed section open");
        assert!(output.ends_with(']'), "missing elapsed section close");
    }

    #[test]
    fn elapsed_is_near_zero_when_measured_immediately() {
        let formatter: LocalTimeWithElapsed = LocalTimeWithElapsed::new(BuildStart::now(), UtcOffset::UTC);
        let output: String = formatter.format();
        assert!(
            output.contains("[0000:00."),
            "elapsed should show zero minutes and seconds"
        );
    }
}
