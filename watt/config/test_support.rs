use std::{
  collections::{
    HashSet,
    VecDeque,
  },
  sync::Arc,
};

use super::{
  EvalContext,
  EvalState,
};
use crate::{
  cpu,
  profile::PowerProfile,
};

pub(super) fn with_cpu_state(
  cpus: &HashSet<Arc<cpu::Cpu>>,
  test: impl FnOnce(&EvalState<'_, '_>),
) {
  let power_supplies = HashSet::new();
  let uncores = HashSet::new();
  let disks = HashSet::new();
  let usb_devices = HashSet::new();
  let gpus = HashSet::new();
  let cpu_log = VecDeque::new();
  let state = EvalState {
    frequency_available: true,
    turbo_available: false,
    cpu_usage: 0.0,
    cpu_usage_volatility: None,
    cpu_temperature: None,
    cpu_temperature_volatility: None,
    cpu_idle_seconds: 0.0,
    cpu_frequency_maximum: None,
    cpu_frequency_minimum: None,
    lid_closed: false,
    virtual_machine: false,
    chassis_type: None,
    power_supply_charge: None,
    power_supply_discharge_rate: None,
    battery_cycles: None,
    battery_health: None,
    discharging: false,
    power_profile_preference: PowerProfile::Balanced,
    context: EvalContext::WidestPossible,
    cpus,
    uncores: &uncores,
    disks: &disks,
    usb_devices: &usb_devices,
    gpus: &gpus,
    power_supplies: &power_supplies,
    cpu_log: &cpu_log,
  };
  test(&state);
}
