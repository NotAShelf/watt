mod provenance;
use std::{
  collections::{
    HashMap,
    HashSet,
  },
  hash::Hash,
  mem,
  sync::Arc,
};

use anyhow::Context as _;
use provenance::Attribution;

use super::{
  CpuDeltas,
  DiskDeltas,
  EvalState,
  GpuDeltas,
  PowerSupplyDeltas,
  Rule,
  UncoreDeltas,
  UsbDeltas,
};
use crate::{
  audio,
  cpu,
  disk,
  gpu,
  power_supply,
  uncore,
  usb,
  vm,
};

/// Actions from one matching rule, available only after complete evaluation.
#[derive(Default)]
pub struct EvaluatedRule {
  pub cpu:                CpuDeltas,
  pub cpu_global:         cpu::GlobalDelta,
  pub uncore:             UncoreDeltas,
  pub vm:                 vm::Delta,
  pub disk:               DiskDeltas,
  pub disk_global:        disk::GlobalDelta,
  pub usb:                UsbDeltas,
  pub audio:              audio::Delta,
  pub gpu:                GpuDeltas,
  pub power:              PowerSupplyDeltas,
  pub platform_profile:   Option<String>,
  pub(crate) attribution: Attribution,
}

impl EvaluatedRule {
  pub(crate) fn empty(state: &EvalState<'_, '_>) -> Self {
    Self {
      cpu: empty_deltas(state.cpus),
      uncore: empty_deltas(state.uncores),
      disk: empty_deltas(state.disks),
      usb: empty_deltas(state.usb_devices),
      gpu: empty_deltas(state.gpus),
      power: empty_deltas(state.power_supplies),
      ..Self::default()
    }
  }

  pub(crate) fn merge(&mut self, lower: Self) {
    self.attribution.merge(lower.attribution);
    merge_deltas(&mut self.cpu, lower.cpu, cpu::Delta::or);
    merge_deltas(&mut self.uncore, lower.uncore, uncore::Delta::or);
    merge_deltas(&mut self.disk, lower.disk, disk::Delta::or);
    merge_deltas(&mut self.usb, lower.usb, usb::Delta::or);
    merge_deltas(&mut self.gpu, lower.gpu, gpu::Delta::or);
    merge_deltas(&mut self.power, lower.power, power_supply::Delta::or);
    self.cpu_global = mem::take(&mut self.cpu_global).or(&lower.cpu_global);
    self.vm = mem::take(&mut self.vm).or(&lower.vm);
    self.disk_global = mem::take(&mut self.disk_global).or(&lower.disk_global);
    self.audio = mem::take(&mut self.audio).or(&lower.audio);
    self.platform_profile =
      self.platform_profile.take().or(lower.platform_profile);
  }

  pub(crate) fn complete(&self) -> bool {
    self.cpu.values().all(cpu::Delta::is_some)
      && self.cpu_global.is_some()
      && self.power.values().all(power_supply::Delta::is_some)
      && self.platform_profile.is_some()
      && self.uncore.values().all(uncore::Delta::is_some)
      && self.vm.is_some()
      && self.disk.values().all(disk::Delta::is_some)
      && self.disk_global.is_some()
      && self.usb.values().all(usb::Delta::is_some)
      && self.audio.is_some()
      && self.gpu.values().all(gpu::Delta::is_some)
  }
}

fn empty_deltas<K: Eq + Hash, V: Default>(
  devices: &HashSet<Arc<K>>,
) -> HashMap<Arc<K>, V> {
  devices
    .iter()
    .map(|device| (Arc::clone(device), V::default()))
    .collect()
}

fn merge_deltas<K: Eq + Hash, V: Default>(
  current: &mut HashMap<K, V>,
  lower: HashMap<K, V>,
  merge: impl Fn(V, &V) -> V,
) {
  for (key, lower) in lower {
    if let Some(value) = current.get_mut(&key) {
      *value = merge(mem::take(value), &lower);
    }
  }
}

impl Rule {
  /// Evaluates a matching rule without applying or merging any actions.
  pub fn evaluate(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Option<EvaluatedRule>> {
    let Some(condition) = self.condition.eval(state)? else {
      return Ok(None);
    };
    if !condition
      .try_into_boolean()
      .context("`if` was not a boolean")?
    {
      return Ok(None);
    }

    let (cpu, cpu_global) = self.cpu.eval(state).context("CPU actions")?;
    let uncore = self.uncore.eval(state).context("uncore actions")?;
    let vm = self.vm.eval(state).context("VM actions")?;
    let (disk, disk_global) = self.disk.eval(state).context("disk actions")?;
    let usb = self.usb.eval(state).context("USB actions")?;
    let audio = self.audio.eval(state).context("audio actions")?;
    let gpu = self.gpu.eval(state).context("GPU actions")?;
    let (power, platform_profile) =
      self.power.eval(state).context("power actions")?;
    let mut actions = EvaluatedRule {
      cpu,
      cpu_global,
      uncore,
      vm,
      disk,
      disk_global,
      usb,
      audio,
      gpu,
      power,
      platform_profile,
      attribution: Attribution::default(),
    };
    actions.attribution = Attribution::capture(&actions, self);
    Ok(Some(actions))
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use super::*;
  use crate::config::test_support::with_cpu_state;

  #[test]
  fn failed_late_action_returns_no_partial_rule() {
    with_cpu_state(&HashSet::new(), |state| {
      let mut rule: Rule = toml::from_str(
        "name='test'\npriority=1\ncpu.turbo=true\naudio.timeout-seconds='\
         invalid'",
      )
      .unwrap();
      let error = rule.evaluate(state).err().expect("invalid audio action");
      assert!(format!("{error:#}").contains("audio"));
      rule.audio = Default::default();
      let result = rule.evaluate(state).unwrap().unwrap();
      assert_eq!(result.cpu_global.turbo, Some(true));
    });
  }

  #[test]
  fn skipped_rule_does_not_evaluate_invalid_actions() {
    with_cpu_state(&HashSet::new(), |state| {
      for condition in ["false", "'$cpu-temperature'"] {
        let rule: Rule = toml::from_str(&format!(
          "name='test'\npriority=1\nif={condition}\naudio.timeout-seconds='\
           invalid'",
        ))
        .unwrap();
        assert!(rule.evaluate(state).unwrap().is_none());
      }
    });
  }
}
