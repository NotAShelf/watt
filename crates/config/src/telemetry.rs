use std::{
  collections::VecDeque,
  time::{
    Duration,
    Instant,
  },
};

pub const HISTORY_SAMPLE_LIMIT: usize = 100;
pub const MAX_POLL_INTERVAL_SECONDS: u64 = 30;

#[derive(Debug, Clone, PartialEq)]
pub struct CpuLog {
  pub at: Instant,

  /// CPU usage between 0-1, a percentage.
  pub usage: f64,

  /// CPU temperature in celsius, if available.
  pub temperature: Option<f64>,

  /// Load average.
  pub load_average: f64,
}

impl CpuLog {
  pub fn recent(
    logs: &VecDeque<Self>,
    now: Instant,
    window: Duration,
  ) -> impl Iterator<Item = &Self> {
    logs.iter().filter(move |log| {
      now
        .checked_duration_since(log.at)
        .is_some_and(|age| age < window)
    })
  }
}
