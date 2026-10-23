use std::time::{
  Duration,
  Instant,
};

use serde::Serialize;

use super::{
  CpuHistory,
  CpuLog,
  CpuVolatility,
  MAX_POLL_INTERVAL_SECONDS,
  System,
  idle_multiplier,
};
use crate::config::{
  self,
  verify::visit_rule_duration_windows,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DurationMetricDiagnostics {
  pub metric:           String,
  pub rule:             String,
  pub priority:         u16,
  pub window_seconds:   Option<f64>,
  pub required_samples: usize,
  pub sample_count:     usize,
  pub available:        bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PollingDiagnostics {
  pub base_interval_seconds:                f64,
  pub current_interval_seconds:             f64,
  pub next_interval_seconds:                f64,
  pub idle_factor:                          f64,
  pub volatility_factor:                    f64,
  pub requested_delay_seconds:              f64,
  pub smoothed_delay_seconds:               f64,
  pub clamped_delay_seconds:                f64,
  pub evaluation_elapsed_seconds:           f64,
  pub cpu_sample_count:                     usize,
  pub temperature_sample_count:             usize,
  pub power_sample_count:                   usize,
  pub cpu_usage_volatility_available:       bool,
  pub cpu_temperature_volatility_available: bool,
  pub power_discharge_rate_available:       bool,
  pub volatility_required_samples:          usize,
  pub power_discharge_required_samples:     usize,
  pub duration_metrics:                     Vec<DurationMetricDiagnostics>,
  pub duration_metrics_omitted:             usize,
}

impl PollingDiagnostics {
  pub(super) fn calculate(
    system: &System,
    previous: Option<Duration>,
    last_activity: Instant,
    now: Instant,
    history: &CpuHistory,
  ) -> Self {
    let base = system.poll_delay_for_power_source().as_secs_f64();
    let idle_for = now.saturating_duration_since(last_activity);
    let volatility = history.volatility.as_ref();
    let idle_factor = if history.idle && idle_for > Duration::from_secs(30) {
      idle_multiplier(idle_for)
    } else {
      1.0
    };
    let volatility_factor = volatility_factor(volatility);
    let requested = (base * idle_factor * volatility_factor).max(1.0);
    let smoothed = previous.map_or(requested, |delay| {
      requested * 0.3 + delay.as_secs_f64() * 0.7
    });
    let clamped = smoothed.clamp(1.0, MAX_POLL_INTERVAL_SECONDS as f64);
    Self {
      base_interval_seconds: base,
      current_interval_seconds: previous
        .map_or(base, |delay| delay.as_secs_f64()),
      next_interval_seconds: clamped,
      idle_factor,
      volatility_factor,
      requested_delay_seconds: requested,
      smoothed_delay_seconds: smoothed,
      clamped_delay_seconds: clamped,
      cpu_sample_count: history.samples,
      temperature_sample_count: history.temperature_samples,
      power_sample_count: system.power_supply_log.len(),
      cpu_usage_volatility_available: volatility.is_some(),
      cpu_temperature_volatility_available: volatility
        .is_some_and(|volatility| volatility.temperature.is_some()),
      power_discharge_rate_available: system
        .power_supply_discharge_rate()
        .is_some(),
      volatility_required_samples: 2,
      power_discharge_required_samples: 2,
      ..Self::default()
    }
  }

  pub(super) fn update_requirements(
    &mut self,
    requirements: &DurationRequirements,
    system: &System,
    now: Instant,
  ) {
    self.duration_metrics_omitted = requirements.omitted_count;
    self.duration_metrics = requirements
      .metrics
      .iter()
      .map(|requirement| {
        let sample_count = requirement.window.map_or(0, |window| {
          CpuLog::recent(&system.cpu_log, now, window).count()
        });
        DurationMetricDiagnostics {
          metric: requirement.metric.to_owned(),
          rule: requirement.rule.clone(),
          priority: requirement.priority,
          window_seconds: requirement.window.map(|window| window.as_secs_f64()),
          required_samples: 1,
          sample_count,
          available: sample_count > 0,
        }
      })
      .collect();
  }
}

fn volatility_factor(volatility: Option<&CpuVolatility>) -> f64 {
  let volatile = volatility.is_some_and(|volatility| {
    volatility.usage > 0.1
      || volatility.temperature.is_some_and(|value| value > 1.0)
  });
  if volatile { 0.5 } else { 1.0 }
}

#[derive(Debug)]
struct DurationRequirement {
  metric:   &'static str,
  rule:     String,
  priority: u16,
  window:   Option<Duration>,
}

#[derive(Debug, Default)]
pub(super) struct DurationRequirements {
  metrics:       Vec<DurationRequirement>,
  omitted_count: usize,
}

pub(super) fn duration_requirements(
  rules: &[config::Rule],
) -> DurationRequirements {
  const LIMIT: usize = 32;

  let mut requirements = DurationRequirements::default();
  for rule in rules {
    visit_rule_duration_windows(rule, |metric, window| {
      if requirements.metrics.iter().any(|existing| {
        existing.metric == metric
          && existing.rule == rule.name
          && existing.priority == rule.priority
          && existing.window == window
      }) {
        return;
      }
      if requirements.metrics.len() == LIMIT {
        requirements.omitted_count =
          requirements.omitted_count.saturating_add(1);
      } else {
        requirements.metrics.push(DurationRequirement {
          metric,
          rule: rule.name.clone(),
          priority: rule.priority,
          window,
        });
      }
    });
  }
  requirements
}
