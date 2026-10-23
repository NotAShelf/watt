use std::{
  collections::{
    BTreeMap,
    HashSet,
  },
  path::{
    Path,
    PathBuf,
  },
};

use super::{
  RuntimeFailures,
  System,
};
use crate::{
  fs,
  power_supply,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCapabilities {
  pub subsystem:        String,
  pub name:             String,
  pub path:             PathBuf,
  pub controls:         Vec<String>,
  pub available_values: BTreeMap<String, Vec<String>>,
}

fn device(
  subsystem: &str,
  name: &str,
  path: &Path,
  controls: &[(&str, &str)],
) -> DeviceCapabilities {
  let mut controls: Vec<_> = controls
    .iter()
    .filter(|(_, file)| fs::exists(path.join(file)))
    .map(|(name, _)| (*name).to_owned())
    .collect();
  controls.sort();
  DeviceCapabilities {
    subsystem: subsystem.to_owned(),
    name: name.to_owned(),
    path: fs::path(path),
    controls,
    available_values: BTreeMap::new(),
  }
}

pub(super) fn snapshot(
  system: &System,
  failures: &mut RuntimeFailures,
) -> Vec<DeviceCapabilities> {
  let mut result = Vec::new();
  cpus(system, &mut result);
  disks(system, &mut result, failures);
  devices(system, &mut result);
  globals(system, &mut result, failures);
  result.retain(|device| !device.controls.is_empty());
  for device in &mut result {
    for values in device.available_values.values_mut() {
      values.sort();
      values.dedup();
    }
    device
      .available_values
      .retain(|_, values| !values.is_empty());
  }
  result.sort_by(|left, right| {
    (&left.subsystem, &left.name, &left.path).cmp(&(
      &right.subsystem,
      &right.name,
      &right.path,
    ))
  });
  result
}

fn cpus(system: &System, result: &mut Vec<DeviceCapabilities>) {
  let mut policies = HashSet::new();
  for cpu in &system.cpus {
    let name = format!("cpu{}", cpu.number);
    let root = PathBuf::from(format!("/sys/devices/system/cpu/{name}"));
    let mut capability = device("cpu", &name, &root, &[
      ("epb", "power/energy_perf_bias"),
      ("pm-qos-resume-latency-us", "power/pm_qos_resume_latency_us"),
    ]);
    capability
      .available_values
      .insert("epb".to_owned(), cpu.available_epbs.clone());
    result.push(capability);
    if let Some(policy) = &cpu.policy
      && policies.insert(&policy.path)
    {
      let mut capability = device("cpu-policy", &policy.name, &policy.path, &[
        ("governor", "scaling_governor"),
        ("epp", "energy_performance_preference"),
        ("frequency-minimum", "scaling_min_freq"),
        ("frequency-maximum", "scaling_max_freq"),
      ]);
      capability
        .available_values
        .insert("governor".to_owned(), cpu.available_governors.clone());
      capability
        .available_values
        .insert("epp".to_owned(), cpu.available_epps.clone());
      result.push(capability);
    }
  }
}

fn devices(system: &System, result: &mut Vec<DeviceCapabilities>) {
  for usb in &system.usb_devices {
    let mut capability = device("usb", &usb.name, &usb.path, &[
      ("autosuspend", "power/control"),
      ("autosuspend-delay-ms", "power/autosuspend_delay_ms"),
    ]);
    capability
      .available_values
      .insert("autosuspend".to_owned(), vec![
        "false".to_owned(),
        "true".to_owned(),
      ]);
    result.push(capability);
  }
  for uncore in &system.uncores {
    result.push(device("uncore", &uncore.name, &uncore.path, &[
      ("frequency-khz-minimum", "min_freq_khz"),
      ("frequency-khz-maximum", "max_freq_khz"),
    ]));
  }
  for power in &system.power_supplies {
    if let Some(config) = power.threshold_config {
      result.push(device("power-supply", &power.name, &power.path, &[
        ("charge-threshold-start", config.path_start),
        ("charge-threshold-end", config.path_end),
      ]));
    }
  }
  for gpu in &system.gpus {
    result.push(device("gpu", &gpu.name, &gpu.path, &[(
      "panel-power-savings",
      "amdgpu/panel_power_savings",
    )]));
    for root in gpu.control_roots().skip(1) {
      result.push(device("gpu", &gpu.name, root, &[(
        "radeon-powersave",
        "power_method",
      )]));
    }
  }
}

fn disks(
  system: &System,
  result: &mut Vec<DeviceCapabilities>,
  failures: &mut RuntimeFailures,
) {
  for disk in &system.disks {
    let mut capability = device("disk", &disk.name, &disk.path, &[
      ("scheduler", "queue/scheduler"),
      ("readahead-kib", "queue/read_ahead_kb"),
    ]);
    if let Some(Some(values)) = failures.attempt(
      format!("disk scheduler capability for {}", disk.name),
      || fs::read(disk.path.join("queue/scheduler")),
    ) {
      capability.available_values.insert(
        "scheduler".to_owned(),
        values
          .split_whitespace()
          .map(|value| value.trim_matches(['[', ']']).to_owned())
          .collect(),
      );
    }
    result.push(capability);
  }
}

fn globals(
  system: &System,
  result: &mut Vec<DeviceCapabilities>,
  failures: &mut RuntimeFailures,
) {
  let pstate = Path::new("/sys/devices/system/cpu/intel_pstate");
  result.push(device("cpu-global", "intel_pstate", pstate, &[
    ("pstate-min-performance-percent", "min_perf_pct"),
    ("pstate-max-performance-percent", "max_perf_pct"),
  ]));
  if let Some(boost) = &system.boost_capabilities {
    for path in &boost.paths {
      result.push(DeviceCapabilities {
        subsystem:        "cpu-global".to_owned(),
        name:             "boost".to_owned(),
        path:             path.clone(),
        controls:         vec!["turbo".to_owned()],
        available_values: BTreeMap::new(),
      });
    }
  }
  let path = Path::new("/sys/firmware/acpi");
  let mut capability = device("platform", "acpi", path, &[(
    "platform-profile",
    "platform_profile",
  )]);
  if let Some(profiles) = failures.attempt(
    "platform profile capability scan",
    power_supply::PowerSupply::get_available_platform_profiles,
  ) {
    capability
      .available_values
      .insert("platform-profile".to_owned(), profiles);
  }
  result.push(capability);
  result.push(device("vm", "vm", Path::new("/proc/sys/vm"), &[
    ("dirty-bytes", "dirty_bytes"),
    ("dirty-ratio", "dirty_ratio"),
    ("dirty-background-bytes", "dirty_background_bytes"),
    ("dirty-background-ratio", "dirty_background_ratio"),
  ]));
  for name in ["snd_hda_intel", "snd_ac97_codec"] {
    let path = PathBuf::from(format!("/sys/module/{name}/parameters"));
    result.push(device("audio", name, &path, &[
      ("timeout-seconds", "power_save"),
      ("reset-controller", "power_save_controller"),
    ]));
  }
  for name in ["transparent_hugepage", "redhat_transparent_hugepage"] {
    let path = PathBuf::from(format!("/sys/kernel/mm/{name}"));
    result.push(device("vm", name, &path, &[
      ("transparent-hugepages", "enabled"),
      ("transparent-hugepage-defrag", "defrag"),
    ]));
  }
  result.push(device(
    "cpu-global",
    "cpu_dma_latency",
    Path::new("/dev/cpu_dma_latency"),
    &[("dma-latency-us", "")],
  ));
  if let Some(hosts) = failures.attempt("ALPM capability scan", alpm_hosts) {
    result.extend(hosts);
  }
}

fn alpm_hosts() -> anyhow::Result<Vec<DeviceCapabilities>> {
  let Some(entries) = fs::read_dir("/sys/class/scsi_host")? else {
    return Ok(Vec::new());
  };
  let mut hosts = Vec::new();
  for entry in entries {
    let entry = entry?;
    if let Some(path) = fs::canonicalize(entry.path())? {
      let name = entry.file_name().to_string_lossy().into_owned();
      hosts.push(device("disk-global", &name, &path, &[(
        "alpm",
        "link_power_management_policy",
      )]));
    }
  }
  Ok(hosts)
}
