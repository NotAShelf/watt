use std::{
  collections::{
    HashSet,
    VecDeque,
  },
  time::{
    Duration,
    Instant,
  },
};

use super::*;
use crate::{
  profile::PowerProfile,
  system::CpuLog,
};

#[test]
fn usage_windows_select_default_rules_with_sparse_samples() {
  let cpus = HashSet::new();
  let power_supplies = HashSet::new();
  let uncores = HashSet::new();
  let disks = HashSet::new();
  let usb_devices = HashSet::new();
  let gpus = HashSet::new();
  let config = DaemonConfig::load_from(None).expect("load default rules");

  // Samples are already interval usage measurements, even when polls are
  // farther apart than the rule's window. Old samples must not dilute them.
  for (samples, expected_usage, expected_rule) in [
    (vec![], None, "default-balanced"),
    (vec![(5, 0.9)], None, "default-balanced"),
    (
      vec![(5, 0.0), (0, 0.5)],
      Some(0.5),
      "plugged-in-performance",
    ),
    (
      vec![(5, 0.0), (0, 0.9)],
      Some(0.9),
      "high-load-performance-sustainance",
    ),
    (
      vec![(0, 0.4), (0, 0.8)],
      Some(0.6),
      "plugged-in-performance",
    ),
    (vec![(5, 0.9), (0, 0.0)], Some(0.0), "default-balanced"),
  ] {
    let now = Instant::now();
    let cpu_log = samples
      .into_iter()
      .map(|(age, usage)| {
        CpuLog {
          at: now - Duration::from_secs(age),
          usage,
          temperature: Some(50.0),
          load_average: 0.0,
        }
      })
      .collect::<VecDeque<_>>();
    let state = EvalState {
      frequency_available:         false,
      turbo_available:             false,
      cpu_usage:                   0.0,
      cpu_usage_volatility:        None,
      cpu_temperature:             Some(50.0),
      cpu_temperature_volatility:  None,
      cpu_idle_seconds:            0.0,
      cpu_frequency_maximum:       None,
      cpu_frequency_minimum:       None,
      lid_closed:                  false,
      virtual_machine:             false,
      chassis_type:                None,
      power_supply_charge:         Some(0.99),
      power_supply_discharge_rate: None,
      battery_cycles:              None,
      battery_health:              None,
      discharging:                 false,
      power_profile_preference:    PowerProfile::Balanced,
      context:                     EvalContext::WidestPossible,
      cpus:                        &cpus,
      uncores:                     &uncores,
      disks:                       &disks,
      usb_devices:                 &usb_devices,
      gpus:                        &gpus,
      power_supplies:              &power_supplies,
      cpu_log:                     &cpu_log,
    };
    let expression = Expression::CpuUsageSince {
      duration: Box::new(Expression::String("1sec".into())),
    };
    let actual = expression
      .eval(&state)
      .expect("evaluate usage")
      .map(|value| value.try_into_number().expect("numeric usage"));
    assert_eq!(actual.is_some(), expected_usage.is_some());
    if let Some((actual, expected)) = actual.zip(expected_usage) {
      assert!((actual - expected).abs() < 1e-10);
    }
    let selected = config
      .rules
      .iter()
      .filter(|rule| {
        rule.condition.eval(&state).expect("evaluate rule")
          == Some(Expression::Boolean(true))
      })
      .max_by_key(|rule| rule.priority)
      .expect("fallback rule");
    assert_eq!(selected.name, expected_rule);

    let zero_window = Expression::CpuUsageSince {
      duration: Box::new(Expression::String("0sec".into())),
    };
    assert_eq!(zero_window.eval(&state).unwrap(), None);
  }
}
