use std::{
  collections::HashSet,
  sync::Arc,
};

use super::{
  test_support::with_cpu_state,
  *,
};

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
