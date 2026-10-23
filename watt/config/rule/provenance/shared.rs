use std::{
  collections::BTreeMap,
  mem,
  path::{
    Path,
    PathBuf,
  },
};

use super::{
  RuleOrigin,
  Scope,
};
use crate::{
  config::EvaluatedRule,
  uncore,
};

type Winners = BTreeMap<PathBuf, (String, RuleOrigin)>;

pub(super) fn winners(
  entries: &BTreeMap<(Scope, &'static str), RuleOrigin>,
) -> Winners {
  let mut winners = Winners::new();
  for ((scope, field), origin) in entries {
    if let Scope::Device(root, name) = scope {
      let winner = winners
        .entry(root.join(field))
        .or_insert_with(|| (name.clone(), origin.clone()));
      if origin.priority > winner.1.priority
        || (origin.priority == winner.1.priority && name < &winner.0)
      {
        *winner = (name.clone(), origin.clone());
      }
    }
  }
  winners
}

fn retain<T>(
  winners: &Winners,
  root: &Path,
  field: &str,
  name: &str,
  value: &mut Option<T>,
) {
  if winners
    .get(&root.join(field))
    .is_some_and(|(winner, _)| winner != name)
  {
    *value = None;
  }
}

pub(super) fn normalize(actions: &mut EvaluatedRule, winners: &Winners) {
  for (device, delta) in &mut actions.disk {
    retain(
      winners,
      &device.path,
      "queue/scheduler",
      &device.name,
      &mut delta.scheduler,
    );
    retain(
      winners,
      &device.path,
      "queue/read_ahead_kb",
      &device.name,
      &mut delta.readahead_kib,
    );
  }
  for (device, delta) in &mut actions.usb {
    retain(
      winners,
      &device.path,
      "power/control",
      &device.name,
      &mut delta.autosuspend,
    );
    retain(
      winners,
      &device.path,
      "power/autosuspend_delay_ms",
      &device.name,
      &mut delta.autosuspend_delay_ms,
    );
  }
  for (device, delta) in &mut actions.uncore {
    retain(
      winners,
      &device.path,
      "min_freq_khz",
      &device.name,
      &mut delta.frequency_khz_minimum,
    );
    retain(
      winners,
      &device.path,
      "max_freq_khz",
      &device.name,
      &mut delta.frequency_khz_maximum,
    );
  }
  combine_uncore_aliases(actions);
  for (device, delta) in &mut actions.power {
    if let Some(config) = device.threshold_config {
      retain(
        winners,
        &device.path,
        config.path_start,
        &device.name,
        &mut delta.charge_threshold_start,
      );
      retain(
        winners,
        &device.path,
        config.path_end,
        &device.name,
        &mut delta.charge_threshold_end,
      );
    }
  }
  for (device, delta) in &mut actions.gpu {
    retain(
      winners,
      &device.path,
      "amdgpu/panel_power_savings",
      &device.name,
      &mut delta.panel_power_savings,
    );
    if let Some(root) = device.radeon_control_root() {
      retain(
        winners,
        root,
        "power_method",
        &device.name,
        &mut delta.radeon_powersave,
      );
    }
  }
}

fn combine_uncore_aliases(actions: &mut EvaluatedRule) {
  let mut aliases: Vec<_> = actions.uncore.drain().collect();
  aliases.sort_by(|(left, _), (right, _)| left.name.cmp(&right.name));
  let mut combined = BTreeMap::new();
  for (device, delta) in aliases {
    let (_, selected) = combined
      .entry(device.path.clone())
      .or_insert_with(|| (device, uncore::Delta::default()));
    *selected = mem::take(selected).or(&delta);
  }
  actions.uncore = combined.into_values().collect();
}
