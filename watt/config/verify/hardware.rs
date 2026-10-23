use std::{
  collections::{
    BTreeMap,
    HashSet,
  },
  fs as stdfs,
  io::ErrorKind,
  path::{
    Path,
    PathBuf,
  },
};

use anyhow::{
  Context as _,
  ensure,
};

use super::{
  DaemonConfig,
  Diagnostic,
  Expression,
  Rule,
  constant_frequency,
  constant_percentage,
  constant_unsigned_integer,
  constants,
};
use crate::{
  config::CpusDelta,
  cpu::{
    Cpu,
    CpuInventory,
  },
  disk::{
    Disk,
    DiskInventory,
  },
  fs,
  gpu::{
    Gpu,
    GpuInventory,
  },
  power_supply::{
    PowerSupply,
    PowerSupplyInventory,
  },
  uncore::{
    Uncore,
    UncoreInventory,
  },
  usb::{
    UsbDevice,
    UsbInventory,
  },
};

impl DaemonConfig {
  /// Checks device selectors, control availability, and constant action values
  /// against this host. Returns an error if hardware discovery is incomplete.
  pub fn verify_hardware(&self) -> anyhow::Result<Vec<Diagnostic>> {
    let inventory =
      CpuInventory::scan().context("CPU hardware discovery failed")?;
    ensure!(
      !inventory.cpus.is_empty(),
      "CPU hardware discovery found no CPUs"
    );
    ensure_complete("CPU", &inventory.failures)?;
    let power = PowerSupplyInventory::scan()
      .context("power-supply hardware discovery failed")?;
    ensure_complete("power-supply", &power.failures)?;
    Ok(
      self
        .verify_cpu_hardware(&inventory.cpus)
        .into_iter()
        .chain(self.verify_platform_hardware())
        .chain(self.verify_disk_hardware()?)
        .chain(self.verify_usb_hardware()?)
        .chain(self.verify_gpu_hardware()?)
        .chain(self.verify_uncore_hardware()?)
        .chain(self.verify_audio_hardware())
        .chain(self.verify_vm_hardware())
        .chain(self.verify_power_hardware(&power.power_supplies))
        .collect(),
    )
  }

  fn enabled_rules(&self) -> impl Iterator<Item = &Rule> {
    self
      .rules
      .iter()
      .filter(|rule| constants::boolean(&rule.condition) != Some(false))
  }

  fn verify_power_hardware(&self, supplies: &[PowerSupply]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      let Some(selected) = selected_power_supply_names(rule, supplies) else {
        continue;
      };
      for name in selected {
        match supplies.iter().find(|supply| supply.name == name) {
          Some(supply) => {
            verify_charge_threshold_controls(rule, supply, &mut diagnostics)
          },
          None => {
            diagnostics.push(Diagnostic::warning(
              rule,
              "power.for",
              format!(
                "power supply {name:?} was not discovered; its controls are \
                 unknown",
              ),
            ))
          },
        }
      }
    }
    diagnostics
  }

  fn verify_platform_hardware(&self) -> Vec<Diagnostic> {
    let mut rules = self
      .enabled_rules()
      .filter(|rule| active_action(&rule.power.platform_profile).is_some())
      .peekable();
    if rules.peek().is_none() {
      return Vec::new();
    }
    let current = fs::read("/sys/firmware/acpi/platform_profile");
    let available = PowerSupply::get_available_platform_profiles();
    let mut diagnostics = Vec::new();
    for rule in rules {
      let message = rule
        .power
        .platform_profile
        .as_ref()
        .and_then(constants::active_expression)
        .and_then(|expression| {
          match expression {
            Expression::FirstAvailablePlatformProfile { values } => {
              platform_alternatives_warning(&current, &available, values)
            },
            _ => {
              match constants::analyze(expression).ok()? {
                constants::Constant::String(value) => {
                  platform_profile_warning(&current, &available, value)
                },
                _ => None,
              }
            },
          }
        });
      if let Some(message) = message {
        diagnostics.push(Diagnostic::warning(
          rule,
          "power.platform-profile",
          message,
        ));
      }
    }
    diagnostics
  }

  fn verify_disk_hardware(&self) -> anyhow::Result<Vec<Diagnostic>> {
    let inventory =
      DiskInventory::scan().context("disk hardware discovery failed")?;
    ensure_complete("disk", &inventory.failures)?;
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      verify_alpm(rule, &mut diagnostics);
      for disk in selected_devices(
        rule,
        "disk.for",
        rule.disk.for_.as_ref(),
        &inventory.devices,
        |disk| &disk.name,
        &mut diagnostics,
      )
      .into_iter()
      .flatten()
      {
        verify_disk_controls(rule, disk, &mut diagnostics);
      }
    }
    Ok(diagnostics)
  }

  fn verify_usb_hardware(&self) -> anyhow::Result<Vec<Diagnostic>> {
    let inventory =
      UsbInventory::scan().context("USB hardware discovery failed")?;
    ensure_complete("USB", &inventory.failures)?;
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      for device in selected_devices(
        rule,
        "usb.for",
        rule.usb.for_.as_ref(),
        &inventory.devices,
        |device| &device.name,
        &mut diagnostics,
      )
      .into_iter()
      .flatten()
      {
        verify_usb_controls(rule, device, &mut diagnostics);
      }
    }
    Ok(diagnostics)
  }

  fn verify_gpu_hardware(&self) -> anyhow::Result<Vec<Diagnostic>> {
    let inventory =
      GpuInventory::scan().context("GPU hardware discovery failed")?;
    ensure_complete("GPU", &inventory.failures)?;
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      for gpu in selected_devices(
        rule,
        "gpu.for",
        rule.gpu.for_.as_ref(),
        &inventory.devices,
        |gpu| &gpu.name,
        &mut diagnostics,
      )
      .into_iter()
      .flatten()
      {
        verify_gpu_controls(rule, gpu, &mut diagnostics);
      }
    }
    Ok(diagnostics)
  }

  fn verify_uncore_hardware(&self) -> anyhow::Result<Vec<Diagnostic>> {
    let inventory =
      UncoreInventory::scan().context("uncore hardware discovery failed")?;
    ensure_complete("uncore", &inventory.failures)?;
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      let Some(selected) = selected_devices(
        rule,
        "uncore.for",
        rule.uncore.for_.as_ref(),
        &inventory.devices,
        |device| &device.name,
        &mut diagnostics,
      ) else {
        continue;
      };
      if inventory.devices.is_empty()
        && (active_action(&rule.uncore.frequency_khz_minimum).is_some()
          || active_action(&rule.uncore.frequency_khz_maximum).is_some())
      {
        diagnostics.push(Diagnostic::warning(
          rule,
          "uncore.frequency-khz",
          "uncore frequency controls are unavailable on this host",
        ));
      }
      for device in selected {
        verify_uncore_controls(rule, device, &mut diagnostics);
      }
    }
    Ok(diagnostics)
  }

  fn verify_audio_hardware(&self) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      for (field, parameter, expression) in [
        (
          "audio.timeout-seconds",
          "power_save",
          &rule.audio.timeout_seconds,
        ),
        (
          "audio.reset-controller",
          "power_save_controller",
          &rule.audio.reset_controller,
        ),
      ] {
        if active_action(expression).is_some() {
          verify_audio_control(rule, field, parameter, &mut diagnostics);
        }
      }
    }
    diagnostics
  }

  fn verify_vm_hardware(&self) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      for (field, expression, control) in [
        ("vm.dirty-bytes", &rule.vm.dirty_bytes, "dirty_bytes"),
        (
          "vm.dirty-background-bytes",
          &rule.vm.dirty_background_bytes,
          "dirty_background_bytes",
        ),
        ("vm.dirty-ratio", &rule.vm.dirty_ratio, "dirty_ratio"),
        (
          "vm.dirty-background-ratio",
          &rule.vm.dirty_background_ratio,
          "dirty_background_ratio",
        ),
      ] {
        verify_control(
          rule,
          field,
          "VM",
          format!("/proc/sys/vm/{control}"),
          expression,
          &mut diagnostics,
        );
      }
      verify_hugepage_controls(rule, &mut diagnostics);
    }
    diagnostics
  }

  /// Checks constant CPU selectors and actions against discovered controls and
  /// hardware bounds. Telemetry-dependent conditions and values wait until
  /// runtime.
  fn verify_cpu_hardware(&self, cpus: &[Cpu]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for rule in self.enabled_rules() {
      verify_cpu_global_controls(rule, cpus, &mut diagnostics);
      let Some(selected) = selected_cpu_numbers(rule, cpus) else {
        continue;
      };
      verify_policy_selection(rule, cpus, &selected, &mut diagnostics);
      for number in selected {
        let Some(cpu) = cpus.iter().find(|cpu| cpu.number == number) else {
          diagnostics.push(Diagnostic::warning(
            rule,
            "cpu.for",
            format!(
              "CPU {number} was not discovered; its controls are unknown"
            ),
          ));
          continue;
        };
        verify_cpu_values(rule, cpu, &mut diagnostics);
        verify_cpu_frequencies(rule, cpu, &mut diagnostics);
        verify_control(
          rule,
          "cpu.pm-qos-resume-latency-us",
          &format!("CPU {number}"),
          format!(
            "/sys/devices/system/cpu/cpu{number}/power/\
             pm_qos_resume_latency_us"
          ),
          &rule.cpu.pm_qos_resume_latency_us,
          &mut diagnostics,
        );
      }
    }
    diagnostics
  }
}

fn verify_audio_control(
  rule: &Rule,
  field: &str,
  parameter: &str,
  diagnostics: &mut Vec<Diagnostic>,
) {
  let mut found = false;
  for module in ["snd_hda_intel", "snd_ac97_codec"] {
    let path = format!("/sys/module/{module}/parameters/{parameter}");
    match fs::read(&path) {
      Ok(None) => {},
      Ok(Some(_)) => found = true,
      Err(error) => {
        found = true;
        diagnostics.push(Diagnostic::warning(
          rule,
          field,
          format!("audio control is unknown at {path}: {error:#}"),
        ));
      },
    }
  }
  if !found {
    diagnostics.push(Diagnostic::warning(
      rule,
      field,
      "audio control is unavailable on this host",
    ));
  }
}

fn verify_hugepage_controls(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  if active_action(&rule.vm.transparent_hugepages).is_none()
    && active_action(&rule.vm.transparent_hugepage_defrag).is_none()
  {
    return;
  }
  let root = if fs::exists("/sys/kernel/mm/transparent_hugepage") {
    "/sys/kernel/mm/transparent_hugepage"
  } else {
    "/sys/kernel/mm/redhat_transparent_hugepage"
  };
  for (field, expression, control) in [
    (
      "vm.transparent-hugepages",
      &rule.vm.transparent_hugepages,
      "enabled",
    ),
    (
      "vm.transparent-hugepage-defrag",
      &rule.vm.transparent_hugepage_defrag,
      "defrag",
    ),
  ] {
    verify_control(
      rule,
      field,
      "VM",
      format!("{root}/{control}"),
      expression,
      diagnostics,
    );
  }
}

fn verify_cpu_global_controls(
  rule: &Rule,
  cpus: &[Cpu],
  diagnostics: &mut Vec<Diagnostic>,
) {
  for (field, expression, control) in [
    (
      "cpu.pstate-min-performance-percent",
      &rule.cpu.pstate_min_performance_percent,
      "min_perf_pct",
    ),
    (
      "cpu.pstate-max-performance-percent",
      &rule.cpu.pstate_max_performance_percent,
      "max_perf_pct",
    ),
  ] {
    verify_control(
      rule,
      field,
      "Intel P-State",
      format!("/sys/devices/system/cpu/intel_pstate/{control}"),
      expression,
      diagnostics,
    );
  }
  if active_action(&rule.cpu.turbo).is_some() {
    let message = match Cpu::turbo(cpus.iter()) {
      Ok(Some(_)) => None,
      Ok(None) => Some("CPU turbo control is unavailable".into()),
      Err(error) => Some(format!("CPU turbo state is unknown: {error:#}")),
    };
    if let Some(message) = message {
      diagnostics.push(Diagnostic::warning(rule, "cpu.turbo", message));
    }
  }
  if active_action(&rule.cpu.dma_latency_us).is_some() {
    let message = match stdfs::metadata(fs::path("/dev/cpu_dma_latency")) {
      Ok(_) => None,
      Err(error) if error.kind() == ErrorKind::NotFound => {
        Some("DMA latency control is unavailable".into())
      },
      Err(error) => {
        Some(format!("DMA latency control support is unknown: {error}"))
      },
    };
    if let Some(message) = message {
      diagnostics.push(Diagnostic::warning(
        rule,
        "cpu.dma-latency-us",
        message,
      ));
    }
  }
}

fn ensure_complete(subsystem: &str, failures: &[String]) -> anyhow::Result<()> {
  ensure!(
    failures.is_empty(),
    "{subsystem} hardware discovery is incomplete: {}",
    failures.join("; ")
  );
  Ok(())
}

/// Returns the devices a constant selector picks, in discovery order, and
/// warns about selected names that were not discovered. Returns `None` when
/// the selector is not constant.
fn selected_devices<'a, D>(
  rule: &Rule,
  field: &str,
  selector: Option<&Expression>,
  devices: &'a [D],
  name: fn(&D) -> &String,
  diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<&'a D>> {
  let available: HashSet<&str> =
    devices.iter().map(|device| name(device).as_str()).collect();
  let selected: HashSet<&str> = match selector {
    None => available.clone(),
    Some(expression) => constants::strings(expression)?.into_iter().collect(),
  };
  for missing in selected.difference(&available) {
    diagnostics.push(Diagnostic::warning(
      rule,
      field,
      format!(
        "device {missing:?} was not discovered; its controls are unknown"
      ),
    ));
  }
  Some(
    devices
      .iter()
      .filter(|device| selected.contains(name(device).as_str()))
      .collect(),
  )
}

fn active_action(expression: &Option<Expression>) -> Option<&Expression> {
  expression.as_ref().filter(|expression| {
    !matches!(
      constants::analyze(expression),
      Ok(constants::Constant::Unset)
    )
  })
}

fn verify_control(
  rule: &Rule,
  field: &str,
  name: &str,
  path: impl AsRef<Path>,
  expression: &Option<Expression>,
  diagnostics: &mut Vec<Diagnostic>,
) {
  if active_action(expression).is_none() {
    return;
  }
  if let Some(message) = control_read_warning(&fs::read(path.as_ref())) {
    diagnostics.push(Diagnostic::warning(
      rule,
      field,
      format!("{name}: {message} at {}", path.as_ref().display()),
    ));
  }
}

fn control_read_warning(
  value: &anyhow::Result<Option<String>>,
) -> Option<String> {
  match value {
    Ok(Some(_)) => None,
    Ok(None) => Some("control is unavailable".into()),
    Err(error) => {
      Some(format!(
        "control support is unknown because it is unreadable: {error:#}"
      ))
    },
  }
}

fn verify_disk_controls(
  rule: &Rule,
  disk: &Disk,
  diagnostics: &mut Vec<Diagnostic>,
) {
  verify_control(
    rule,
    "disk.readahead-kib",
    &disk.name,
    disk.path.join("queue/read_ahead_kb"),
    &rule.disk.readahead_kib,
    diagnostics,
  );
  if let Some(expression) = active_action(&rule.disk.scheduler) {
    verify_scheduler(rule, disk, expression, diagnostics);
  }
  for (field, expression) in [
    ("disk.apm", &rule.disk.apm),
    ("disk.spindown", &rule.disk.spindown),
  ] {
    if active_action(expression).is_none() {
      continue;
    }
    let message = if fs::exists(format!("/dev/{}", disk.name)) {
      "ATA control support is unknown without querying the device; application \
       requires hdparm"
    } else {
      "device node is unavailable"
    };
    diagnostics.push(Diagnostic::warning(
      rule,
      field,
      format!("disk {:?}: {message}", disk.name),
    ));
  }
}

fn verify_scheduler(
  rule: &Rule,
  disk: &Disk,
  expression: &Expression,
  diagnostics: &mut Vec<Diagnostic>,
) {
  let current = fs::read(disk.path.join("queue/scheduler"));
  let message = control_read_warning(&current).or_else(|| {
    let constants::Constant::String(value) =
      constants::analyze(expression).ok()?
    else {
      return None;
    };
    let available = current.as_ref().ok()?.as_ref()?;
    if available.trim().is_empty() {
      return Some("supported schedulers are unknown".into());
    }
    (!available
      .split_whitespace()
      .any(|item| item.trim_matches(['[', ']']) == value))
    .then(|| format!("scheduler {value:?} is unsupported"))
  });
  if let Some(message) = message {
    diagnostics.push(Diagnostic::warning(
      rule,
      "disk.scheduler",
      format!("{}: {message}", disk.name),
    ));
  }
}

fn verify_usb_controls(
  rule: &Rule,
  device: &UsbDevice,
  diagnostics: &mut Vec<Diagnostic>,
) {
  for (field, expression, control) in [
    ("usb.autosuspend", &rule.usb.autosuspend, "power/control"),
    (
      "usb.autosuspend-delay-seconds",
      &rule.usb.autosuspend_delay_seconds,
      "power/autosuspend_delay_ms",
    ),
  ] {
    verify_control(
      rule,
      field,
      &device.name,
      device.path.join(control),
      expression,
      diagnostics,
    );
  }
}

fn verify_gpu_controls(
  rule: &Rule,
  gpu: &Gpu,
  diagnostics: &mut Vec<Diagnostic>,
) {
  verify_control(
    rule,
    "gpu.panel-power-savings",
    &gpu.name,
    gpu.path.join("amdgpu/panel_power_savings"),
    &rule.gpu.panel_power_savings,
    diagnostics,
  );
  let Some(expression) = active_action(&rule.gpu.radeon_powersave) else {
    return;
  };
  let Some(root) = gpu.radeon_control_root() else {
    diagnostics.push(Diagnostic::warning(
      rule,
      "gpu.radeon-powersave",
      format!("GPU {:?}: Radeon power controls are unavailable", gpu.name),
    ));
    return;
  };
  let verify = |control, diagnostics: &mut Vec<Diagnostic>| {
    verify_control(
      rule,
      "gpu.radeon-powersave",
      &gpu.name,
      root.join(control),
      &rule.gpu.radeon_powersave,
      diagnostics,
    );
  };
  verify("power_method", diagnostics);
  let Ok(constants::Constant::String(value)) = constants::analyze(expression)
  else {
    return;
  };
  match value {
    "default" | "auto" | "low" | "mid" | "high" => {
      verify("power_profile", diagnostics)
    },
    "dpm-battery" | "dpm-balanced" | "dpm-performance" => {
      verify("power_dpm_state", diagnostics)
    },
    "dynpm" => {},
    _ => {
      diagnostics.push(Diagnostic::warning(
        rule,
        "gpu.radeon-powersave",
        format!("Radeon power mode {value:?} is unsupported"),
      ))
    },
  }
}

fn verify_uncore_controls(
  rule: &Rule,
  device: &Uncore,
  diagnostics: &mut Vec<Diagnostic>,
) {
  for (field, expression, control) in [
    (
      "uncore.frequency-khz-minimum",
      &rule.uncore.frequency_khz_minimum,
      "min_freq_khz",
    ),
    (
      "uncore.frequency-khz-maximum",
      &rule.uncore.frequency_khz_maximum,
      "max_freq_khz",
    ),
  ] {
    verify_control(
      rule,
      field,
      &device.name,
      device.path.join(control),
      expression,
      diagnostics,
    );
    if let Ok(Some(value)) = constant_unsigned_integer(expression.as_ref())
      && !(device.initial_min_khz..=device.initial_max_khz).contains(&value)
    {
      diagnostics.push(Diagnostic::warning(
        rule,
        field,
        format!(
          "uncore {:?}: {value} kHz is outside hardware bounds {}..={} kHz",
          device.name, device.initial_min_khz, device.initial_max_khz
        ),
      ));
    }
  }
}

fn verify_alpm(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  if active_action(&rule.disk.alpm).is_none() {
    return;
  }
  match alpm_paths() {
    Ok(paths) if paths.is_empty() => {
      diagnostics.push(Diagnostic::warning(
        rule,
        "disk.alpm",
        "SATA link-power controls are unavailable",
      ))
    },
    Ok(paths) => {
      for path in paths {
        verify_control(
          rule,
          "disk.alpm",
          "SATA host",
          path,
          &rule.disk.alpm,
          diagnostics,
        );
      }
    },
    Err(error) => {
      diagnostics.push(Diagnostic::warning(
        rule,
        "disk.alpm",
        format!("SATA link-power controls are unknown: {error:#}"),
      ))
    },
  }
}

fn alpm_paths() -> anyhow::Result<Vec<PathBuf>> {
  let Some(entries) = fs::read_dir("/sys/class/scsi_host")? else {
    return Ok(Vec::new());
  };
  let mut paths = Vec::new();
  for entry in entries {
    let entry = entry.context("failed to read SATA host entry")?;
    let path = entry.path().join("link_power_management_policy");
    if fs::exists(&path) {
      paths.push(path);
    }
  }
  Ok(paths)
}

fn selected_power_supply_names<'a>(
  rule: &'a Rule,
  supplies: &'a [PowerSupply],
) -> Option<Vec<&'a str>> {
  let mut names = match &rule.power.for_ {
    None => supplies.iter().map(|supply| supply.name.as_str()).collect(),
    Some(expression) => constants::strings(expression)?,
  };
  names.sort_unstable();
  names.dedup();
  Some(names)
}

fn verify_charge_threshold_controls(
  rule: &Rule,
  supply: &PowerSupply,
  diagnostics: &mut Vec<Diagnostic>,
) {
  if supply.threshold_config.is_some() {
    return;
  }
  for (field, expression) in [
    (
      "power.charge-threshold-start",
      &rule.power.charge_threshold_start,
    ),
    (
      "power.charge-threshold-end",
      &rule.power.charge_threshold_end,
    ),
  ] {
    if matches!(constant_percentage(expression.as_ref()), Ok(Some(_))) {
      diagnostics.push(Diagnostic::warning(
        rule,
        field,
        format!(
          "power supply {:?}: charge-threshold control is unavailable",
          supply.name,
        ),
      ));
    }
  }
}

fn platform_profile_warning(
  current: &anyhow::Result<Option<String>>,
  available: &anyhow::Result<Vec<String>>,
  value: &str,
) -> Option<String> {
  match (current, available) {
    (Ok(None), _) => Some("control is unavailable".into()),
    (Err(error), _) => {
      Some(format!("current platform profile is unknown: {error:#}"))
    },
    (_, Err(error)) => {
      Some(format!(
        "supported platform profiles are unknown: {error:#}"
      ))
    },
    (Ok(current), Ok(available)) => {
      control_warning(current.as_deref(), available, value)
    },
  }
}

fn platform_alternatives_warning(
  current: &anyhow::Result<Option<String>>,
  available: &anyhow::Result<Vec<String>>,
  values: &[Expression],
) -> Option<String> {
  let candidates = literal_strings(values)?;
  if candidates.is_empty() {
    return None;
  }
  match (current, available) {
    (Ok(None), _) => Some("control is unavailable; setting is skipped".into()),
    (Err(error), _) => {
      Some(format!("current platform profile is unknown: {error:#}"))
    },
    (_, Err(error)) => {
      Some(format!(
        "supported platform profiles are unknown: {error:#}"
      ))
    },
    (Ok(current), Ok(available)) => {
      unavailable_alternatives_warning(
        &candidates,
        current.as_deref(),
        available,
      )
    },
  }
}

fn selected_cpu_numbers(rule: &Rule, cpus: &[Cpu]) -> Option<Vec<u32>> {
  let mut numbers = match &rule.cpu.for_ {
    None => cpus.iter().map(|cpu| cpu.number).collect(),
    Some(expression) => {
      let constants::Constant::List(values) =
        constants::analyze(expression).ok()?
      else {
        return None;
      };
      values
        .into_iter()
        .map(|value| {
          match value {
            constants::Constant::Number(value) => {
              CpusDelta::cpu_number(Expression::Number(value)).ok()
            },
            _ => None,
          }
        })
        .collect::<Option<Vec<_>>>()?
    },
  };
  numbers.sort_unstable();
  numbers.dedup();
  Some(numbers)
}

fn verify_policy_selection(
  rule: &Rule,
  cpus: &[Cpu],
  selected: &[u32],
  diagnostics: &mut Vec<Diagnostic>,
) {
  if rule.cpu.for_.is_none()
    || ![
      &rule.cpu.governor,
      &rule.cpu.energy_performance_preference,
      &rule.cpu.frequency_mhz_minimum,
      &rule.cpu.frequency_mhz_maximum,
    ]
    .iter()
    .any(|setting| active_action(setting).is_some())
  {
    return;
  }
  let policies: BTreeMap<_, _> = cpus
    .iter()
    .filter(|cpu| selected.binary_search(&cpu.number).is_ok())
    .filter_map(|cpu| cpu.policy.as_ref())
    .map(|policy| (&policy.path, policy))
    .collect();
  for policy in policies.into_values() {
    let omitted: Vec<_> = policy
      .related_cpus
      .iter()
      .filter(|number| selected.binary_search(number).is_err())
      .collect();
    if !omitted.is_empty() {
      diagnostics.push(Diagnostic::warning(
        rule,
        "cpu.for",
        format!(
          "{} also affects CPUs {omitted:?} outside cpu.for",
          policy.name
        ),
      ));
    }
  }
}

fn verify_cpu_values(
  rule: &Rule,
  cpu: &Cpu,
  diagnostics: &mut Vec<Diagnostic>,
) {
  for (field, expression) in [
    ("cpu.governor", &rule.cpu.governor),
    (
      "cpu.energy-performance-preference",
      &rule.cpu.energy_performance_preference,
    ),
    ("cpu.energy-perf-bias", &rule.cpu.energy_perf_bias),
  ] {
    let Some(expression) = expression else {
      continue;
    };
    let message = match constants::analyze(expression) {
      Ok(constants::Constant::String(value)) => {
        cpu_value_warning(cpu, field, value)
      },
      Ok(constants::Constant::Number(value))
        if field == "cpu.energy-performance-preference" =>
      {
        numeric_epp_warning(cpu, &value.to_string())
      },
      _ => alternatives_warning(cpu, field, expression),
    };
    if let Some(message) = message {
      diagnostics.push(Diagnostic::warning(
        rule,
        field,
        format!("CPU {}: {message}", cpu.number),
      ));
    }
  }
}

fn cpu_value_warning(cpu: &Cpu, field: &str, value: &str) -> Option<String> {
  let (current, available) = match field {
    "cpu.governor" => (&cpu.governor, &cpu.available_governors),
    "cpu.energy-performance-preference" if value.parse::<u16>().is_ok() => {
      return numeric_epp_warning(cpu, value);
    },
    "cpu.energy-performance-preference" => (&cpu.epp, &cpu.available_epps),
    "cpu.energy-perf-bias" => (&cpu.epb, &cpu.available_epbs),
    _ => unreachable!("unknown CPU control {field}"),
  };
  control_warning(current.as_deref(), available, value)
}

fn control_warning(
  current: Option<&str>,
  available: &[String],
  value: &str,
) -> Option<String> {
  if current.is_none() {
    Some("control is unavailable".into())
  } else if available.is_empty() {
    Some("supported values are unknown".into())
  } else if !available.iter().any(|candidate| candidate == value) {
    Some(format!("value {value:?} is unsupported"))
  } else {
    None
  }
}

fn numeric_epp_warning(cpu: &Cpu, value: &str) -> Option<String> {
  if cpu.epp.is_none() {
    return Some("control is unavailable".into());
  }
  cpu.validate_epp(value).err().map(|error| error.to_string())
}

// Fallback operators select from their source capability list, but the result
// must still be valid for the action receiving it.
fn alternatives_warning(
  cpu: &Cpu,
  field: &str,
  expression: &Expression,
) -> Option<String> {
  let (values, current, available) =
    match constants::active_expression(expression)? {
      Expression::FirstAvailableGovernor { values } => {
        (values, &cpu.governor, &cpu.available_governors)
      },
      Expression::FirstAvailableEnergyPerformancePreference { values } => {
        (values, &cpu.epp, &cpu.available_epps)
      },
      Expression::FirstAvailableEnergyPerfBias { values } => {
        (values, &cpu.epb, &cpu.available_epbs)
      },
      _ => return None,
    };
  let candidates = literal_strings(values)?;
  match candidates
    .iter()
    .find(|value| available.iter().any(|item| item == *value))
  {
    Some(value) => cpu_value_warning(cpu, field, value),
    None => {
      unavailable_alternatives_warning(
        &candidates,
        current.as_deref(),
        available,
      )
    },
  }
}

fn literal_strings(values: &[Expression]) -> Option<Vec<&str>> {
  values
    .iter()
    .map(|value| {
      match constants::analyze(value).ok()? {
        constants::Constant::String(value) => Some(value),
        _ => None,
      }
    })
    .collect()
}

fn unavailable_alternatives_warning(
  candidates: &[&str],
  current: Option<&str>,
  available: &[String],
) -> Option<String> {
  if candidates.is_empty()
    || candidates
      .iter()
      .any(|value| available.iter().any(|item| item == value))
  {
    None
  } else if current.is_none() {
    Some("control is unavailable; setting is skipped".into())
  } else if available.is_empty() {
    Some("supported values are unknown; setting is skipped".into())
  } else {
    Some(
      "none of the ordered alternatives are available; setting is skipped"
        .into(),
    )
  }
}

fn verify_cpu_frequencies(
  rule: &Rule,
  cpu: &Cpu,
  diagnostics: &mut Vec<Diagnostic>,
) {
  for (field, expression) in [
    ("cpu.frequency-mhz-minimum", &rule.cpu.frequency_mhz_minimum),
    ("cpu.frequency-mhz-maximum", &rule.cpu.frequency_mhz_maximum),
  ] {
    let Ok(Some(value)) = constant_frequency(expression.as_ref()) else {
      continue;
    };
    let message = match (
      cpu.frequency_control,
      cpu.frequency_minimum,
      cpu.frequency_maximum,
    ) {
      (false, ..) => Some("frequency control is unavailable".into()),
      (true, Some(minimum), Some(maximum))
        if value < minimum || value > maximum =>
      {
        Some(format!(
          "{value} is outside hardware bounds {minimum}..={maximum}"
        ))
      },
      (true, Some(_), Some(_)) => None,
      _ => Some("hardware frequency bounds are unknown".into()),
    };
    if let Some(message) = message {
      diagnostics.push(Diagnostic::warning(
        rule,
        field,
        format!("CPU {}: {message}", cpu.number),
      ));
    }
  }
}

#[cfg(test)]
mod tests {
  use std::{
    fs,
    io::{
      self,
      ErrorKind,
    },
    os::unix::fs::symlink,
  };

  use super::{
    super::{
      Severity,
      tests::rule_config,
    },
    *,
  };
  use crate::test_support::SystemFixture;

  type Verify = fn(&DaemonConfig) -> anyhow::Result<Vec<Diagnostic>>;

  const DEVICE_VERIFIERS: [(&str, &str, Verify); 4] = [
    ("disk", "/sys/block", DaemonConfig::verify_disk_hardware),
    (
      "usb",
      "/sys/bus/usb/devices",
      DaemonConfig::verify_usb_hardware,
    ),
    ("gpu", "/sys/class/drm", DaemonConfig::verify_gpu_hardware),
    (
      "uncore",
      "/sys/devices/system/cpu/intel_uncore_frequency",
      DaemonConfig::verify_uncore_hardware,
    ),
  ];

  fn hardware_fixture() -> SystemFixture {
    let fixture = SystemFixture::new();
    fixture.policy(0, &[0], "intel_pstate");
    fixture.cpu_telemetry(&[0]);
    fs::create_dir_all(fixture.path("/sys/class/power_supply")).unwrap();
    for (_, root, _) in DEVICE_VERIFIERS {
      fs::create_dir_all(fixture.path(root)).unwrap();
    }
    fixture
  }

  fn uncore(fixture: &SystemFixture, name: &str) {
    for (file, value) in [
      ("initial_min_freq_khz", "800"),
      ("initial_max_freq_khz", "3000"),
      ("min_freq_khz", "800"),
      ("max_freq_khz", "3000"),
    ] {
      fixture.write(
        format!("/sys/devices/system/cpu/intel_uncore_frequency/{name}/{file}"),
        value,
      );
    }
  }

  #[test]
  fn charge_threshold_verification_respects_selectors_and_dynamic_values() {
    let fixture = SystemFixture::new();
    fixture.write("/sys/class/power_supply/BAT0/type", "Battery");
    fixture.write("/sys/class/power_supply/BAT1/type", "Battery");
    fixture.write(
      "/sys/class/power_supply/BAT1/charge_control_start_threshold",
      "40",
    );
    fixture.write(
      "/sys/class/power_supply/BAT1/charge_control_end_threshold",
      "80",
    );
    let inventory = PowerSupplyInventory::scan().unwrap();
    assert!(inventory.failures.is_empty());
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "unavailable"
priority = 1
power.for = ["BAT0", "missing", "BAT0"]
power.charge-threshold-end = 60
[[rule]]
name = "supported"
priority = 2
power.for = ["BAT1"]
power.charge-threshold-start = 40
power.charge-threshold-end = 80
[[rule]]
name = "dynamic-selector"
priority = 3
power.for = ["BAT0", "$power-profile-preference"]
power.charge-threshold-end = 60
[[rule]]
name = "dynamic-value"
priority = 4
power.for = ["BAT0"]
power.charge-threshold-end = "$cpu-temperature"
[[rule]]
name = "disabled"
priority = 5
if = false
power.for = ["BAT0"]
power.charge-threshold-end = 60
"#,
    )
    .unwrap();
    let diagnostics = config.verify_power_hardware(&inventory.power_supplies);
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert_eq!(diagnostics[0].rule, "unavailable");
    assert_eq!(diagnostics[0].field, "power.charge-threshold-end");
    assert!(diagnostics[0].message.contains("BAT0"));
    assert!(diagnostics[0].message.contains("unavailable"));
    assert_eq!(diagnostics[1].field, "power.for");
    assert!(diagnostics[1].message.contains("missing"));
    assert!(diagnostics[1].message.contains("unknown"));
  }

  #[test]
  fn platform_alternatives_defer_dynamic_values_and_report_skipped_settings() {
    let fixture = SystemFixture::new();
    let control = "/sys/firmware/acpi/platform_profile";
    let choices = "/sys/firmware/acpi/platform_profile_choices";
    fixture.write(control, "balanced");
    fixture.write(choices, "balanced performance");
    for (values, expected) in [
      ("['missing', 'performance']", None),
      ("['missing']", Some("none of the ordered alternatives")),
      ("[]", None),
      ("['missing', '$power-profile-preference']", None),
    ] {
      let config: DaemonConfig = toml::from_str(&format!(
        "[[rule]]\nname='platform'\npriority=1\npower.\
         platform-profile={{first-available-platform-profile={values}}}",
      ))
      .unwrap();
      let diagnostics = config.verify_platform_hardware();
      match expected {
        Some(message) => {
          assert_eq!(diagnostics.len(), 1);
          assert!(diagnostics[0].message.contains(message));
        },
        None => assert!(diagnostics.is_empty(), "{diagnostics:?}"),
      }
    }
    let config: DaemonConfig = toml::from_str(
      "[[rule]]\nname='platform'\npriority=1\npower.\
       platform-profile={first-available-platform-profile=['performance']}",
    )
    .unwrap();
    for path in [control, choices] {
      fixture.fail_read(path, ErrorKind::PermissionDenied);
      let diagnostics = config.verify_platform_hardware();
      assert_eq!(diagnostics.len(), 1);
      assert!(diagnostics[0].message.contains("unknown"));
      fixture.clear_io(path);
    }
    fs::remove_file(fixture.path(control)).unwrap();
    let diagnostics = config.verify_platform_hardware();
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("unavailable"));
  }

  #[test]
  fn platform_verification_distinguishes_unavailable_unknown_and_unsupported() {
    let fixture = SystemFixture::new();
    let control = "/sys/firmware/acpi/platform_profile";
    let choices = "/sys/firmware/acpi/platform_profile_choices";
    let config: DaemonConfig = toml::from_str(
      "[[rule]]\nname='platform'\npriority=1\npower.platform-profile='\
       performance'\n[[rule]]\nname='disabled'\npriority=2\nif=false\npower.\
       platform-profile='invalid'\n",
    )
    .unwrap();
    let diagnostics = config.verify_platform_hardware();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule, "platform");
    assert_eq!(diagnostics[0].field, "power.platform-profile");
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert!(diagnostics[0].message.contains("unavailable"));
    fixture.write(control, "balanced");
    for (advertised, expected) in [("balanced", "unsupported"), ("", "unknown")]
    {
      fixture.write(choices, advertised);
      let diagnostics = config.verify_platform_hardware();
      assert_eq!(diagnostics.len(), 1);
      assert!(diagnostics[0].message.contains(expected));
    }
    fixture.write(choices, "balanced performance");
    assert!(config.verify_platform_hardware().is_empty());
    for path in [control, choices] {
      fixture.fail_read(path, ErrorKind::PermissionDenied);
      let diagnostics = config.verify_platform_hardware();
      assert_eq!(diagnostics.len(), 1);
      assert!(diagnostics[0].message.contains("unknown"));
      fixture.clear_io(path);
    }
    assert!(config.verify_platform_hardware().is_empty());
    assert_eq!(
      fs::read_to_string(fixture.path(control)).unwrap(),
      "balanced"
    );
    assert_eq!(fixture.write_count(control), 0);
    assert_eq!(fixture.write_count(choices), 0);
  }

  #[test]
  fn alternatives_follow_each_cpu_advertised_values() {
    let fixture = SystemFixture::new();
    let first = fixture.policy(0, &[0], "intel_pstate");
    let second = fixture.policy(1, &[1], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0, 1]);
    fixture.write(second.join("scaling_available_governors"), "powersave");
    fixture.write("/sys/devices/system/cpu/cpu0/power/energy_perf_bias", "6");
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "fallbacks"
priority = 1
cpu.for = [0]
cpu.governor = { first-available-governor = ["missing", "performance"] }
cpu.energy-performance-preference = { first-available-energy-performance-preference = ["missing", "power"] }
cpu.energy-perf-bias = { first-available-energy-perf-bias = ["missing", "balance-power"] }
[[rule]]
name = "numeric-is-not-advertised"
priority = 2
cpu.for = [0]
cpu.energy-performance-preference = { first-available-energy-performance-preference = ["123"] }
[[rule]]
name = "empty"
priority = 3
cpu.for = [0]
cpu.governor = { first-available-governor = [] }
[[rule]]
name = "heterogeneous"
priority = 4
cpu.governor = { first-available-governor = ["performance"] }
[[rule]]
name = "unavailable"
priority = 5
cpu.for = [1]
cpu.energy-performance-preference = { first-available-energy-performance-preference = ["power"] }
[[rule]]
name = "dynamic"
priority = 6
cpu.governor = { first-available-governor = [{ if = "?discharging", then = "performance", else = "powersave" }] }
"#,
    )
    .unwrap();
    let inventory = CpuInventory::scan().unwrap();
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    assert_eq!(diagnostics.len(), 3);
    assert_eq!(diagnostics[0].rule, "numeric-is-not-advertised");
    assert!(
      diagnostics[0]
        .message
        .contains("none of the ordered alternatives")
    );
    assert_eq!(diagnostics[1].rule, "heterogeneous");
    assert!(diagnostics[1].message.starts_with("CPU 1:"));
    assert_eq!(diagnostics[2].rule, "unavailable");
    assert!(diagnostics[2].message.contains("control is unavailable"));
    assert_eq!(fixture.write_count(first.join("scaling_governor")), 0);
    assert_eq!(fixture.write_count(second.join("scaling_governor")), 0);

    fs::remove_file(second.join("scaling_available_governors")).unwrap();
    let inventory = CpuInventory::scan().unwrap();
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    assert!(
      diagnostics[1]
        .message
        .contains("supported values are unknown")
    );
  }

  #[test]
  fn partial_policy_warnings_preserve_scope_across_hotplug() {
    let fixture = SystemFixture::new();
    fixture.policy(0, &[0, 1, 2], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0, 1, 2]);
    fixture.write("/sys/devices/system/cpu/cpu0/power/energy_perf_bias", "6");
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "partial"
priority = 1
cpu.for = [0, 1, 0]
cpu.governor = "performance"
[[rule]]
name = "whole"
priority = 2
cpu.for = [0, 1, 2]
cpu.governor = "performance"
[[rule]]
name = "local"
priority = 3
cpu.for = [0]
cpu.energy-perf-bias = "balance-power"
"#,
    )
    .unwrap();
    let inventory = CpuInventory::scan().unwrap();
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule, "partial");
    assert_eq!(diagnostics[0].field, "cpu.for");
    assert_eq!(
      diagnostics[0].message,
      "policy0 also affects CPUs [2] outside cpu.for"
    );

    fs::remove_dir_all(fixture.path("/sys/devices/system/cpu/cpu2")).unwrap();
    let inventory = CpuInventory::scan().unwrap();
    let offline = config.verify_cpu_hardware(&inventory.cpus);
    assert_eq!(offline[0], diagnostics[0]);
    assert_eq!(offline.len(), 2);
    assert_eq!(offline[1].rule, "whole");
    assert!(offline[1].message.contains("not discovered"));
  }

  #[test]
  fn hardware_diagnostics_distinguish_unknown_and_unavailable_controls() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0], "acpi-cpufreq");
    let unknown = fixture.policy(1, &[1], "acpi-cpufreq");
    fs::remove_file(unknown.join("scaling_available_governors")).unwrap();
    fixture.cpu_telemetry(&[0, 1, 2]);
    fixture.write("/sys/devices/system/cpu/cpu2/power/energy_perf_bias", "6");
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "unsupported"
priority = 1
cpu.for = [0, 0]
cpu.governor = "ondemand"
cpu.frequency-mhz-minimum = 4000
[[rule]]
name = "unknown"
priority = 2
cpu.for = [1]
cpu.governor = "performance"
[[rule]]
name = "no-cpufreq"
priority = 3
cpu.for = [2]
cpu.energy-performance-preference = "power"
cpu.energy-perf-bias = "balance-power"
[[rule]]
name = "missing"
priority = 4
cpu.for = [9]
cpu.governor = "performance"
[[rule]]
name = "disabled"
priority = 5
if = false
cpu.governor = "missing"
[[rule]]
name = "dynamic"
priority = 6
cpu.for = ["%cpu-core-count"]
cpu.governor = "missing"
"#,
    )
    .unwrap();
    let inventory = CpuInventory::scan().unwrap();
    assert!(inventory.failures.is_empty());
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    let fields: Vec<_> = diagnostics
      .iter()
      .map(|d| (d.rule.as_str(), d.field.as_str()))
      .collect();
    assert_eq!(fields, [
      ("unsupported", "cpu.governor"),
      ("unsupported", "cpu.frequency-mhz-minimum"),
      ("unknown", "cpu.governor"),
      ("no-cpufreq", "cpu.energy-performance-preference"),
      ("missing", "cpu.for"),
    ]);
    assert!(diagnostics[1].message.contains("outside hardware bounds"));
    assert!(diagnostics[2].message.contains("unknown"));
    assert!(diagnostics[3].message.contains("unavailable"));
    assert!(diagnostics[4].message.contains("not discovered"));
    for name in ["scaling_governor", "scaling_min_freq", "scaling_max_freq"] {
      assert_eq!(fixture.write_count(policy.join(name)), 0);
    }
  }

  #[test]
  fn numeric_epp_verification_requires_the_driver_and_control() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0], "intel_pstate");
    fixture.cpu_telemetry(&[0]);
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "numeric"
priority = 1
cpu.energy-performance-preference = 123
cpu.frequency-mhz-minimum = 800
cpu.frequency-mhz-maximum = 3000
"#,
    )
    .unwrap();
    let inventory = CpuInventory::scan().unwrap();
    assert!(config.verify_cpu_hardware(&inventory.cpus).is_empty());
    fixture.write(policy.join("scaling_driver"), "acpi-cpufreq");
    let inventory = CpuInventory::scan().unwrap();
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("P-State"));
    fs::remove_file(policy.join("energy_performance_preference")).unwrap();
    let inventory = CpuInventory::scan().unwrap();
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("unavailable"));
  }

  #[test]
  fn audio_verification_checks_each_requested_control() {
    let fixture = SystemFixture::new();
    let config: DaemonConfig = toml::from_str(
      r#"[[rule]]
name='audio'
priority=1
audio.timeout-seconds=10
audio.reset-controller=true
[[rule]]
name='disabled'
priority=2
if=false
audio.timeout-seconds=10"#,
    )
    .unwrap();

    let diagnostics = config.verify_audio_hardware();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert_eq!(diagnostics[0].field, "audio.timeout-seconds");
    assert_eq!(diagnostics[1].field, "audio.reset-controller");

    fixture.write("/sys/module/snd_hda_intel/parameters/power_save", "0");
    let diagnostics = config.verify_audio_hardware();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].field, "audio.reset-controller");

    fixture.write(
      "/sys/module/snd_hda_intel/parameters/power_save_controller",
      "N",
    );
    assert!(config.verify_audio_hardware().is_empty());
  }

  #[test]
  fn fallback_selection_uses_source_but_validation_uses_destination() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0], "intel_pstate");
    fixture.cpu_telemetry(&[0]);
    let config: DaemonConfig = toml::from_str(r#"
[[rule]]
name = "cross-invalid"
priority = 1
cpu.governor = { first-available-energy-performance-preference = ["power", "performance"] }
[[rule]]
name = "source-skips-unavailable"
priority = 2
cpu.governor = { first-available-energy-performance-preference = ["powersave", "performance"] }
[[rule]]
name = "same-control"
priority = 3
cpu.governor = { first-available-governor = ["missing", "powersave"] }
[[rule]]
name = "epp-destination"
priority = 4
cpu.energy-performance-preference = { first-available-governor = ["powersave", "performance"] }
[[rule]]
name = "cross-valid"
priority = 5
cpu.energy-performance-preference = { first-available-governor = ["missing", "performance"] }
"#).unwrap();
    assert!(config.verify_static().is_empty());
    let inventory = CpuInventory::scan().unwrap();
    let diagnostics = config.verify_cpu_hardware(&inventory.cpus);
    let fields: Vec<_> = diagnostics
      .iter()
      .map(|diagnostic| (diagnostic.rule.as_str(), diagnostic.field.as_str()))
      .collect();
    assert_eq!(fields, [
      ("cross-invalid", "cpu.governor"),
      ("epp-destination", "cpu.energy-performance-preference"),
    ]);
    assert!(diagnostics[0].message.contains("power"));
    assert!(diagnostics[1].message.contains("powersave"));
    assert_eq!(fixture.write_count(policy.join("scaling_governor")), 0);
    assert_eq!(
      fixture.write_count(policy.join("energy_performance_preference")),
      0
    );
  }

  #[test]
  fn discovery_failures_are_errors_not_empty_inventories() {
    for (device, root, verify) in DEVICE_VERIFIERS {
      let fixture = hardware_fixture();
      let config = rule_config(&format!("{device}.for=['absent']"));
      let diagnostics = verify(&config).unwrap();
      assert_eq!(diagnostics.len(), 1);
      assert_eq!(diagnostics[0].field, format!("{device}.for"));
      assert!(config.verify_hardware().is_ok());
      fs::remove_dir_all(fixture.path(root)).unwrap();
      fixture.write(root, "not a directory");
      for error in [verify(&config), config.verify_hardware()] {
        assert_eq!(
          error
            .unwrap_err()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
          ErrorKind::NotADirectory,
          "{device}"
        );
      }
    }
  }

  #[test]
  fn missing_required_inventory_roots_are_errors_not_empty_successes() {
    let fixture = SystemFixture::new();
    for root in ["/sys/block", "/sys/bus/usb/devices", "/sys/class/drm"] {
      fs::remove_dir(fixture.path(root)).unwrap();
    }
    assert!(DiskInventory::scan().is_err());
    assert!(UsbInventory::scan().is_err());
    assert!(GpuInventory::scan().is_err());
    assert!(UncoreInventory::scan().unwrap().devices.is_empty());
  }

  #[test]
  fn selectors_skip_disabled_and_dynamic_rules() {
    let _fixture = hardware_fixture();
    for (device, _, verify) in DEVICE_VERIFIERS {
      let config: DaemonConfig = toml::from_str(&format!(
        r#"
[[rule]]
name='enabled'
priority=1
{device}.for=['absent']
[[rule]]
name='disabled'
priority=2
if=false
{device}.for=['absent']
[[rule]]
name='dynamic'
priority=3
{device}.for=[{{if='?discharging', then='absent', else='other'}}]
[[rule]]
name='partly-dynamic'
priority=4
{device}.for=['absent', '$power-profile-preference']
"#
      ))
      .unwrap();
      let diagnostics = verify(&config).unwrap();
      assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
      assert_eq!(diagnostics[0].rule, "enabled");
      assert_eq!(diagnostics[0].field, format!("{device}.for"));
    }
  }

  #[test]
  fn partial_discovery_rejects_verification_without_writes() {
    let fixture = SystemFixture::new();
    fixture.write("/sys/block/sda/removable", "0");
    fixture.write("/sys/block/sda/queue/read_ahead_kb", "128");
    fixture.write("/sys/block/sdb/queue/read_ahead_kb", "128");
    let inventory = DiskInventory::scan().unwrap();
    assert_eq!(inventory.devices.len(), 1);
    assert_eq!(inventory.devices[0].name, "sda");
    assert_eq!(inventory.failures.len(), 1);
    assert_eq!(Disk::all().unwrap().len(), 1);
    assert!(
      rule_config("disk.readahead-kib=256")
        .verify_disk_hardware()
        .is_err()
    );
    assert_eq!(fixture.write_count("/sys/block/sda/queue/read_ahead_kb"), 0);

    fixture.write("/sys/bus/usb/devices/1-1/idVendor", "1234");
    fixture.write("/sys/bus/usb/devices/1-2/idVendor", "1234");
    fixture.fail_read(
      "/sys/bus/usb/devices/1-2/idVendor",
      ErrorKind::PermissionDenied,
    );
    let inventory = UsbInventory::scan().unwrap();
    assert_eq!(inventory.devices.len(), 1);
    assert_eq!(inventory.failures.len(), 1);
    assert_eq!(UsbDevice::all().unwrap().len(), 1);
    assert!(
      rule_config("usb.autosuspend=true")
        .verify_usb_hardware()
        .is_err()
    );

    uncore(&fixture, "package_00_die_00");
    fixture.write(
      "/sys/devices/system/cpu/intel_uncore_frequency/package_01_die_00/\
       min_freq_khz",
      "800",
    );
    let inventory = UncoreInventory::scan().unwrap();
    assert_eq!(inventory.devices.len(), 1);
    assert_eq!(inventory.failures.len(), 1);
    assert_eq!(Uncore::all().unwrap().len(), 1);
    assert!(
      rule_config("uncore.frequency-khz-minimum=1000")
        .verify_uncore_hardware()
        .is_err()
    );
    assert_eq!(
      fixture.write_count(
        "/sys/devices/system/cpu/intel_uncore_frequency/package_00_die_00/\
         min_freq_khz"
      ),
      0
    );
  }

  #[test]
  fn drm_inventory_keeps_unsupported_cards_and_rejects_dangling_cards() {
    let fixture = SystemFixture::new();
    fs::create_dir_all(fixture.path("/sys/class/drm/card0")).unwrap();
    symlink("missing-card", fixture.path("/sys/class/drm/card1")).unwrap();
    symlink(
      "missing-connector",
      fixture.path("/sys/class/drm/card0-eDP-1"),
    )
    .unwrap();
    let inventory = GpuInventory::scan().unwrap();
    assert_eq!(inventory.devices.len(), 1);
    assert_eq!(inventory.failures.len(), 1);
    assert_eq!(Gpu::all().unwrap().len(), 1);
    let config = rule_config("gpu.panel-power-savings=1");
    assert!(config.verify_gpu_hardware().is_err());
    fs::remove_file(fixture.path("/sys/class/drm/card1")).unwrap();
    let diagnostics = config.verify_gpu_hardware().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].field, "gpu.panel-power-savings");
  }

  #[test]
  fn scheduler_capabilities_distinguish_unavailable_unreadable_and_unsupported()
  {
    let fixture = SystemFixture::new();
    fixture.write("/sys/block/sda/removable", "0");
    let config = rule_config("disk.scheduler='bfq'");
    let path = "/sys/block/sda/queue/scheduler";
    let message = || {
      let diagnostics = config.verify_disk_hardware().unwrap();
      assert_eq!(diagnostics.len(), 1);
      diagnostics[0].message.clone()
    };
    assert!(message().contains("unavailable"));
    fixture.write(path, "[none] mq-deadline");
    assert!(message().contains("unsupported"));
    fixture.fail_read(path, ErrorKind::PermissionDenied);
    assert!(message().contains("unknown"));
    fixture.clear_io(path);
    fixture.write(path, "none [bfq]");
    assert!(config.verify_disk_hardware().unwrap().is_empty());
    assert_eq!(
      fs::read_to_string(fixture.path(path)).unwrap(),
      "none [bfq]"
    );
    assert_eq!(fixture.write_count(path), 0);
  }

  #[test]
  fn disk_requested_controls_respect_selection_and_optional_reads() {
    let fixture = hardware_fixture();
    fixture.write("/sys/block/sda/removable", "0");
    fixture.write("/sys/block/sda/queue/read_ahead_kb", "128");
    for selector in ["", "disk.for=['sda']\n"] {
      let config = rule_config(&format!("{selector}disk.scheduler='none'"));
      let diagnostics = config.verify_disk_hardware().unwrap();
      assert_eq!(diagnostics.len(), 1);
      assert_eq!(diagnostics[0].field, "disk.scheduler");
      fixture.write("/sys/block/sda/queue/scheduler", "[none] mq-deadline");
      fixture.fail_read(
        "/sys/block/sda/queue/scheduler",
        ErrorKind::PermissionDenied,
      );
      let diagnostics = config.verify_disk_hardware().unwrap();
      assert_eq!(diagnostics.len(), 1);
      assert_eq!(diagnostics[0].field, "disk.scheduler");
      for fields in [
        format!("if=false\n{selector}disk.scheduler='none'"),
        format!("{selector}disk.readahead-kib=256"),
      ] {
        assert!(
          rule_config(&fields)
            .verify_disk_hardware()
            .unwrap()
            .is_empty()
        );
      }
      fixture.clear_io("/sys/block/sda/queue/scheduler");
      fs::remove_file(fixture.path("/sys/block/sda/queue/scheduler")).unwrap();
    }
    for selector in [
      "{if='?discharging', then='sda', else='other'}",
      "'sda', {if='?discharging', then='sda', else='other'}",
      "",
    ] {
      assert!(
        rule_config(&format!("disk.for=[{selector}]\ndisk.scheduler='none'"))
          .verify_disk_hardware()
          .unwrap()
          .is_empty()
      );
    }
  }

  #[test]
  fn gpu_requested_controls_respect_selection_and_optional_reads() {
    let fixture = hardware_fixture();
    let panel = "/sys/class/drm/card0/amdgpu/panel_power_savings";
    fixture.write("/sys/class/drm/card0/device/power_method", "profile");
    fixture.write("/sys/class/drm/card0/device/power_profile", "auto");
    for selector in ["", "gpu.for=['card0']\n"] {
      let config = rule_config(&format!("{selector}gpu.panel-power-savings=1"));
      let diagnostics = config.verify_gpu_hardware().unwrap();
      assert_eq!(diagnostics.len(), 1);
      assert_eq!(diagnostics[0].field, "gpu.panel-power-savings");
      fixture.write(panel, "1");
      fixture.fail_read(panel, ErrorKind::PermissionDenied);
      let diagnostics = config.verify_gpu_hardware().unwrap();
      assert_eq!(diagnostics.len(), 1);
      assert_eq!(diagnostics[0].field, "gpu.panel-power-savings");
      for fields in [
        format!("if=false\n{selector}gpu.panel-power-savings=1"),
        format!("{selector}gpu.radeon-powersave='auto'"),
      ] {
        assert!(
          rule_config(&fields)
            .verify_gpu_hardware()
            .unwrap()
            .is_empty()
        );
      }
      fixture.clear_io(panel);
      fs::remove_file(fixture.path(panel)).unwrap();
    }
    for selector in [
      "{if='?discharging', then='card0', else='other'}",
      "'card0', {if='?discharging', then='card0', else='other'}",
      "",
    ] {
      assert!(
        rule_config(&format!(
          "gpu.for=[{selector}]\ngpu.panel-power-savings=1"
        ))
        .verify_gpu_hardware()
        .unwrap()
        .is_empty()
      );
    }
  }

  #[test]
  fn uncore_controls_inspect_selected_devices_without_writes() {
    let fixture = hardware_fixture();
    let root =
      "/sys/devices/system/cpu/intel_uncore_frequency/package_00_die_00";
    let maximum = format!("{root}/max_freq_khz");
    uncore(&fixture, "package_00_die_00");
    for fields in [
      "uncore.frequency-khz-maximum=2000",
      "uncore.for=['package_00_die_00']\nuncore.frequency-khz-maximum=2000",
      "if=false\nuncore.for=['missing']\nuncore.frequency-khz-maximum=2000",
      "uncore.for=[{if='?discharging', then='missing', \
       else='other'}]\nuncore.frequency-khz-maximum=2000",
      "uncore.for=[]\nuncore.frequency-khz-maximum=2000",
    ] {
      assert!(
        rule_config(fields)
          .verify_uncore_hardware()
          .unwrap()
          .is_empty()
      );
    }
    let config = rule_config("uncore.frequency-khz-maximum=2000");
    fs::remove_file(fixture.path(&maximum)).unwrap();
    assert!(config.verify_uncore_hardware().is_err());
    fixture.write(&maximum, "3000");
    fixture.fail_read(&maximum, ErrorKind::PermissionDenied);
    assert!(config.verify_uncore_hardware().is_err());
    fixture.clear_io(&maximum);
    assert!(config.verify_uncore_hardware().unwrap().is_empty());
    assert_eq!(fixture.write_count(format!("{root}/min_freq_khz")), 0);
    assert_eq!(fixture.write_count(&maximum), 0);
  }

  #[test]
  fn usb_gpu_and_uncore_check_only_requested_real_controls() {
    let fixture = SystemFixture::new();
    fixture.write("/sys/bus/usb/devices/1-1/idVendor", "1234");
    fixture.write("/sys/bus/usb/devices/1-1/power/control", "on");
    fixture.write("/sys/class/drm/card0/device/power_method", "profile");
    uncore(&fixture, "package_00_die_00");
    assert!(
      rule_config("usb.autosuspend=true")
        .verify_usb_hardware()
        .unwrap()
        .is_empty()
    );
    let diagnostics = rule_config("usb.autosuspend-delay-seconds=1")
      .verify_usb_hardware()
      .unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].field, "usb.autosuspend-delay-seconds");
    let diagnostics = rule_config("gpu.radeon-powersave='dpm-battery'")
      .verify_gpu_hardware()
      .unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].field, "gpu.radeon-powersave");
    let unsupported = rule_config("gpu.radeon-powersave='invalid'")
      .verify_gpu_hardware()
      .unwrap();
    assert_eq!(unsupported.len(), 1);
    assert!(unsupported[0].message.contains("unsupported"));
    assert!(
      rule_config("gpu.radeon-powersave='dynpm'")
        .verify_gpu_hardware()
        .unwrap()
        .is_empty()
    );
    let diagnostics =
      rule_config("uncore.frequency-khz-minimum={maximum=[1000,4000]}")
        .verify_uncore_hardware()
        .unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].field, "uncore.frequency-khz-minimum");
    assert_eq!(
      fixture.write_count("/sys/bus/usb/devices/1-1/power/control"),
      0
    );
    assert_eq!(
      fixture.write_count("/sys/class/drm/card0/device/power_method"),
      0
    );
  }

  #[test]
  fn configured_global_controls_report_missing_or_unreadable_without_writes() {
    let fixture = SystemFixture::new();
    let vm = "/proc/sys/vm/dirty_ratio";
    let audio = "/sys/module/snd_hda_intel/parameters/power_save";
    let pstate = "/sys/devices/system/cpu/intel_pstate/min_perf_pct";
    for path in [vm, audio, pstate] {
      fixture.write(path, "10");
      fixture.fail_read(path, ErrorKind::PermissionDenied);
    }
    let config = rule_config(
      r#"vm.dirty-ratio=20
vm.dirty-background-ratio=10
audio.timeout-seconds=1
cpu.pstate-min-performance-percent=20"#,
    );
    let vm_diagnostics = config.verify_vm_hardware();
    assert_eq!(vm_diagnostics.len(), 2);
    assert!(vm_diagnostics.iter().any(|diagnostic| {
      diagnostic.field == "vm.dirty-ratio"
        && diagnostic.message.contains("unknown")
    }));
    let audio_diagnostics = config.verify_audio_hardware();
    assert_eq!(audio_diagnostics.len(), 1);
    assert!(audio_diagnostics[0].message.contains("unknown"));
    let cpu_diagnostics = config.verify_cpu_hardware(&[]);
    assert_eq!(cpu_diagnostics.len(), 1);
    assert!(cpu_diagnostics[0].message.contains("unknown"));
    for path in [vm, audio, pstate] {
      assert_eq!(fixture.write_count(path), 0);
      assert_eq!(fs::read_to_string(fixture.path(path)).unwrap(), "10");
    }
  }

  #[test]
  fn dynamic_device_selectors_defer_local_controls_not_independent_globals() {
    let _fixture = SystemFixture::new();
    let config = rule_config(
      r#"cpu.for=[{if='?discharging',then=0,else=1}]
cpu.governor='missing'
cpu.pstate-min-performance-percent=20
cpu.pstate-max-performance-percent=80
cpu.dma-latency-us=10
cpu.turbo=true
disk.for=['missing','$power-profile-preference']
disk.scheduler='missing'
disk.alpm='min_power'
power.for=['missing','$power-profile-preference']
power.charge-threshold-end=80
power.platform-profile='balanced'"#,
    );
    let cpu = config.verify_cpu_hardware(&[]);
    let fields: Vec<_> = cpu
      .iter()
      .map(|diagnostic| diagnostic.field.as_str())
      .collect();
    assert_eq!(fields, [
      "cpu.pstate-min-performance-percent",
      "cpu.pstate-max-performance-percent",
      "cpu.turbo",
      "cpu.dma-latency-us"
    ]);
    let disk = config.verify_disk_hardware().unwrap();
    assert_eq!(disk.len(), 1);
    assert_eq!(disk[0].field, "disk.alpm");
    assert!(config.verify_power_hardware(&[]).is_empty());
    let platform = config.verify_platform_hardware();
    assert_eq!(platform.len(), 1);
    assert_eq!(platform[0].field, "power.platform-profile");
    let mut disabled = config;
    disabled.rules[0].condition = Expression::Boolean(false);
    assert!(disabled.verify_cpu_hardware(&[]).is_empty());
    assert!(disabled.verify_disk_hardware().unwrap().is_empty());
    assert!(disabled.verify_platform_hardware().is_empty());
  }

  #[test]
  fn provably_unset_actions_do_not_warn_about_unreached_hardware_controls() {
    let fixture = SystemFixture::new();
    fixture.write("/sys/block/sda/removable", "0");
    fixture.write("/sys/bus/usb/devices/1-1/idVendor", "1234");
    fs::create_dir_all(fixture.path("/sys/class/drm/card0")).unwrap();
    fixture.policy(0, &[0, 1], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0, 1]);
    let config = rule_config(
      r#"cpu.for=[0]
cpu.turbo={if=false,then=true}
cpu.pstate-min-performance-percent={if=false,then=20}
cpu.pstate-max-performance-percent={if=false,then=80}
cpu.dma-latency-us={if=false,then=10}
cpu.pm-qos-resume-latency-us={if=false,then=10}
cpu.frequency-mhz-minimum={if=false,then=800}
cpu.frequency-mhz-maximum={if=false,then=3000}
cpu.energy-performance-preference={if=false,then='power'}
cpu.energy-perf-bias={if=false,then='power'}
cpu.governor={if=false,then='missing'}
disk.scheduler={if=false,then='missing'}
disk.readahead-kib={if=false,then=128}
disk.apm={if=false,then=10}
disk.spindown={if=false,then=10}
disk.alpm={if=false,then='min_power'}
usb.autosuspend={if=false,then=true}
usb.autosuspend-delay-seconds={if=false,then=1}
gpu.panel-power-savings={if=false,then=1}
gpu.radeon-powersave={if=false,then='low'}
uncore.frequency-khz-minimum={if=false,then=1000}
uncore.frequency-khz-maximum={if=false,then=2000}
vm.dirty-ratio={if=false,then=20}
vm.transparent-hugepages={if=false,then='always'}
vm.transparent-hugepage-defrag={if=false,then='always'}
audio.timeout-seconds={if=false,then=1}
audio.reset-controller={if=false,then=true}
power.platform-profile={if=false,then='balanced'}
power.charge-threshold-end={if=false,then=80}"#,
    );
    assert!(config.verify_cpu_hardware(&Cpu::all().unwrap()).is_empty());
    for (_, _, verify) in DEVICE_VERIFIERS {
      assert!(verify(&config).unwrap().is_empty());
    }
    assert!(config.verify_vm_hardware().is_empty());
    assert!(config.verify_audio_hardware().is_empty());
    assert!(config.verify_platform_hardware().is_empty());
    assert!(config.verify_power_hardware(&[]).is_empty());
  }

  #[test]
  fn unknown_actions_still_check_configured_control_availability() {
    let fixture = SystemFixture::new();
    fixture.write("/sys/bus/usb/devices/1-1/idVendor", "1234");
    let config = rule_config(
      r#"usb.autosuspend='?discharging'
vm.dirty-ratio='%cpu-core-count'
audio.timeout-seconds='%cpu-core-count'
cpu.pstate-min-performance-percent='%cpu-core-count'"#,
    );
    for (diagnostics, field) in [
      (config.verify_usb_hardware().unwrap(), "usb.autosuspend"),
      (config.verify_vm_hardware(), "vm.dirty-ratio"),
      (config.verify_audio_hardware(), "audio.timeout-seconds"),
      (
        config.verify_cpu_hardware(&[]),
        "cpu.pstate-min-performance-percent",
      ),
    ] {
      assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
      assert_eq!(diagnostics[0].field, field);
    }
  }
}
