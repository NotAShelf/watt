use std::time::{
  Duration,
  Instant,
};

use serde::Serialize;

use super::{
  CpuLog,
  CpuVolatility,
  MAX_POLL_INTERVAL_SECONDS,
  System,
  idle_multiplier,
};
use crate::config;

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
  ) -> Self {
    let base = system.poll_delay_for_power_source().as_secs_f64();
    let idle_for = now.saturating_duration_since(last_activity);
    let volatility = system.cpu_volatility(now);
    let idle_factor = if system
      .is_cpu_idle_with_volatility(now, volatility.as_ref())
      && idle_for > Duration::from_secs(30)
    {
      idle_multiplier(idle_for)
    } else {
      1.0
    };
    let volatility_factor = volatility_factor(volatility.as_ref());
    let requested = (base * idle_factor * volatility_factor).max(1.0);
    let smoothed = previous.map_or(requested, |delay| {
      requested * 0.3 + delay.as_secs_f64() * 0.7
    });
    let clamped = smoothed.clamp(1.0, MAX_POLL_INTERVAL_SECONDS as f64);
    let (cpu_sample_count, temperature_sample_count) =
      CpuLog::recent(&system.cpu_log, now, Duration::from_secs(300)).fold(
        (0, 0),
        |(cpu, temperature), sample| {
          (
            cpu + 1,
            temperature + usize::from(sample.temperature.is_some()),
          )
        },
      );
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
      cpu_sample_count,
      temperature_sample_count,
      power_sample_count: system.power_supply_log.len(),
      cpu_usage_volatility_available: volatility.is_some(),
      cpu_temperature_volatility_available: volatility
        .as_ref()
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
      .cloned()
      .map(|(mut requirement, window)| {
        requirement.sample_count = window.map_or(0, |window| {
          CpuLog::recent(&system.cpu_log, now, window).count()
        });
        requirement.available =
          requirement.sample_count >= requirement.required_samples;
        requirement
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

#[derive(Debug, Clone, Default)]
pub(super) struct DurationRequirements {
  pub metrics:       Vec<(DurationMetricDiagnostics, Option<Duration>)>,
  pub omitted_count: usize,
}

pub(super) fn duration_requirements(config: &str) -> DurationRequirements {
  let mut requirements = DurationRequirements::default();
  let Ok(value) = toml::from_str::<toml::Value>(config) else {
    return requirements;
  };
  for rule in value
    .get("rule")
    .and_then(toml::Value::as_array)
    .into_iter()
    .flatten()
  {
    let name = rule.get("name").and_then(toml::Value::as_str).unwrap_or("");
    let priority = rule
      .get("priority")
      .and_then(toml::Value::as_integer)
      .and_then(|priority| u16::try_from(priority).ok())
      .unwrap_or(0);
    visit_requirements(rule, (name, priority), &mut requirements);
  }
  requirements
}

fn visit_requirements(
  value: &toml::Value,
  origin: (&str, u16),
  requirements: &mut DurationRequirements,
) {
  match value {
    toml::Value::Table(table) => {
      for (key, value) in table {
        record_requirement(key, value, origin, requirements);
        visit_requirements(value, origin, requirements);
      }
    },
    toml::Value::Array(array) => {
      for value in array {
        visit_requirements(value, origin, requirements);
      }
    },
    _ => {},
  }
}

fn record_requirement(
  key: &str,
  value: &toml::Value,
  origin: (&str, u16),
  requirements: &mut DurationRequirements,
) {
  if !matches!(key, "cpu-usage-since" | "load-average-since") {
    return;
  }
  let window = requirement_window(value);
  let window_seconds = window.map(|duration| duration.as_secs_f64());
  let duplicate =
    requirements
      .metrics
      .iter()
      .any(|(existing, existing_window)| {
        existing.metric == key
          && existing.rule == origin.0
          && existing.priority == origin.1
          && *existing_window == window
      });
  if duplicate {
    return;
  }
  if requirements.metrics.len() >= 32 {
    requirements.omitted_count = requirements.omitted_count.saturating_add(1);
    return;
  }
  requirements.metrics.push((
    DurationMetricDiagnostics {
      metric: key.to_owned(),
      rule: origin.0.to_owned(),
      priority: origin.1,
      window_seconds,
      required_samples: 1,
      ..DurationMetricDiagnostics::default()
    },
    window,
  ));
}

fn requirement_window(value: &toml::Value) -> Option<Duration> {
  let expression: config::Expression = value.clone().try_into().ok()?;
  config::verify::constant_duration(&expression)
}
