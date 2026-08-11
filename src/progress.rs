use std::time::{Duration, Instant};

/// Emit rate-limited progress updates for long-running, countable work.
/// Callers print their own stage-start message so there is immediate feedback;
/// this reporter adds throughput and an ETA once at least one item is done.
pub struct Progress {
    label: &'static str,
    unit: &'static str,
    total: usize,
    started: Instant,
    last_reported: Instant,
}

impl Progress {
    const REPORT_INTERVAL: Duration = Duration::from_secs(5);

    pub fn new(label: &'static str, unit: &'static str, total: usize) -> Self {
        let now = Instant::now();
        Self { label, unit, total, started: now, last_reported: now }
    }

    /// Report at most once per interval, while always reporting completion.
    pub fn update(&mut self, completed: usize) {
        let now = Instant::now();
        if completed < self.total
            && now.duration_since(self.last_reported) < Self::REPORT_INTERVAL
        {
            return;
        }
        self.last_reported = now;

        let completed = completed.min(self.total);
        let elapsed = now.duration_since(self.started);
        let elapsed_secs = elapsed.as_secs_f64();
        let rate = if elapsed_secs > 0.0 { completed as f64 / elapsed_secs } else { 0.0 };
        let eta = if completed == 0 || rate == 0.0 {
            None
        } else {
            Some(Duration::from_secs_f64((self.total - completed) as f64 / rate))
        };
        let percent = if self.total == 0 {
            100.0
        } else {
            completed as f64 * 100.0 / self.total as f64
        };

        match eta {
            Some(eta) => eprintln!(
                "{} {}/{} {} ({percent:.1}%) — {:.1} {}/s, ETA {}",
                self.label,
                completed,
                self.total,
                self.unit,
                rate,
                self.unit,
                format_duration(eta),
            ),
            None => eprintln!(
                "{} {}/{} {} ({percent:.1}%)",
                self.label, completed, self.total, self.unit
            ),
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_compact_and_human_readable() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(format_duration(Duration::from_secs(7380)), "2h 03m");
    }
}
