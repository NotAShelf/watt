use std::{
  collections::{
    HashSet,
    VecDeque,
  },
  sync::Arc,
};

use super::*;

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
  let power_supplies = HashSet::new();
  let uncores = HashSet::new();
  let disks = HashSet::new();
  let usb_devices = HashSet::new();
  let gpus = HashSet::new();
  let cpu_log = VecDeque::new();
  let state = EvalState {
    frequency_available:         true,
    turbo_available:             false,
    cpu_usage:                   0.0,
    cpu_usage_volatility:        None,
    cpu_temperature:             None,
    cpu_temperature_volatility:  None,
    cpu_idle_seconds:            0.0,
    cpu_frequency_maximum:       None,
    cpu_frequency_minimum:       None,
    lid_closed:                  false,
    virtual_machine:             false,
    chassis_type:                None,
    power_supply_charge:         None,
    power_supply_discharge_rate: None,
    battery_cycles:              None,
    battery_health:              None,
    discharging:                 false,
    power_profile_preference:    crate::profile::PowerProfile::Balanced,
    context:                     EvalContext::WidestPossible,
    cpus:                        &cpus,
    uncores:                     &uncores,
    disks:                       &disks,
    usb_devices:                 &usb_devices,
    gpus:                        &gpus,
    power_supplies:              &power_supplies,
    cpu_log:                     &cpu_log,
  };
  let config = DaemonConfig::load_from(None).expect("load default config");
  let rule = config
    .rules
    .iter()
    .find(|rule| rule.name == "battery-balanced")
    .expect("default battery-balanced rule");

  let (deltas, _) = rule.cpu.eval(&state).expect("evaluate CPU deltas");

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
}
