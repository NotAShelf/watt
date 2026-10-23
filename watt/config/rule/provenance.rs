mod global;
mod shared;

use std::{
  collections::{
    BTreeMap,
    HashSet,
  },
  path::PathBuf,
};

use super::EvaluatedRule;
use crate::{
  config::Rule,
  cpu::Cpu,
  fs::RuleOrigin,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
  Cpu(u32),
  Device(PathBuf, String),
  Global,
}

/// Captured at successful rule evaluation, then merged alongside the actions.
#[derive(Default)]
pub(crate) struct Attribution(BTreeMap<(Scope, &'static str), RuleOrigin>);

impl Attribution {
  fn record(
    &mut self,
    scope: Scope,
    fields: &[(&'static str, bool)],
    rule: &Rule,
  ) {
    for &(field, present) in fields {
      if present {
        self.0.insert((scope.clone(), field), RuleOrigin {
          name:     rule.name.clone(),
          priority: rule.priority,
        });
      }
    }
  }

  pub(super) fn capture(actions: &EvaluatedRule, rule: &Rule) -> Self {
    let mut result = Self::default();
    result.cpus(actions, rule);
    result.devices(actions, rule);
    result.globals(actions, rule);
    result
  }

  fn cpus(&mut self, actions: &EvaluatedRule, rule: &Rule) {
    for (cpu, delta) in &actions.cpu {
      self.record(
        Scope::Cpu(cpu.number),
        &[
          ("scaling_governor", delta.governor.is_some()),
          (
            "energy_performance_preference",
            delta.energy_performance_preference.is_some(),
          ),
          ("energy_perf_bias", delta.energy_perf_bias.is_some()),
          ("scaling_min_freq", delta.frequency_minimum.is_some()),
          ("scaling_max_freq", delta.frequency_maximum.is_some()),
          (
            "pm_qos_resume_latency_us",
            delta.pm_qos_resume_latency_us.is_some(),
          ),
        ],
        rule,
      );
    }
  }

  fn devices(&mut self, actions: &EvaluatedRule, rule: &Rule) {
    for (device, delta) in &actions.disk {
      self.record(
        Scope::Device(device.path.clone(), device.name.clone()),
        &[
          ("queue/scheduler", delta.scheduler.is_some()),
          ("queue/read_ahead_kb", delta.readahead_kib.is_some()),
        ],
        rule,
      );
    }
    for (device, delta) in &actions.usb {
      self.record(
        Scope::Device(device.path.clone(), device.name.clone()),
        &[
          ("power/control", delta.autosuspend.is_some()),
          (
            "power/autosuspend_delay_ms",
            delta.autosuspend_delay_ms.is_some(),
          ),
        ],
        rule,
      );
    }
    for (device, delta) in &actions.uncore {
      self.record(
        Scope::Device(device.path.clone(), device.name.clone()),
        &[
          ("min_freq_khz", delta.frequency_khz_minimum.is_some()),
          ("max_freq_khz", delta.frequency_khz_maximum.is_some()),
        ],
        rule,
      );
    }
    for (device, delta) in &actions.power {
      if let Some(config) = device.threshold_config {
        self.record(
          Scope::Device(device.path.clone(), device.name.clone()),
          &[
            (config.path_start, delta.charge_threshold_start.is_some()),
            (config.path_end, delta.charge_threshold_end.is_some()),
          ],
          rule,
        );
      }
    }
    for (device, delta) in &actions.gpu {
      self.record(
        Scope::Device(device.path.clone(), device.name.clone()),
        &[(
          "amdgpu/panel_power_savings",
          delta.panel_power_savings.is_some(),
        )],
        rule,
      );
      for root in device.control_roots().skip(1) {
        self.record(
          Scope::Device(root.to_owned(), device.name.clone()),
          &[
            ("power_method", delta.radeon_powersave.is_some()),
            ("power_profile", delta.radeon_powersave.is_some()),
            ("power_dpm_state", delta.radeon_powersave.is_some()),
          ],
          rule,
        );
      }
    }
  }

  fn globals(&mut self, actions: &EvaluatedRule, rule: &Rule) {
    self.record(
      Scope::Global,
      &[
        ("turbo", actions.cpu_global.turbo.is_some()),
        (
          "min_perf_pct",
          actions.cpu_global.pstate_min_performance_percent.is_some(),
        ),
        (
          "max_perf_pct",
          actions.cpu_global.pstate_max_performance_percent.is_some(),
        ),
        (
          "link_power_management_policy",
          actions.disk_global.alpm.is_some(),
        ),
        ("power_save", actions.audio.timeout_seconds.is_some()),
        (
          "power_save_controller",
          actions.audio.reset_controller.is_some(),
        ),
        ("platform_profile", actions.platform_profile.is_some()),
      ],
      rule,
    );
    self.record(
      Scope::Global,
      &[
        ("dirty_bytes", actions.vm.dirty_bytes.is_some()),
        ("dirty_ratio", actions.vm.dirty_ratio.is_some()),
        (
          "dirty_background_bytes",
          actions.vm.dirty_background_bytes.is_some(),
        ),
        (
          "dirty_background_ratio",
          actions.vm.dirty_background_ratio.is_some(),
        ),
        ("enabled", actions.vm.transparent_hugepages.is_some()),
        ("defrag", actions.vm.transparent_hugepage_defrag.is_some()),
      ],
      rule,
    );
  }

  pub(super) fn merge(&mut self, lower: Self) {
    for (key, origin) in lower.0 {
      self.0.entry(key).or_insert(origin);
    }
  }

  pub(crate) fn resolve(
    self,
    actions: &mut EvaluatedRule,
  ) -> ResolvedAttribution {
    let mut resolved = ResolvedAttribution::default();
    let mut cpus: Vec<_> = actions.cpu.keys().collect();
    cpus.sort_by_key(|cpu| cpu.number);
    let mut policies = HashSet::new();
    for cpu in cpus {
      let first = cpu
        .policy
        .as_ref()
        .is_none_or(|policy| policies.insert(&policy.path));
      self.resolve_cpu(cpu, first, &mut resolved.rules);
    }
    let winners = shared::winners(&self.0);
    shared::normalize(actions, &winners);
    resolved.rules.extend(
      winners
        .into_iter()
        .map(|(path, (_, origin))| (path, origin)),
    );
    self.resolve_globals(actions, &mut resolved);
    resolved
  }

  fn resolve_globals(
    &self,
    actions: &mut EvaluatedRule,
    resolved: &mut ResolvedAttribution,
  ) {
    for ((scope, field), origin) in &self.0 {
      if *scope != Scope::Global {
        continue;
      }
      match global::paths(field, actions) {
        Ok(paths) => {
          for path in paths {
            resolved.rules.insert(path, origin.clone());
          }
        },
        Err(error) => {
          actions.disk_global.alpm = None;
          resolved.failures.push(error);
        },
      }
    }
  }

  fn resolve_cpu(
    &self,
    cpu: &Cpu,
    first: bool,
    result: &mut BTreeMap<PathBuf, RuleOrigin>,
  ) {
    let root =
      PathBuf::from(format!("/sys/devices/system/cpu/cpu{}", cpu.number));
    for ((_, field), origin) in self
      .0
      .range((Scope::Cpu(cpu.number), "")..(Scope::Cpu(cpu.number + 1), ""))
    {
      let path = match *field {
        "energy_perf_bias" => root.join("power/energy_perf_bias"),
        "pm_qos_resume_latency_us" => {
          root.join("power/pm_qos_resume_latency_us")
        },
        _ if first => {
          cpu
            .policy
            .as_ref()
            .map_or_else(|| root.join("cpufreq"), |policy| policy.path.clone())
            .join(field)
        },
        _ => continue,
      };
      result.insert(path, origin.clone());
    }
  }
}

#[derive(Default)]
pub(crate) struct ResolvedAttribution {
  pub rules:    BTreeMap<PathBuf, RuleOrigin>,
  pub failures: Vec<anyhow::Error>,
}
