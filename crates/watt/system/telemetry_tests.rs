use std::collections::VecDeque;

use super::*;
use crate::test_support::SystemFixture;

fn discharge_history(samples: &[(u64, f64)]) -> System {
  let start = Instant::now();
  System {
    power_supply_log: samples
      .iter()
      .map(|&(seconds, charge)| {
        PowerSupplyLog {
          at: start + Duration::from_secs(seconds),
          charge,
        }
      })
      .collect::<VecDeque<_>>(),
    ..System::default()
  }
}

#[test]
fn discharge_rate_uses_elapsed_hours_and_keeps_unchanged_readings() {
  let system = discharge_history(&[(0, 0.9), (900, 0.8), (1800, 0.8)]);
  let rate = system.power_supply_discharge_rate().unwrap();
  assert!((rate - 0.2).abs() < 1e-10, "{rate}");
}

#[test]
fn discharge_rate_starts_again_after_charging() {
  let system =
    discharge_history(&[(0, 0.9), (900, 0.7), (1800, 0.9), (3600, 0.8)]);
  let rate = system.power_supply_discharge_rate().unwrap();
  assert!((rate - 0.2).abs() < 1e-10, "{rate}");
}

#[test]
fn discharge_rate_rejects_missing_or_nonincreasing_timestamps() {
  for samples in [&[][..], &[(0, 0.9)], &[(0, 0.9), (0, 0.8)], &[
    (10, 0.9),
    (0, 0.8),
  ]] {
    assert!(
      discharge_history(samples)
        .power_supply_discharge_rate()
        .is_none()
    );
  }
}

#[test]
fn discharge_rate_is_zero_for_unchanged_charge_over_time() {
  let rate = discharge_history(&[(0, 0.8), (60, 0.8)])
    .power_supply_discharge_rate()
    .unwrap();
  assert!(rate.abs() < f64::EPSILON);
}

fn cpu_history(samples: &[(u64, f64, Option<f64>)]) -> (System, Instant) {
  let now = Instant::now();
  let system = System {
    cpu_log: samples
      .iter()
      .map(|&(age, usage, temperature)| {
        CpuLog {
          at: now - Duration::from_secs(age),
          usage,
          temperature,
          load_average: 0.0,
        }
      })
      .collect(),
    ..System::default()
  };
  (system, now)
}

#[test]
fn volatility_excludes_old_spikes() {
  let (system, now) = cpu_history(&[
    (600, 1.0, Some(100.0)),
    (10, 0.01, Some(50.0)),
    (5, 0.01, Some(50.0)),
  ]);
  let volatility = system.cpu_volatility(now).unwrap();
  assert!(volatility.usage.abs() < f64::EPSILON);
  assert!(volatility.temperature.unwrap().abs() < f64::EPSILON);
  assert!(system.is_cpu_idle(now));
}

#[test]
fn volatility_does_not_bridge_missing_temperature_readings() {
  let (system, now) = cpu_history(&[
    (15, 0.1, Some(50.0)),
    (10, 0.4, None),
    (5, 0.7, Some(90.0)),
  ]);
  let volatility = system.cpu_volatility(now).unwrap();
  assert!((volatility.usage - 0.3).abs() < 1e-10);
  assert!(volatility.temperature.is_none());
}

#[test]
fn volatility_rejects_short_or_reordered_histories() {
  for samples in [
    &[][..],
    &[(0, 0.0, None)],
    &[(300, 1.0, Some(100.0)), (0, 0.0, Some(50.0))],
    &[(0, 0.0, None), (1, 0.0, None)],
    &[(0, 0.0, None), (0, 0.0, None)],
  ] {
    let (system, now) = cpu_history(samples);
    assert!(system.cpu_volatility(now).is_none());
    assert!(!system.is_cpu_idle(now));
  }
}

#[test]
fn future_cpu_samples_do_not_affect_current_volatility() {
  let (mut system, now) = cpu_history(&[(2, 0.0, None), (1, 0.0, None)]);
  system.cpu_log.push_back(CpuLog {
    at:           now + Duration::from_secs(1),
    usage:        1.0,
    temperature:  None,
    load_average: 0.0,
  });
  assert!(system.cpu_volatility(now).unwrap().usage.abs() < f64::EPSILON);
  assert!(system.is_cpu_idle(now));
}

#[test]
fn polling_ignores_small_temperature_changes_but_reacts_to_load_and_heat() {
  for (usage, temperature, expected_seconds) in
    [(0.5, 50.5, 5.0), (0.5, 52.0, 2.5), (0.8, 50.0, 2.5)]
  {
    let (system, now) =
      cpu_history(&[(2, 0.5, Some(50.0)), (1, usage, Some(temperature))]);
    assert_eq!(
      compute_poll_delay(&system, None, now, now),
      Duration::from_secs_f64(expected_seconds)
    );
  }
}

#[test]
fn polling_smooths_and_bounds_delays() {
  let (system, now) = cpu_history(&[(1, 0.5, None), (0, 0.5, None)]);
  for (previous, expected) in [(10, 8.5), (100, 30.0), (0, 1.5)] {
    assert_eq!(
      compute_poll_delay(
        &system,
        Some(Duration::from_secs(previous)),
        now,
        now
      ),
      Duration::from_secs_f64(expected)
    );
  }
  let (system, now) = cpu_history(&[(1, 0.0, None), (0, 1.0, None)]);
  assert_eq!(
    compute_poll_delay(&system, Some(Duration::ZERO), now, now),
    Duration::from_secs(1)
  );
}

#[test]
fn polling_accounts_for_battery_discharge_rate() {
  let fixture = SystemFixture::new();
  fixture.write("/sys/class/power_supply/BAT0/type", "Battery");
  fixture.write("/sys/class/power_supply/BAT0/status", "Discharging");
  for (charge, expected_seconds) in [(0.6, 15.0), (0.75, 10.0), (0.85, 7.5)] {
    let mut system = discharge_history(&[(0, 0.9), (3600, charge)]);
    system.scan_power_supplies().unwrap();
    let now = Instant::now();
    assert_eq!(
      compute_poll_delay(&system, None, now, now),
      Duration::from_secs_f64(expected_seconds)
    );
  }
}

#[test]
fn polling_lengthens_after_cpu_inactivity() {
  let (system, now) = cpu_history(&[(1, 0.01, None), (0, 0.01, None)]);
  assert_eq!(
    compute_poll_delay(&system, None, now - Duration::from_secs(120), now),
    Duration::from_secs(10)
  );
  assert_eq!(
    compute_poll_delay(&system, None, now + Duration::from_secs(1), now),
    Duration::from_secs(5)
  );
}

#[test]
fn battery_aggregation_skips_supplies_without_charge_telemetry() {
  let fixture = SystemFixture::new();
  fixture.write("/sys/class/power_supply/AC/type", "Mains");
  fixture.write("/sys/class/power_supply/BAT0/type", "Battery");
  let mut system = System::default();
  system.scan_power_supplies().unwrap();
  system.append_power_supply_log(Instant::now());
  assert!(system.power_supply_log.is_empty());

  fixture.write("/sys/class/power_supply/BAT0/capacity", "50");
  system.scan_power_supplies().unwrap();
  system.append_power_supply_log(Instant::now());
  assert_eq!(system.power_supply_log.len(), 1);
  assert!((system.power_supply_log[0].charge - 0.5).abs() < f64::EPSILON);
}
