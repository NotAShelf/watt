use std::{
  collections::HashSet,
  sync::Arc,
};

use super::{
  test_support::with_cpu_state,
  *,
};
use crate::test_support::SystemFixture;

fn frequency(mhz: f64) -> cpu::frequency::Frequency {
  cpu::frequency::Frequency::from_mhz(mhz).expect("valid test frequency")
}

#[test]
fn default_frequency_rule_uses_each_cpu_hardware_range() {
  let available_cpu = Arc::new(cpu::Cpu {
    number: 0,
    frequency_control: true,
    frequency: Some(frequency(1000.0)),
    frequency_minimum: Some(frequency(702.4)),
    frequency_maximum: Some(frequency(3504.0)),
    ..cpu::Cpu::default()
  });
  let unavailable_cpu = Arc::new(cpu::Cpu {
    number: 6,
    ..cpu::Cpu::default()
  });
  let cpus =
    HashSet::from([Arc::clone(&available_cpu), Arc::clone(&unavailable_cpu)]);
  with_cpu_state(&cpus, |state| {
    let config = DaemonConfig::load_from(None).expect("load default config");
    let rule = config
      .rules
      .iter()
      .find(|rule| rule.name == "battery-balanced")
      .expect("default battery-balanced rule");

    let (deltas, _) = rule.cpu.eval(state).expect("evaluate CPU deltas");

    assert_eq!(
      deltas
        .get(&available_cpu)
        .and_then(|delta| delta.frequency_minimum)
        .map(cpu::frequency::Frequency::as_khz),
      Some(702_400)
    );
    assert_eq!(
      deltas
        .get(&unavailable_cpu)
        .and_then(|delta| delta.frequency_minimum),
      None
    );
  });
}

#[test]
fn cpu_rules_reject_invalid_selectors_before_producing_actions() {
  let cpus = HashSet::from([Arc::new(cpu::Cpu::default())]);
  with_cpu_state(&cpus, |state| {
    for number in [
      -1.0,
      0.5,
      f64::from(u32::MAX) + 1.0,
      f64::NAN,
      f64::INFINITY,
    ] {
      let rule = CpusDelta {
        for_: Some(Expression::List(vec![Expression::Number(number)])),
        governor: Some(Expression::String("performance".into())),
        ..CpusDelta::default()
      };
      assert!(rule.eval(state).is_err(), "accepted CPU selector {number}");
    }
    let rule = CpusDelta {
      for_: Some(Expression::List(vec![
        Expression::Number(0.0),
        Expression::Number(0.0),
      ])),
      governor: Some(Expression::String("performance".into())),
      turbo: Some(Expression::Boolean(false)),
      ..CpusDelta::default()
    };
    let (deltas, global) = rule.eval(state).unwrap();
    assert_eq!(deltas.len(), 1);
    assert_eq!(
      deltas.values().next().unwrap().governor.as_deref(),
      Some("performance")
    );
    assert_eq!(global.turbo, Some(false));

    let empty = CpusDelta {
      for_: Some(Expression::List(vec![])),
      ..rule
    };
    let (deltas, global) = empty.eval(state).unwrap();
    assert!(deltas.is_empty());
    assert_eq!(global.turbo, Some(false));
  });
}

#[test]
fn resume_latency_rules_reject_unrepresentable_values() {
  let cpus = HashSet::from([Arc::new(cpu::Cpu::default())]);
  with_cpu_state(&cpus, |state| {
    for value in [-1.0, 0.5, u64::MAX as f64, f64::NAN, f64::INFINITY] {
      let rule = CpusDelta {
        pm_qos_resume_latency_us: Some(Expression::Number(value)),
        ..CpusDelta::default()
      };
      assert!(rule.eval(state).is_err(), "accepted resume latency {value}");
    }
    for (expression, expected) in [
      (Expression::Number(0.0), "0"),
      (Expression::Number(100.0), "100"),
      (Expression::String("n/a".into()), "n/a"),
    ] {
      let rule = CpusDelta {
        pm_qos_resume_latency_us: Some(expression),
        ..CpusDelta::default()
      };
      let (deltas, _) = rule.eval(state).unwrap();
      assert_eq!(
        deltas
          .values()
          .next()
          .unwrap()
          .pm_qos_resume_latency_us
          .as_deref(),
        Some(expected)
      );
    }
  });
}

#[test]
fn shared_policy_rules_preserve_priority_and_per_cpu_settings() {
  let fixture = SystemFixture::new();
  fixture.policy(0, &[0, 1], "intel_pstate");
  fixture.policy(2, &[2], "acpi-cpufreq");
  fixture.cpu_telemetry(&[0, 1, 2]);
  let cpus = cpu::Cpu::all().unwrap().into_iter().map(Arc::new).collect();
  with_cpu_state(&cpus, |state| {
    let high = CpusDelta {
      for_: Some(Expression::List(vec![Expression::Number(1.0)])),
      governor: Some(Expression::String("performance".into())),
      energy_perf_bias: Some(Expression::String("performance".into())),
      ..CpusDelta::default()
    };
    let low = CpusDelta {
      governor: Some(Expression::String("powersave".into())),
      ..CpusDelta::default()
    };
    let (high, _) = high.eval(state).unwrap();
    let (low, _) = low.eval(state).unwrap();
    assert_eq!(high.len(), 2);
    for cpu in &cpus {
      let delta = high.get(cpu).cloned().unwrap_or_default().or(&low[cpu]);
      let expected = if cpu.number == 2 {
        "powersave"
      } else {
        "performance"
      };
      assert_eq!(delta.governor.as_deref(), Some(expected));
      assert_eq!(
        delta.energy_perf_bias.as_deref(),
        (cpu.number == 1).then_some("performance")
      );
    }
  });
}
