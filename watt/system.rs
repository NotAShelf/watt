#[cfg(not(unix))] use std::future;
use std::{
  collections::{
    BTreeMap,
    HashMap,
    HashSet,
    VecDeque,
  },
  env,
  future::Future,
  iter::once,
  path::{
    Path,
    PathBuf,
  },
  pin::Pin,
  sync::Arc,
  time::{
    Duration,
    Instant,
  },
};

use anyhow::{
  Context,
  bail,
};
use tokio::{
  signal,
  sync::{
    RwLock,
    watch,
  },
  time::sleep,
};

#[cfg(feature = "metrics")] use crate::metrics;
use crate::{
  config,
  cpu::{
    self,
    policy::Policy,
  },
  dbus::server,
  disk,
  fs,
  gpu,
  power_supply,
  profile,
  uncore,
  usb,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CpuLog {
  pub at: Instant,

  /// CPU usage between 0-1, a percentage.
  pub usage: f64,

  /// CPU temperature in celsius, if available.
  pub temperature: Option<f64>,

  /// Load average.
  pub load_average: f64,
}

impl CpuLog {
  pub(crate) fn recent(
    logs: &VecDeque<Self>,
    now: Instant,
    window: Duration,
  ) -> impl Iterator<Item = &Self> {
    logs.iter().filter(move |log| {
      now
        .checked_duration_since(log.at)
        .is_some_and(|age| age < window)
    })
  }
}

#[derive(Debug)]
struct CpuVolatility {
  usage: f64,

  temperature: Option<f64>,
}

#[derive(Debug, Clone)]
struct PowerSupplyLog {
  at: Instant,

  /// Charge 0-1, as a percentage.
  charge: f64,
}

#[derive(Debug, Clone)]
struct RuntimeFailure {
  condition:   DegradedCondition,
  first:       Instant,
  latest:      Instant,
  occurrences: u64,
}

/// A current runtime failure preventing Watt from evaluating or applying
/// policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DegradedCondition {
  pub context: String,
  pub message: String,
}

#[derive(Default, Debug)]
struct RuntimeFailures {
  errors: HashMap<String, RuntimeFailure>,
  seen:   HashSet<String>,
}

impl RuntimeFailures {
  fn begin_iteration(&mut self) {
    self.seen.clear();
  }

  fn record(&mut self, context: impl Into<String>, error: anyhow::Error) {
    const MAX_FAILURES: usize = 32;

    let context = context.into();
    let message = format!("{error:#}");
    let key = format!("{context}: {message}");
    self.seen.insert(key.clone());

    if let Some(failure) = self.errors.get_mut(&key) {
      failure.latest = Instant::now();
      failure.occurrences += 1;
      log::debug!("runtime failure persists: {key}");
      return;
    }

    if self.errors.len() >= MAX_FAILURES
      && let Some(key) = self
        .errors
        .iter()
        .min_by_key(|(_, failure)| failure.latest)
        .map(|(key, _)| key.clone())
    {
      self.errors.remove(&key);
    }

    log::error!("runtime failure: {key}");
    let now = Instant::now();
    self.errors.insert(key, RuntimeFailure {
      condition:   DegradedCondition { context, message },
      first:       now,
      latest:      now,
      occurrences: 1,
    });
  }

  fn attempt<T>(
    &mut self,
    context: impl Into<String>,
    operation: impl FnOnce() -> anyhow::Result<T>,
  ) -> Option<T> {
    match operation() {
      Ok(value) => Some(value),
      Err(error) => {
        self.record(context, error);
        None
      },
    }
  }

  fn apply_cpu_deltas(&mut self, deltas: &HashMap<Arc<cpu::Cpu>, cpu::Delta>) {
    let mut cpus: Vec<_> = deltas.iter().collect();
    cpus.sort_by_key(|(cpu, _)| cpu.number);
    let mut policies = HashSet::new();
    for (cpu, delta) in cpus {
      self.apply_cpu_settings(cpu, &delta.cpu_settings());
      let first_member = cpu
        .policy
        .as_ref()
        .is_none_or(|policy| policies.insert(&policy.path));
      if first_member {
        self.apply_cpu_settings(cpu, &delta.policy_settings());
      }
    }
  }

  fn apply_cpu_settings(&mut self, cpu: &cpu::Cpu, delta: &cpu::Delta) {
    if *delta == cpu::Delta::default() {
      return;
    }
    if self
      .attempt(format!("CPU validation for {cpu}"), || delta.validate(cpu))
      .is_some()
    {
      self.attempt(format!("CPU application for {cpu}"), || {
        delta.apply(&mut cpu.clone())
      });
    }
  }

  fn finish_iteration(&mut self) {
    self.errors.retain(|key, _| {
      let recovered = !self.seen.contains(key);
      if recovered {
        log::info!("runtime failure recovered: {key}");
      }
      !recovered
    });
  }

  fn latest_message(&self) -> Option<String> {
    self
      .errors
      .iter()
      .max_by_key(|(_, failure)| failure.latest)
      .map(|(message, failure)| {
        let elapsed = failure.latest.duration_since(failure.first).as_secs();
        format!(
          "{message} ({} occurrence(s), active for {elapsed}s)",
          failure.occurrences,
        )
      })
  }

  fn conditions(&self) -> Vec<DegradedCondition> {
    let mut conditions: Vec<_> = self
      .errors
      .values()
      .map(|failure| failure.condition.clone())
      .collect();
    conditions.sort();
    conditions
  }
}

#[derive(Default, Debug, Clone)]
struct System {
  is_ac: bool,

  lid_closed:      bool,
  virtual_machine: bool,
  chassis_type:    Option<String>,

  load_average_1min:  f64,
  load_average_5min:  f64,
  load_average_15min: f64,

  /// All CPUs.
  cpus:             HashSet<Arc<cpu::Cpu>>,
  /// CPU usage and temperature log.
  cpu_log:          VecDeque<CpuLog>,
  cpu_temperatures: HashMap<u32, f64>,

  /// All Intel uncore frequency devices.
  uncores: HashSet<Arc<uncore::Uncore>>,

  /// All block devices Watt can tune.
  disks: HashSet<Arc<disk::Disk>>,

  /// All USB devices Watt can tune.
  usb_devices: HashSet<Arc<usb::UsbDevice>>,

  /// All GPU devices Watt can tune.
  gpus: HashSet<Arc<gpu::Gpu>>,

  /// All power supplies.
  power_supplies:   HashSet<Arc<power_supply::PowerSupply>>,
  /// Power supply status log.
  power_supply_log: VecDeque<PowerSupplyLog>,

  /// Battery cycle count (aggregated average across all batteries).
  battery_cycles: Option<f64>,
  /// Battery health (aggregated average across all batteries).
  battery_health: Option<f64>,
}

fn retire_missing_devices<T>(
  subsystem: &'static str,
  previous: Vec<PathBuf>,
  current: &HashSet<Arc<T>>,
  path: impl Fn(&T) -> &Path,
) {
  fs::protect_scan_failures(
    subsystem,
    &previous,
    current.iter().map(|device| path(device).to_owned()),
  );
  for previous_path in previous {
    if fs::known_missing(&previous_path)
      && !current.iter().any(|device| path(device) == previous_path)
    {
      fs::retire_settings_under(previous_path);
    }
  }
}

fn reject_partial_inventory(
  subsystem: &str,
  failures: Vec<String>,
) -> anyhow::Result<()> {
  if !failures.is_empty() {
    bail!("partial {subsystem} scan failed: {}", failures.join("; "));
  }
  Ok(())
}

fn protect_cpu_scan_failures(
  previous: &HashSet<Arc<cpu::Cpu>>,
  current: &HashSet<Arc<cpu::Cpu>>,
) {
  let roots = |cpus: &HashSet<Arc<cpu::Cpu>>| -> Vec<PathBuf> {
    cpus
      .iter()
      .flat_map(|cpu| {
        let local =
          PathBuf::from(format!("/sys/devices/system/cpu/cpu{}", cpu.number));
        once(local).chain(cpu.policy.as_ref().map(|policy| policy.path.clone()))
      })
      .collect()
  };
  fs::protect_scan_failures("CPU", &roots(previous), roots(current));
}

impl System {
  fn eval_state(
    &self,
    now: Instant,
    last_cpu_activity: Instant,
    power_profile_preference: profile::PowerProfile,
    turbo_available: bool,
  ) -> config::EvalState<'_, '_> {
    let volatility = self.cpu_volatility(now);
    config::EvalState {
      frequency_available: self
        .cpus
        .iter()
        .any(|cpu| cpu.frequency_available()),
      turbo_available,

      cpu_usage: self.cpu_log.back().map_or(0.0, |log| log.usage),
      cpu_usage_volatility: volatility.as_ref().map(|vol| vol.usage),
      cpu_temperature: self.cpu_log.back().and_then(|log| log.temperature),
      cpu_temperature_volatility: volatility.and_then(|vol| vol.temperature),
      cpu_idle_seconds: now
        .saturating_duration_since(last_cpu_activity)
        .as_secs_f64(),
      cpu_frequency_maximum: self
        .cpus
        .iter()
        .filter_map(|cpu| cpu.frequency_maximum)
        .max()
        .map(|frequency| frequency.as_mhz()),
      cpu_frequency_minimum: self
        .cpus
        .iter()
        .filter_map(|cpu| cpu.frequency_minimum)
        .min()
        .map(|frequency| frequency.as_mhz()),

      lid_closed: self.lid_closed,
      virtual_machine: self.virtual_machine,
      chassis_type: self.chassis_type.as_deref(),

      power_supply_charge: self.power_supply_log.back().map(|log| log.charge),
      power_supply_discharge_rate: self.power_supply_discharge_rate(),

      battery_cycles: self.battery_cycles,
      battery_health: self.battery_health,

      discharging: self.is_discharging(),
      power_profile_preference,

      context: config::EvalContext::WidestPossible,

      cpus: &self.cpus,
      uncores: &self.uncores,
      disks: &self.disks,
      usb_devices: &self.usb_devices,
      gpus: &self.gpus,
      power_supplies: &self.power_supplies,
      cpu_log: &self.cpu_log,
    }
  }

  fn scan(&mut self, failures: &mut RuntimeFailures) {
    log::trace!("scanning view of system hardware...");

    self.scan_component(failures, "CPU scan", Self::scan_cpus);
    self.scan_component(
      failures,
      "power supply scan",
      Self::scan_power_supplies,
    );
    self.scan_component(failures, "uncore scan", Self::scan_uncores);
    self.scan_component(failures, "disk scan", Self::scan_disks);
    self.scan_component(failures, "USB scan", Self::scan_usb_devices);
    self.scan_component(failures, "GPU scan", Self::scan_gpus);
    self.scan_component(failures, "AC state scan", Self::scan_ac_state);
    self.scan_component(failures, "load average scan", Self::scan_load_average);
    self.scan_component(failures, "lid state scan", Self::scan_lid_state);
    self.scan_component(failures, "chassis type scan", Self::scan_chassis_type);
    self.scan_component(
      failures,
      "virtual machine scan",
      Self::scan_virtual_machine,
    );
    self.scan_component(failures, "temperature scan", Self::scan_temperatures);
    self.append_logs();
  }

  fn scan_component(
    &mut self,
    failures: &mut RuntimeFailures,
    context: &'static str,
    scan: impl FnOnce(&mut Self) -> anyhow::Result<()>,
  ) {
    if let Err(error) = scan(self) {
      failures.record(context, error);
    }
  }

  fn scan_cpus(&mut self) -> anyhow::Result<()> {
    let previous_stats: HashMap<u32, cpu::CpuStat> = self
      .cpus
      .iter()
      .map(|cpu| (cpu.number, cpu.stat.clone()))
      .collect();
    let inventory = cpu::CpuInventory::scan()?;
    let cpus = inventory
      .cpus
      .into_iter()
      .map(|mut cpu| {
        cpu.previous_stat = previous_stats.get(&cpu.number).cloned();
        Arc::from(cpu)
      })
      .collect::<HashSet<_>>();
    protect_cpu_scan_failures(&self.cpus, &cpus);
    let current_policies: HashSet<_> = cpus
      .iter()
      .filter_map(|cpu| cpu.policy.as_ref().map(|policy| &policy.path))
      .collect();
    let previous_policies: HashSet<_> = self
      .cpus
      .iter()
      .filter_map(|cpu| cpu.policy.as_ref().map(|policy| &policy.path))
      .collect();
    for path in previous_policies.difference(&current_policies) {
      let present = self.cpus.iter().any(|cpu| {
        cpu
          .policy
          .as_ref()
          .is_some_and(|policy| &policy.path == *path)
          && !fs::known_missing(format!(
            "/sys/devices/system/cpu/cpu{}",
            cpu.number
          ))
      });
      if !present || fs::known_missing(path) {
        fs::retire_settings_under(path);
      }
    }
    for number in previous_stats.keys() {
      let path = format!("/sys/devices/system/cpu/cpu{number}");
      if !cpus.iter().any(|cpu| cpu.number == *number)
        && fs::known_missing(&path)
      {
        fs::retire_settings_under(path);
      }
    }
    self.cpus = cpus;
    reject_partial_inventory("CPU", inventory.failures)
  }

  fn scan_power_supplies(&mut self) -> anyhow::Result<()> {
    let previous = self
      .power_supplies
      .iter()
      .map(|power_supply| power_supply.path.clone())
      .collect::<Vec<_>>();
    let inventory = power_supply::PowerSupplyInventory::scan()?;
    let power_supplies = inventory
      .power_supplies
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices(
      "power-supply",
      previous,
      &power_supplies,
      |supply| &supply.path,
    );
    self.power_supplies = power_supplies;
    reject_partial_inventory("power-supply", inventory.failures)
  }

  fn scan_uncores(&mut self) -> anyhow::Result<()> {
    let previous = self
      .uncores
      .iter()
      .map(|uncore| uncore.path.clone())
      .collect::<Vec<_>>();
    let inventory = uncore::UncoreInventory::scan()?;
    let uncores = inventory
      .devices
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices("uncore", previous, &uncores, |uncore| &uncore.path);
    self.uncores = uncores;
    reject_partial_inventory("uncore", inventory.failures)
  }

  fn scan_disks(&mut self) -> anyhow::Result<()> {
    let previous = self
      .disks
      .iter()
      .map(|disk| disk.path.clone())
      .collect::<Vec<_>>();
    let inventory = disk::DiskInventory::scan()?;
    let disks = inventory
      .devices
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices("disk", previous, &disks, |disk| &disk.path);
    self.disks = disks;
    reject_partial_inventory("disk", inventory.failures)
  }

  fn scan_usb_devices(&mut self) -> anyhow::Result<()> {
    let previous = self
      .usb_devices
      .iter()
      .map(|device| device.path.clone())
      .collect::<Vec<_>>();
    let inventory = usb::UsbInventory::scan()?;
    let usb_devices = inventory
      .devices
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices("USB", previous, &usb_devices, |device| {
      &device.path
    });
    self.usb_devices = usb_devices;
    reject_partial_inventory("USB", inventory.failures)
  }

  fn scan_gpus(&mut self) -> anyhow::Result<()> {
    let previous = self
      .gpus
      .iter()
      .flat_map(|gpu| gpu.control_roots().map(Path::to_owned))
      .collect::<Vec<_>>();
    let inventory = gpu::GpuInventory::scan()?;
    let gpus = inventory
      .devices
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    let roots = gpus
      .iter()
      .flat_map(|gpu| gpu.control_roots())
      .collect::<HashSet<_>>();
    fs::protect_scan_failures(
      "GPU",
      &previous,
      roots.iter().map(|path| path.to_path_buf()),
    );
    for path in previous {
      if fs::known_missing(&path) && !roots.contains(path.as_path()) {
        fs::retire_settings_under(path);
      }
    }
    self.gpus = gpus;
    reject_partial_inventory("GPU", inventory.failures)
  }

  fn scan_ac_state(&mut self) -> anyhow::Result<()> {
    self.is_ac = self
      .power_supplies
      .iter()
      .any(|power_supply| power_supply.is_ac())
      || self.is_desktop()?;
    Ok(())
  }

  fn scan_chassis_type(&mut self) -> anyhow::Result<()> {
    self.chassis_type = read_chassis_type()?;
    Ok(())
  }

  fn scan_virtual_machine(&mut self) -> anyhow::Result<()> {
    self.virtual_machine = detect_virtual_machine()?;
    Ok(())
  }

  fn append_logs(&mut self) {
    let at = Instant::now();
    self.append_cpu_log(at);
    self.append_power_supply_log(at);
    self.aggregate_battery_data();
  }

  fn append_cpu_log(&mut self, at: Instant) {
    if self.cpus.is_empty() {
      return;
    }
    while self.cpu_log.len() >= 100 {
      self.cpu_log.pop_front();
    }
    self.cpu_log.push_back(CpuLog {
      at,
      usage: self.cpus.iter().map(|cpu| cpu.current_usage()).sum::<f64>()
        / self.cpus.len() as f64,
      temperature: (!self.cpu_temperatures.is_empty()).then(|| {
        self.cpu_temperatures.values().sum::<f64>()
          / self.cpu_temperatures.len() as f64
      }),
      load_average: self.load_average_1min,
    });
  }

  fn append_power_supply_log(&mut self, at: Instant) {
    let (charge_sum, charge_count) = self.power_supplies.iter().fold(
      (0.0, 0u32),
      |(sum, count), power_supply| {
        match power_supply.charge_percent {
          Some(charge) => (sum + charge, count + 1),
          None => (sum, count),
        }
      },
    );
    if charge_count == 0 {
      return;
    }
    while self.power_supply_log.len() >= 100 {
      self.power_supply_log.pop_front();
    }
    self.power_supply_log.push_back(PowerSupplyLog {
      at,
      charge: charge_sum / charge_count as f64,
    });
  }

  fn aggregate_battery_data(&mut self) {
    let batteries = config::find_batteries(&self.power_supplies);
    let (cycle_sum, cycle_count, health_sum, health_count) =
      batteries.iter().fold(
        (0u64, 0u32, 0.0, 0u32),
        |(cycles, cycle_count, health, health_count), battery| {
          (
            cycles + battery.cycles.unwrap_or_default(),
            cycle_count + u32::from(battery.cycles.is_some()),
            health + battery.health.unwrap_or_default(),
            health_count + u32::from(battery.health.is_some()),
          )
        },
      );
    self.battery_cycles =
      (cycle_count > 0).then(|| cycle_sum as f64 / cycle_count as f64);
    self.battery_health =
      (health_count > 0).then(|| health_sum / health_count as f64);
  }

  fn scan_temperatures(&mut self) -> anyhow::Result<()> {
    log::debug!("scanning CPU temperatures...");
    let mut temperatures = HashMap::new();
    Self::scan_hwmon_temperatures(&mut temperatures)?;
    if temperatures.is_empty() {
      Self::scan_thermal_zones(&mut temperatures)?;
    }
    self.cpu_temperatures = temperatures;
    Ok(())
  }

  fn scan_hwmon_temperatures(
    temperatures: &mut HashMap<u32, f64>,
  ) -> anyhow::Result<()> {
    const PATH: &str = "/sys/class/hwmon";
    for entry in fs::read_dir(PATH)
      .context("failed to read hardware information")?
      .with_context(|| format!("'{PATH}' doesn't exist, are you on linux?"))?
    {
      let entry =
        entry.with_context(|| format!("failed to read entry of '{PATH}'"))?;
      Self::read_hwmon_temperatures(&entry.path(), temperatures)?;
    }
    Ok(())
  }

  fn read_hwmon_temperatures(
    device_path: &Path,
    temperatures: &mut HashMap<u32, f64>,
  ) -> anyhow::Result<()> {
    let Some(name) = fs::read(device_path.join("name")).with_context(|| {
      format!(
        "failed to read name of hardware entry at '{}'",
        device_path.display()
      )
    })?
    else {
      return Ok(());
    };
    if matches!(
      name.as_str(),
      "coretemp" | "k10temp" | "zenpower" | "amdgpu"
    ) || name.contains("cpu")
      || name.contains("temp")
    {
      Self::get_temperatures(device_path, temperatures)?;
    }
    Ok(())
  }

  fn scan_thermal_zones(
    temperatures: &mut HashMap<u32, f64>,
  ) -> anyhow::Result<()> {
    const PATH: &str = "/sys/devices/virtual/thermal";
    log::trace!("no hwmon CPU temperatures, checking '{PATH}'");
    let Some(zones) =
      fs::read_dir(PATH).context("failed to read thermal information")?
    else {
      return Ok(());
    };
    let mut counter = 0;
    for entry in zones {
      let entry =
        entry.with_context(|| format!("failed to read entry of '{PATH}'"))?;
      if !entry
        .file_name()
        .to_string_lossy()
        .starts_with("thermal_zone")
      {
        continue;
      }
      let Some(temperature) = Self::read_thermal_zone(&entry.path())? else {
        continue;
      };
      temperatures.insert(777 + counter, temperature);
      counter += 1;
    }
    Ok(())
  }

  fn read_thermal_zone(zone_path: &Path) -> anyhow::Result<Option<f64>> {
    let Some(kind) = fs::read(zone_path.join("type")).with_context(|| {
      format!("failed to read type of zone at '{}'", zone_path.display())
    })?
    else {
      return Ok(None);
    };
    if !kind.contains("cpu") && !kind.contains("x86") && !kind.contains("core")
    {
      return Ok(None);
    }
    fs::read_n::<i64>(zone_path.join("temp"))
      .with_context(|| {
        format!(
          "failed to read temperature of zone at '{}'",
          zone_path.display()
        )
      })
      .map(|temperature| temperature.map(|value| value as f64 / 1000.0))
  }

  fn get_temperatures(
    device_path: &Path,
    temperatures: &mut HashMap<u32, f64>,
  ) -> anyhow::Result<()> {
    // Increased range to handle systems with many sensors.
    for i in 1..=96 {
      let label_path = device_path.join(format!("temp{i}_label"));
      let input_path = device_path.join(format!("temp{i}_input"));

      if !label_path.exists() || !input_path.exists() {
        log::debug!(
          "{label_path} or {input_path} doesn't exist, skipping temp label",
          label_path = label_path.display(),
          input_path = input_path.display(),
        );
        continue;
      }

      log::debug!(
        "{label_path} or {input_path} exists, scanning temp label...",
        label_path = label_path.display(),
        input_path = input_path.display(),
      );

      let Some(label) = fs::read(&label_path).with_context(|| {
        format!(
          "failed to read hardware hardware device label from '{path}'",
          path = label_path.display(),
        )
      })?
      else {
        continue;
      };
      log::debug!("label content: {label}");

      // Match various common label formats:
      // "Core X", "core X", "Core-X", "CPU Core X", etc.
      let number = label
        .trim()
        .trim_start_matches("cpu")
        .trim_start_matches("CPU")
        .trim_start()
        .trim_start_matches("core")
        .trim_start_matches("Core")
        .trim_start()
        .trim_start_matches("Tctl")
        .trim_start_matches("Tdie")
        .trim_start_matches("Tccd")
        .trim_start();

      let number = if number.chars().all(|c| c.is_ascii_digit()) {
        number
      } else {
        number
          .trim_start_matches([
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
          ])
          .trim_start()
          .trim_start_matches("-")
      };

      log::debug!(
        "stripped 'Core' or similar identifier prefix of label content: \
         {number}"
      );

      let key = number
        .parse::<u32>()
        .ok()
        .or_else(|| number.is_empty().then_some(0));
      let Some(key) = key else {
        log::debug!("stripped content not a valid number, skipping");
        continue;
      };

      let Some(temperature_mc) =
        fs::read_n::<i64>(&input_path).with_context(|| {
          format!(
            "failed to read CPU temperature from '{path}'",
            path = input_path.display(),
          )
        })?
      else {
        continue;
      };
      log::debug!(
        "temperature content: {celsius} celsius",
        celsius = temperature_mc as f64 / 1000.0,
      );

      temperatures.insert(key, temperature_mc as f64 / 1000.0);
    }

    Ok(())
  }

  fn scan_load_average(&mut self) -> anyhow::Result<()> {
    log::trace!("scanning load average");

    let content = fs::read("/proc/loadavg")
      .context("failed to read load average from '/proc/loadavg'")?
      .context("'/proc/loadavg' doesn't exist, are you on linux?")?;

    let mut parts = content.split_whitespace();

    let (
      Some(load_average_1min),
      Some(load_average_5min),
      Some(load_average_15min),
    ) = (parts.next(), parts.next(), parts.next())
    else {
      bail!(
        "failed to parse first 3 load average entries due to there not being \
         enough, content: {content}"
      );
    };

    self.load_average_1min = load_average_1min
      .parse()
      .context("failed to parse load average")?;
    self.load_average_5min = load_average_5min
      .parse()
      .context("failed to parse load average")?;
    self.load_average_15min = load_average_15min
      .parse()
      .context("failed to parse load average")?;

    Ok(())
  }

  // Scan and identify the current lid state.
  // XXX: Most "uniform" APIs for identifying this data rely on some abstraction
  // library that *might or might not be installed*. The verbose fallback is,
  // unfortunately, necessary as there is no guarantee that we can use those
  // APIs.
  fn scan_lid_state(&mut self) -> anyhow::Result<()> {
    log::trace!("scanning lid state");

    // Try ACPI button interface first
    let acpi_lid_paths = [
      "/proc/acpi/button/lid/LID/state", // most likely to exist
      "/proc/acpi/button/lid/LID0/state",
      "/proc/acpi/button/lid/LID1/state",
    ];

    for path in acpi_lid_paths {
      if let Some(content) =
        fs::read(path).context("failed to read lid state from ACPI")?
      {
        // Content is typically "state:      open" or "state:      closed"
        self.lid_closed = content.contains("closed");
        log::debug!("lid state from {path}: {content}");
        return Ok(());
      }
    }

    // Try sysfs input device interface as fallback
    const INPUT_PATH: &str = "/sys/class/input";

    if let Some(input_entries) =
      fs::read_dir(INPUT_PATH).context("failed to read input device entries")?
    {
      for entry in input_entries {
        let Ok(entry) = entry else {
          log::debug!("failed to read input device entry");
          continue;
        };

        let entry_path = entry.path();
        let device_name_path = entry_path.join("device/name");

        let Some(name) = fs::read(&device_name_path).with_context(|| {
          format!(
            "failed to read input device name from '{path}'",
            path = device_name_path.display(),
          )
        })?
        else {
          continue;
        };

        // Look for lid switch input device
        let "Lid Switch" = name.trim() else {
          continue;
        };

        // Read the lid switch state from the SW_LID capability
        let state_path = entry_path.join("device/capabilities/sw");

        let Some(sw_caps) = fs::read(&state_path).with_context(|| {
          format!(
            "failed to read switch capabilities from '{path}'",
            path = state_path.display(),
          )
        })?
        else {
          log::debug!(
            "found lid switch at {path} but no switch capabilities file",
            path = entry_path.display()
          );
          continue;
        };

        // SW_LID is bit 0 in the capabilities bitmask
        // The state file shows the current state of switches as a hex bitmask
        // If bit 0 is set, the lid is closed
        if let Ok(caps) = u64::from_str_radix(sw_caps.trim(), 16) {
          self.lid_closed = (caps & 0x1) != 0;
          log::debug!(
            "lid state from input device {path}: {state}",
            path = entry_path.display(),
            state = if self.lid_closed { "closed" } else { "open" }
          );
          return Ok(());
        }
      }
    }

    // If we reach here, this is likely a desktop or the lid state is not
    // available Default to lid open (false)
    log::debug!(
      "no lid switch found, assuming desktop or lid state unavailable"
    );
    self.lid_closed = false;

    Ok(())
  }

  fn is_desktop(&mut self) -> anyhow::Result<bool> {
    log::debug!("checking chassis type to determine if system is a desktop");
    if let Some(chassis_type) = fs::read("/sys/class/dmi/id/chassis_type")
      .context("failed to read chassis type")?
    {
      // 3=Desktop, 4=Low Profile Desktop, 5=Pizza Box, 6=Mini Tower,
      // 7=Tower, 8=Portable, 9=Laptop, 10=Notebook, 11=Hand Held, 13=All In
      // One, 14=Sub Notebook, 15=Space-saving, 16=Lunch Box, 17=Main
      // Server Chassis, 31=Convertible Laptop
      match chassis_type.trim() {
        // Desktop form factors.
        "3" | "4" | "5" | "6" | "7" | "15" | "16" | "17" => {
          log::debug!("chassis is a desktop form factor, short circuting true");
          return Ok(true);
        },

        // Laptop form factors.
        "9" | "10" | "14" | "31" => {
          log::debug!("chassis is a laptop form factor, short circuting false");
          return Ok(false);
        },

        // Unknown, continue with other checks
        unknown => log::debug!("unknown chassis type: '{unknown}'"),
        // God, I hate hardware.
      }
    }

    // Check battery-specific ACPI paths that laptops typically have
    let laptop_acpi_paths = [
      "/sys/class/power_supply/BAT0",
      "/sys/class/power_supply/BAT1",
      "/proc/acpi/battery",
    ];

    log::debug!("checking existence of ACPI paths");
    for path in laptop_acpi_paths {
      if fs::exists(path) {
        log::debug!("path '{path}' exists, short circuting false");
        return Ok(false); // Likely a laptop.
      }
    }

    log::debug!("checking if power saving paths exists");
    // Check CPU power policies, desktops often don't have these
    let power_saving_exists =
      fs::exists("/sys/module/intel_pstate/parameters/no_hwp")
        || fs::exists("/sys/devices/system/cpu/cpufreq/conservative");

    if !power_saving_exists {
      log::debug!("power saving paths do not exist, short circuting true");
      return Ok(true); // likely a desktop.
    }

    // Default to assuming desktop if we can't determine.
    log::debug!(
      "cannot determine whether if we are a desktop, defaulting to true"
    );
    Ok(true)
  }

  fn cpu_volatility(&self, now: Instant) -> Option<CpuVolatility> {
    let mut logs =
      CpuLog::recent(&self.cpu_log, now, Duration::from_secs(5 * 60));
    let mut previous = logs.next()?;
    let mut usage_change_sum = 0.0;
    let mut change_count = 0;
    let mut temperature_change_sum = 0.0;
    let mut temperature_change_count = 0;

    for sample in logs {
      if sample.at <= previous.at {
        return None;
      }
      usage_change_sum += (sample.usage - previous.usage).abs();
      change_count += 1;
      if let (Some(before), Some(after)) =
        (previous.temperature, sample.temperature)
      {
        temperature_change_sum += (after - before).abs();
        temperature_change_count += 1;
      }
      previous = sample;
    }

    (change_count > 0).then(|| {
      CpuVolatility {
        usage:       usage_change_sum / change_count as f64,
        temperature: (temperature_change_count > 0)
          .then(|| temperature_change_sum / temperature_change_count as f64),
      }
    })
  }

  fn is_cpu_idle(&self, now: Instant) -> bool {
    let (total, count) =
      CpuLog::recent(&self.cpu_log, now, Duration::from_secs(5 * 60))
        .fold((0.0, 0usize), |(total, count), log| {
          (total + log.usage, count + 1)
        });
    if count < 2 {
      return false;
    }

    total / (count as f64) < 0.1
      && self
        .cpu_volatility(now)
        .is_some_and(|volatility| volatility.usage < 0.05)
  }

  fn poll_delay_for_power_source(&self) -> Duration {
    if !self.is_discharging() {
      return Duration::from_secs(5);
    }
    let seconds = match self.power_supply_discharge_rate() {
      Some(rate) if rate > 0.2 => 15.0,
      Some(rate) if rate > 0.1 => 10.0,
      Some(_) => 7.5,
      None => 10.0,
    };
    Duration::from_secs_f64(seconds)
  }

  fn is_discharging(&self) -> bool {
    self.power_supplies.iter().any(|power_supply| {
      power_supply.charge_state.as_deref() == Some("Discharging")
    })
  }

  /// Fraction of battery capacity discharged per hour during the latest
  /// uninterrupted period of nonincreasing charge.
  fn power_supply_discharge_rate(&self) -> Option<f64> {
    let end = self.power_supply_log.back()?;
    let mut start = end;

    for sample in self.power_supply_log.iter().rev().skip(1) {
      if sample.at >= start.at || sample.charge < start.charge {
        break;
      }
      start = sample;
    }

    let elapsed = end.at.checked_duration_since(start.at)?.as_secs_f64();
    (elapsed > 0.0).then(|| (start.charge - end.charge) * 3600.0 / elapsed)
  }
}

/// Calculate the idle time multiplier from CPU inactivity.
///
/// Returns a multiplier between 1.0 and 5.0:
/// - For idle times < 2 minutes: Linear interpolation from 1.0 to 2.0
/// - For idle times >= 2 minutes: Logarithmic scaling (1.0 + log2(minutes))
fn idle_multiplier(idle_for: Duration) -> f64 {
  let factor = match idle_for.as_secs() < 120 {
    // Less than 2 minutes.
    // Linear interpolation from 1.0 (at 0s) to 2.0 (at 120s)
    true => (idle_for.as_secs() as f64) / 120.0,

    // 2 minutes or more.
    // Logarithmic scaling: 1.0 + log2(minutes)
    false => {
      let idle_minutes = idle_for.as_secs() as f64 / 60.0;
      idle_minutes.log2()
    },
  };

  // Clamp the multiplier to avoid excessive delays.
  (1.0 + factor).clamp(1.0, 5.0)
}

fn compute_poll_delay(
  system: &System,
  last_polling_delay: Option<Duration>,
  last_cpu_activity: Instant,
  now: Instant,
) -> Duration {
  let mut delay = system.poll_delay_for_power_source();

  if system.is_cpu_idle(now) {
    let idle_for = now.saturating_duration_since(last_cpu_activity);

    if idle_for > Duration::from_secs(30) {
      let factor = idle_multiplier(idle_for);

      log::debug!(
        "CPU has been idle for {seconds} seconds (approx {minutes} minutes), \
         applying idle factor: {factor:.2}x",
        seconds = idle_for.as_secs(),
        minutes = idle_for.as_secs() / 60,
      );

      delay = Duration::from_secs_f64(delay.as_secs_f64() * factor);
    }
  }

  if let Some(volatility) = system.cpu_volatility(now)
    && (volatility.usage > 0.1
      || volatility
        .temperature
        .is_some_and(|temperature| temperature > 1.0))
  {
    delay = (delay / 2).max(Duration::from_secs(1));
  }

  let delay = match last_polling_delay {
    Some(last_delay) => {
      Duration::from_secs_f64(
        delay.as_secs_f64() * 0.3 + last_delay.as_secs_f64() * 0.7,
      )
    },
    None => delay,
  };

  Duration::from_secs_f64(delay.as_secs_f64().clamp(1.0, 30.0))
}

fn detect_performance_degradation(
  system: &System,
  failures: &RuntimeFailures,
  thermal_threshold: Option<u16>,
) -> Option<String> {
  if !failures.errors.is_empty() {
    return Some("policy-error".to_owned());
  }
  thermal_threshold
    .filter(|threshold| {
      system
        .cpu_temperatures
        .values()
        .copied()
        .max_by(f64::total_cmp)
        .is_some_and(|temperature| temperature >= f64::from(*threshold))
    })
    .map(|_| "high-operating-temperature".to_owned())
}

async fn wait_for_shutdown() -> anyhow::Result<()> {
  #[cfg(unix)]
  {
    let mut terminate =
      signal::unix::signal(signal::unix::SignalKind::terminate())
        .context("failed to listen for SIGTERM")?;
    tokio::select! {
      result = signal::ctrl_c() => result.context("failed to listen for Ctrl-C"),
      _ = terminate.recv() => Ok(()),
    }
  }

  #[cfg(not(unix))]
  signal::ctrl_c()
    .await
    .context("failed to listen for Ctrl-C")
}

async fn wait_for_reload() -> anyhow::Result<()> {
  #[cfg(unix)]
  {
    let mut reload = signal::unix::signal(signal::unix::SignalKind::hangup())
      .context("failed to listen for SIGHUP")?;
    reload.recv().await;
    Ok(())
  }

  #[cfg(not(unix))]
  future::pending().await
}

fn read_chassis_type() -> anyhow::Result<Option<String>> {
  let Some(chassis_type) = fs::read("/sys/class/dmi/id/chassis_type")? else {
    return Ok(None);
  };

  Ok(match chassis_type.trim() {
    "3" | "4" | "5" | "6" | "7" | "15" | "16" | "17" => {
      Some("desktop".to_owned())
    },
    "8" => Some("portable".to_owned()),
    "9" | "10" | "14" | "31" => Some("laptop".to_owned()),
    "11" => Some("handheld".to_owned()),
    "13" => Some("all-in-one".to_owned()),
    _ => None,
  })
}

fn detect_virtual_machine() -> anyhow::Result<bool> {
  const DMI_PATHS: &[&str] = &[
    "/sys/class/dmi/id/product_name",
    "/sys/class/dmi/id/sys_vendor",
    "/sys/class/dmi/id/board_vendor",
    "/sys/class/dmi/id/bios_vendor",
  ];
  const VIRTUAL_MARKERS: &[&str] = &[
    "bhyve",
    "bochs",
    "hyper-v",
    "kvm",
    "parallels",
    "qemu",
    "virtualbox",
    "vmware",
    "xen",
  ];

  for path in DMI_PATHS {
    let Some(value) = fs::read(path)? else {
      continue;
    };
    let value = value.to_lowercase();
    if VIRTUAL_MARKERS.iter().any(|marker| value.contains(marker)) {
      return Ok(true);
    }
  }

  if let Some(cpuinfo) = fs::read("/proc/cpuinfo")? {
    return Ok(
      cpuinfo
        .lines()
        .any(|line| line.starts_with("flags") && line.contains(" hypervisor")),
    );
  }

  Ok(false)
}

/// CPU controls discovered during the latest successful scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuCapabilities {
  pub number:            u32,
  pub policy:            Option<String>,
  pub frequency_control: bool,
  pub governors:         Vec<String>,
  pub epps:              Vec<String>,
  pub epbs:              Vec<String>,
}

#[derive(Debug)]
pub struct DaemonState {
  config:               String,
  system:               System,
  rule_count:           usize,
  profile:              profile::ProfileState,
  profile_state_path:   Option<PathBuf>,
  last_applied_rules:   Vec<String>,
  performance_degraded: Option<String>,
  error_count:          usize,
  latest_error:         Option<String>,
  recent_errors:        Vec<String>,
  degraded_conditions:  Vec<DegradedCondition>,
  applied_settings:     Vec<fs::AppliedSetting>,
}

impl DaemonState {
  pub(crate) fn new(config: String, rule_count: usize) -> Self {
    let profile_state_path = env::var_os("WATT_STATE_PATH").map(PathBuf::from);
    let mut profile = profile::ProfileState::new();
    if let Some(path) = &profile_state_path {
      match profile::ProfileState::load_preference(path) {
        Ok(Some(preference)) => {
          profile.set_preference(preference);
        },
        Ok(None) => {},
        Err(error) => log::warn!("failed to load active profile: {error:#}"),
      }
    }
    Self {
      config,
      system: System::default(),
      rule_count,
      profile,
      profile_state_path,
      last_applied_rules: Vec::new(),
      performance_degraded: None,
      error_count: 0,
      latest_error: None,
      recent_errors: Vec::new(),
      degraded_conditions: Vec::new(),
      applied_settings: Vec::new(),
    }
  }

  pub fn config(&self) -> &str {
    &self.config
  }

  fn set_config(&mut self, config: String, rule_count: usize) {
    self.config = config;
    self.rule_count = rule_count;
  }

  fn update_system(
    &mut self,
    system: &System,
    last_applied_rules: Vec<String>,
    performance_degraded: Option<String>,
    failures: &RuntimeFailures,
  ) {
    self.system = system.clone();
    self.last_applied_rules = last_applied_rules;
    self.performance_degraded = performance_degraded;
    self.error_count = failures.errors.len();
    self.latest_error = failures.latest_message();
    self.degraded_conditions = failures.conditions();
    let mut recent: Vec<_> = failures.errors.iter().collect();
    recent.sort_by(|(left, left_failure), (right, right_failure)| {
      right_failure
        .latest
        .cmp(&left_failure.latest)
        .then_with(|| right.cmp(left))
    });
    self.recent_errors = recent
      .into_iter()
      .map(|(message, _)| message.clone())
      .collect();
    self.applied_settings = fs::applied_settings();
  }

  pub fn active_profile(&self) -> profile::PowerProfile {
    self.profile.get_effective_profile()
  }

  pub fn set_active_profile(
    &mut self,
    profile: profile::PowerProfile,
  ) -> anyhow::Result<Vec<u32>> {
    if let Some(path) = &self.profile_state_path {
      profile::ProfileState::save_profile(path, profile)
        .context("failed to persist active profile")?;
    }
    Ok(self.profile.set_preference(profile))
  }

  pub fn profile_holds(&self) -> Vec<profile::ProfileHold> {
    self.profile.get_holds()
  }

  pub fn add_profile_hold(
    &mut self,
    profile: profile::PowerProfile,
    reason: String,
    application_id: String,
  ) -> u32 {
    self.profile.add_hold(profile, reason, application_id)
  }

  pub fn add_profile_hold_for(
    &mut self,
    profile: profile::PowerProfile,
    reason: String,
    application_id: String,
    owner: String,
  ) -> u32 {
    self
      .profile
      .add_hold_for(profile, reason, application_id, owner)
  }

  pub(crate) fn begin_profile_owner_check(
    &mut self,
    owner: &str,
  ) -> profile::OwnerCheck {
    self.profile.begin_owner_check(owner)
  }

  pub(crate) fn add_checked_profile_hold(
    &mut self,
    check: profile::OwnerCheck,
    profile: profile::PowerProfile,
    reason: String,
    application_id: String,
  ) -> Option<u32> {
    self
      .profile
      .add_checked_hold(check, profile, reason, application_id)
  }

  pub fn release_profile_hold(&mut self, cookie: u32) -> anyhow::Result<()> {
    self.profile.release_hold(cookie)
  }

  pub fn release_profile_hold_for(
    &mut self,
    cookie: u32,
    owner: &str,
  ) -> Result<(), profile::ReleaseHoldError> {
    self.profile.release_hold_for(cookie, owner)
  }

  pub fn release_profile_owner(&mut self, owner: &str) -> Vec<u32> {
    self.profile.release_owner(owner)
  }

  pub fn release_bus_holds(&mut self) {
    self.profile.release_bus_holds();
  }

  pub fn rule_count(&self) -> usize {
    self.rule_count
  }

  pub fn cpu_count(&self) -> usize {
    self.system.cpus.len()
  }

  pub fn cpu_policies(&self) -> Vec<Arc<Policy>> {
    self
      .system
      .cpus
      .iter()
      .filter_map(|cpu| cpu.policy.as_ref())
      .map(|policy| (&policy.path, policy))
      .collect::<BTreeMap<_, _>>()
      .into_values()
      .cloned()
      .collect()
  }

  pub fn cpu_capabilities(&self) -> Vec<CpuCapabilities> {
    let mut capabilities: Vec<_> = self
      .system
      .cpus
      .iter()
      .map(|cpu| {
        CpuCapabilities {
          number:            cpu.number,
          policy:            cpu
            .policy
            .as_ref()
            .map(|policy| policy.name.clone()),
          frequency_control: cpu.frequency_control,
          governors:         cpu.available_governors.clone(),
          epps:              cpu.available_epps.clone(),
          epbs:              cpu.available_epbs.clone(),
        }
      })
      .collect();
    capabilities.sort_unstable_by_key(|cpu| cpu.number);
    capabilities
  }

  pub fn boost_capabilities(&self) -> Option<cpu::BoostCapabilities> {
    cpu::Cpu::boost_capabilities(self.system.cpus.iter().map(AsRef::as_ref))
  }

  pub fn latest_cpu_log(&self) -> Option<CpuLog> {
    self.system.cpu_log.back().cloned()
  }

  pub fn is_discharging(&self) -> bool {
    self.system.is_discharging()
  }

  pub fn performance_degraded(&self) -> Option<&str> {
    self
      .performance_degraded
      .as_deref()
      .filter(|_| self.active_profile() == profile::PowerProfile::Performance)
  }

  pub fn last_applied_rules(&self) -> Vec<String> {
    self.last_applied_rules.clone()
  }

  pub fn error_count(&self) -> usize {
    self.error_count
  }

  pub fn latest_error(&self) -> Option<&str> {
    self.latest_error.as_deref()
  }

  pub fn recent_errors(&self) -> Vec<String> {
    self.recent_errors.clone()
  }

  pub fn degraded_conditions(&self) -> &[DegradedCondition] {
    &self.degraded_conditions
  }

  pub fn applied_settings(&self) -> Vec<fs::AppliedSetting> {
    self.applied_settings.clone()
  }
}

fn load_config(path: &Path) -> anyhow::Result<(config::DaemonConfig, String)> {
  let config = config::DaemonConfig::load_from(Some(path))?;
  let serialized = toml::to_string_pretty(&config)
    .context("failed to serialize daemon config")?;
  Ok((config, serialized))
}

fn reload_config(
  path: &Path,
  active: &mut config::DaemonConfig,
) -> anyhow::Result<String> {
  let (config, serialized) = load_config(path)?;
  #[cfg(feature = "metrics")]
  if config.metrics != active.metrics {
    bail!("changing metrics configuration requires restarting watt");
  }
  *active = config;
  Ok(serialized)
}

fn evaluate_rules(
  rules: &[config::Rule],
  state: &config::EvalState<'_, '_>,
  failures: &mut RuntimeFailures,
) -> (config::EvaluatedRule, Vec<String>) {
  let mut actions = config::EvaluatedRule::empty(state);
  let mut applied = Vec::new();
  for rule in rules.iter().rev() {
    let Some(Some(evaluated)) = failures.attempt(
      format!("rule '{}' (priority {})", rule.name, rule.priority),
      || rule.evaluate(state),
    ) else {
      continue;
    };
    actions.merge(evaluated);
    applied.push(rule.name.clone());
    if actions.complete() {
      break;
    }
  }
  (actions, applied)
}

fn apply_actions(
  actions: config::EvaluatedRule,
  runtime_failures: &mut RuntimeFailures,
  dma_latency: &mut cpu::DmaLatency,
) {
  let config::EvaluatedRule {
    cpu: cpu_deltas,
    cpu_global: cpu_global_delta,
    uncore: uncore_deltas,
    vm: vm_delta,
    disk: disk_deltas,
    disk_global: disk_global_delta,
    usb: usb_deltas,
    audio: audio_delta,
    gpu: gpu_deltas,
    power: power_deltas,
    platform_profile: power_platform_profile,
  } = actions;

  fs::begin_settings_iteration();
  runtime_failures.apply_cpu_deltas(&cpu_deltas);

  log::trace!("applying CPU deltas to {len} CPUs", len = cpu_deltas.len());

  runtime_failures.attempt("global CPU application", || {
    cpu_global_delta.apply(cpu_deltas.keys().map(|arc| &**arc), dma_latency)
  });

  log::trace!(
    "applying uncore deltas to {len} devices",
    len = uncore_deltas.len(),
  );

  for (uncore, delta) in uncore_deltas {
    runtime_failures
      .attempt(format!("uncore application for {uncore}"), || {
        delta.apply(&uncore)
      });
  }

  runtime_failures.attempt("VM application", || vm_delta.apply());

  log::trace!(
    "applying disk deltas to {len} devices",
    len = disk_deltas.len(),
  );
  for (disk, delta) in disk_deltas {
    runtime_failures.attempt(format!("disk application for {disk}"), || {
      delta.apply(&disk)
    });
  }
  runtime_failures
    .attempt("global disk application", || disk_global_delta.apply());

  log::trace!(
    "applying USB deltas to {len} devices",
    len = usb_deltas.len(),
  );
  for (device, delta) in usb_deltas {
    runtime_failures.attempt(format!("USB application for {device}"), || {
      delta.apply(&device)
    });
  }

  runtime_failures.attempt("audio application", || audio_delta.apply());

  log::trace!(
    "applying GPU deltas to {len} devices",
    len = gpu_deltas.len(),
  );
  for (gpu, delta) in gpu_deltas {
    runtime_failures
      .attempt(format!("GPU application for {gpu}"), || delta.apply(&gpu));
  }

  log::trace!(
    "applying power supply deltas to {len} devices",
    len = power_deltas.len(),
  );

  for (power, delta) in power_deltas {
    runtime_failures
      .attempt(format!("power supply application for {power}"), || {
        delta.apply(&mut (*power).clone())
      });
  }

  if let Some(platform_profile) = power_platform_profile {
    runtime_failures.attempt("platform profile application", || {
      power_supply::PowerSupply::set_platform_profile(&platform_profile)
    });
  }

  for failure in fs::restore_unmanaged_settings() {
    runtime_failures.record(
      format!("setting restoration for {}", failure.path.display()),
      anyhow::anyhow!(failure.message),
    );
  }
}

struct PollingState {
  last_polling_delay: Option<Duration>,
  last_cpu_activity:  Instant,
  system:             System,
  runtime_failures:   RuntimeFailures,
  dma_latency:        cpu::DmaLatency,
}

impl PollingState {
  fn new() -> Self {
    Self {
      last_polling_delay: None,
      last_cpu_activity:  Instant::now(),
      system:             System::default(),
      runtime_failures:   RuntimeFailures::default(),
      dma_latency:        cpu::DmaLatency::default(),
    }
  }

  async fn poll(
    &mut self,
    config: &config::DaemonConfig,
    state: &RwLock<DaemonState>,
    applied_rules_tx: &watch::Sender<Vec<String>>,
  ) -> Duration {
    log::debug!("starting main polling loop iteration");
    let start = Instant::now();

    self.runtime_failures.begin_iteration();
    self.system.scan(&mut self.runtime_failures);
    let now = Instant::now();

    if !self.system.is_cpu_idle(now) {
      self.last_cpu_activity = now;
    }

    let power_profile_preference = state.read().await.active_profile();
    let turbo_available = self
      .runtime_failures
      .attempt("turbo capability scan", || {
        cpu::Cpu::turbo(self.system.cpus.iter().map(AsRef::as_ref))
      })
      .flatten()
      .is_some();
    let eval_state = self.system.eval_state(
      now,
      self.last_cpu_activity,
      power_profile_preference,
      turbo_available,
    );

    let (actions, last_applied_rules) =
      evaluate_rules(&config.rules, &eval_state, &mut self.runtime_failures);
    apply_actions(actions, &mut self.runtime_failures, &mut self.dma_latency);

    let delay = compute_poll_delay(
      &self.system,
      self.last_polling_delay,
      self.last_cpu_activity,
      now,
    );
    self.last_polling_delay = Some(delay);
    self.runtime_failures.finish_iteration();
    let performance_degraded = detect_performance_degradation(
      &self.system,
      &self.runtime_failures,
      config.thermal.degradation_threshold_celsius,
    );
    state.write().await.update_system(
      &self.system,
      last_applied_rules.clone(),
      performance_degraded,
      &self.runtime_failures,
    );
    applied_rules_tx.send_if_modified(|rules| {
      let changed = *rules != last_applied_rules;
      if changed {
        *rules = last_applied_rules;
      }
      changed
    });

    let elapsed = start.elapsed();
    log::trace!(
      "filtered and applied rules in {seconds} seconds or {minutes} minutes",
      seconds = elapsed.as_secs_f64(),
      minutes = elapsed.as_secs_f64() / 60.0,
    );

    log::trace!(
      "next poll will be in {seconds} seconds or {minutes} minutes, possibly \
       delayed if application of rules takes more than the polling delay",
      seconds = delay.as_secs_f64(),
      minutes = delay.as_secs_f64() / 60.0,
    );

    delay.saturating_sub(elapsed)
  }
}

enum DaemonEvent {
  Shutdown,
  Reload,
  Poll,
}

async fn next_daemon_event(
  shutdown: Pin<&mut impl Future<Output = anyhow::Result<()>>>,
  reload: Pin<&mut impl Future<Output = anyhow::Result<()>>>,
  delay: Duration,
) -> anyhow::Result<DaemonEvent> {
  tokio::select! {
    result = shutdown => {
      result?;
      Ok(DaemonEvent::Shutdown)
    },
    result = reload => {
      result?;
      Ok(DaemonEvent::Reload)
    },
    () = sleep(delay) => Ok(DaemonEvent::Poll),
  }
}

async fn reload_daemon_config(
  path: Option<&Path>,
  config: &mut config::DaemonConfig,
  state: &RwLock<DaemonState>,
) {
  let Some(path) = path else {
    log::debug!("ignoring SIGHUP for the builtin config");
    return;
  };
  match reload_config(path, config) {
    Ok(serialized) => {
      state
        .write()
        .await
        .set_config(serialized, config.rules.len());
      log::info!("reloaded daemon config");
    },
    Err(error) => log::error!("failed to reload daemon config: {error:#}"),
  }
}

pub async fn run_daemon(
  config: config::DaemonConfig,
  config_path: Option<PathBuf>,
) -> anyhow::Result<()> {
  if !config.rules.is_sorted_by_key(|rule| rule.priority) {
    bail!("daemon config rules must be sorted by priority");
  }

  log::info!("starting daemon...");

  let serialized_config = toml::to_string_pretty(&config)
    .context("failed to serialize daemon config")?;
  let state = Arc::new(RwLock::new(DaemonState::new(
    serialized_config,
    config.rules.len(),
  )));
  let (applied_rules_tx, applied_rules_rx) = watch::channel(Vec::new());

  #[cfg(feature = "metrics")]
  if let Some(metrics_config) = &config.metrics {
    metrics::start(metrics_config, Arc::clone(&state))?;
  }

  tokio::spawn({
    let state = Arc::clone(&state);
    async move {
      if let Err(error) = server::start(state, applied_rules_rx).await {
        log::error!("D-Bus server exited with error: {error}");
      }
    }
  });

  run_polling_loop(config, config_path, state, applied_rules_tx).await
}

async fn run_polling_loop(
  mut config: config::DaemonConfig,
  config_path: Option<PathBuf>,
  state: Arc<RwLock<DaemonState>>,
  applied_rules_tx: watch::Sender<Vec<String>>,
) -> anyhow::Result<()> {
  let mut polling = PollingState::new();
  let _managed_settings = fs::manage_settings();
  let shutdown_signal = wait_for_shutdown();
  tokio::pin!(shutdown_signal);
  let reload_signal = wait_for_reload();
  tokio::pin!(reload_signal);
  let mut sleep_for = Duration::ZERO;

  loop {
    match next_daemon_event(
      shutdown_signal.as_mut(),
      reload_signal.as_mut(),
      sleep_for,
    )
    .await?
    {
      DaemonEvent::Shutdown => {
        log::info!("received shutdown signal");
        break;
      },
      DaemonEvent::Reload => {
        reload_signal.set(wait_for_reload());
        reload_daemon_config(config_path.as_deref(), &mut config, &state).await;
      },
      DaemonEvent::Poll => {},
    }
    sleep_for = polling.poll(&config, &state, &applied_rules_tx).await;
  }

  log::info!("stopping polling loop and shutting down");

  Ok(())
}

#[cfg(test)]
mod tests {
  use std::{
    env,
    fs as stdfs,
    io::ErrorKind,
    os::unix::fs::symlink,
    process,
  };

  use futures_util::StreamExt as _;
  use tokio::{
    task::yield_now,
    time::timeout,
  };
  use zbus::{
    Proxy,
    connection,
    fdo::DBusProxy,
    zvariant::OwnedValue,
  };

  use super::*;
  use crate::{
    audio,
    dbus::test_support::TestBus,
    profile::PowerProfile,
    test_support::SystemFixture,
  };

  #[test]
  fn thermal_threshold_reports_only_hot_systems() {
    let mut system = System::default();
    let failures = RuntimeFailures::default();
    assert_eq!(
      detect_performance_degradation(&system, &failures, Some(90)),
      None
    );
    system.cpu_temperatures.insert(0, 95.0);
    assert_eq!(
      detect_performance_degradation(&system, &failures, Some(90)).as_deref(),
      Some("high-operating-temperature")
    );
  }

  #[tokio::test]
  async fn boost_degradation_notifies_clients_and_recovers() {
    let fixture = SystemFixture::new();
    let first = fixture.policy(0, &[0], "amd-pstate-epp").join("boost");
    let second = fixture.policy(1, &[1], "amd-pstate-epp").join("boost");
    fixture.cpu_telemetry(&[0, 1]);
    fixture.write(&first, "0");
    fixture.write(&second, "0");
    let system = System {
      cpus: cpu::Cpu::all().unwrap().into_iter().map(Arc::new).collect(),
      ..System::default()
    };
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let connection = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let client = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let closing = connection.clone();
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      let (_updates, applied_rules) = watch::channel(Vec::new());
      let observe = async {
        wait_native_service(&client).await;
        let proxy = Proxy::new(
          &client,
          "dev.notashelf.Watt",
          "/dev/notashelf/Watt",
          "dev.notashelf.Watt",
        )
        .await
        .unwrap();
        let mut changes = proxy
          .receive_property_changed::<Vec<HashMap<String, OwnedValue>>>(
            "DegradedConditions",
          )
          .await;
        let mut counts =
          proxy.receive_property_changed::<u32>("ErrorCount").await;
        let mut errors = proxy
          .receive_property_changed::<String>("LatestError")
          .await;
        assert_eq!(proxy.get_property::<u32>("ErrorCount").await.unwrap(), 0);
        assert!(
          proxy
            .get_property::<String>("LatestError")
            .await
            .unwrap()
            .is_empty()
        );
        counts.next().await.unwrap();
        errors.next().await.unwrap();
        let ppd = Proxy::new(
          &client,
          "net.hadess.PowerProfiles",
          "/net/hadess/PowerProfiles",
          "net.hadess.PowerProfiles",
        )
        .await
        .unwrap();
        let mut ppd_changes = ppd
          .receive_property_changed::<String>("PerformanceDegraded")
          .await;
        let initial_ppd: String =
          ppd.get_property("PerformanceDegraded").await.unwrap();
        assert!(initial_ppd.is_empty());
        ppd_changes.next().await.unwrap();
        let initial: Vec<HashMap<String, OwnedValue>> =
          proxy.get_property("DegradedConditions").await.unwrap();
        assert!(initial.is_empty());
        // Consume the property cache's initial value before testing signals.
        changes.next().await.unwrap();
        let _settings = fs::manage_settings();
        let mut failures = RuntimeFailures::default();
        let mut dma_latency = cpu::DmaLatency::default();
        fixture.write_result(&first, Err(ErrorKind::PermissionDenied));
        for (expected_conditions, expected_reason) in
          [(1, "policy-error"), (0, "")]
        {
          failures.begin_iteration();
          let now = Instant::now();
          let eval =
            system.eval_state(now, now, PowerProfile::Performance, true);
          let mut actions = config::EvaluatedRule::empty(&eval);
          actions.cpu_global.turbo = Some(true);
          apply_actions(actions, &mut failures, &mut dma_latency);
          failures.finish_iteration();
          let degraded =
            detect_performance_degradation(&system, &failures, None);
          {
            let mut state = state.write().await;
            state.update_system(&system, Vec::new(), degraded, &failures);
            state.set_active_profile(PowerProfile::PowerSaver).unwrap();
            assert!(state.performance_degraded().is_none());
            state.set_active_profile(PowerProfile::Performance).unwrap();
          }
          assert_eq!(
            counts.next().await.unwrap().get().await.unwrap(),
            expected_conditions as u32
          );
          let latest = errors.next().await.unwrap().get().await.unwrap();
          assert_eq!(latest.is_empty(), expected_conditions == 0);
          assert_eq!(
            proxy.cached_property::<u32>("ErrorCount").unwrap(),
            Some(expected_conditions as u32)
          );
          assert_eq!(
            proxy.cached_property::<String>("LatestError").unwrap(),
            Some(latest)
          );
          let conditions = changes.next().await.unwrap().get().await.unwrap();
          assert_eq!(conditions.len(), expected_conditions);
          assert_eq!(
            ppd_changes.next().await.unwrap().get().await.unwrap(),
            expected_reason,
          );
          assert_eq!(stdfs::read_to_string(&second).unwrap(), "1");
          for condition in &conditions {
            assert_eq!(
              <&str>::try_from(&condition["context"]).unwrap(),
              "global CPU application"
            );
            let message = <&str>::try_from(&condition["message"]).unwrap();
            assert!(message.contains(first.to_str().unwrap()), "{message}");
          }
          fixture.clear_io(&first);
        }
        assert_eq!(stdfs::read_to_string(&first).unwrap(), "1");
        closing.close().await.unwrap();
      };
      let (result, ()) = tokio::join!(
        server::run_connection(connection, state.clone(), applied_rules),
        observe
      );
      assert!(result.is_err());
      client.close().await.unwrap();
    })
    .await
    .expect("boost degradation notification timed out");
  }

  async fn wait_native_service(connection: &zbus::Connection) {
    let bus = DBusProxy::new(connection).await.unwrap();
    while !bus
      .name_has_owner("dev.notashelf.Watt".try_into().unwrap())
      .await
      .unwrap()
    {
      yield_now().await;
    }
  }

  #[test]
  fn failed_profile_persistence_preserves_preference_and_holds() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("not-a-directory");
    stdfs::write(&parent, "occupied").unwrap();
    let mut state = DaemonState::new(String::new(), 0);
    state.profile_state_path = Some(parent.join("profile.toml"));
    state.profile.set_preference(PowerProfile::Performance);
    let cookie = state.add_profile_hold(
      PowerProfile::PowerSaver,
      "battery".into(),
      "test".into(),
    );

    assert!(state.set_active_profile(PowerProfile::Balanced).is_err());
    assert_eq!(state.active_profile(), PowerProfile::PowerSaver);
    state.release_profile_hold(cookie).unwrap();
    assert_eq!(state.active_profile(), PowerProfile::Performance);
    assert_eq!(stdfs::read_to_string(parent).unwrap(), "occupied");
  }

  #[test]
  fn renamed_gpu_and_uncore_aliases_keep_restoration_ownership() {
    let fixture = SystemFixture::new();
    let gpu = fixture.path("/sys/devices/gpu");
    let panel = gpu.join("amdgpu/panel_power_savings");
    fixture.write(&panel, "0");
    let uncore = fixture.path("/sys/devices/uncore");
    for (name, value) in [
      ("initial_min_freq_khz", "800000"),
      ("initial_max_freq_khz", "3000000"),
      ("min_freq_khz", "800000"),
      ("max_freq_khz", "3000000"),
    ] {
      fixture.write(uncore.join(name), value);
    }
    let gpu_alias = fixture.path("/sys/class/drm/card0");
    let uncore_alias = fixture
      .path("/sys/devices/system/cpu/intel_uncore_frequency/package_00_die_00");
    for (target, alias) in [(&gpu, &gpu_alias), (&uncore, &uncore_alias)] {
      stdfs::create_dir_all(alias.parent().unwrap()).unwrap();
      symlink(target, alias).unwrap();
    }
    let mut system = System::default();
    system.scan_gpus().unwrap();
    system.scan_uncores().unwrap();
    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    system
      .gpus
      .iter()
      .next()
      .unwrap()
      .set_panel_power_savings(2)
      .unwrap();
    system
      .uncores
      .iter()
      .next()
      .unwrap()
      .set_min_khz(1_000_000)
      .unwrap();
    stdfs::rename(&gpu_alias, gpu_alias.with_file_name("card1")).unwrap();
    stdfs::rename(
      &uncore_alias,
      uncore_alias.with_file_name("package_01_die_00"),
    )
    .unwrap();
    system.scan_gpus().unwrap();
    system.scan_uncores().unwrap();
    assert_eq!(fs::applied_settings().len(), 2);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(panel).unwrap(), "0");
    assert_eq!(
      stdfs::read_to_string(uncore.join("min_freq_khz")).unwrap(),
      "800000"
    );
  }

  #[test]
  fn renamed_battery_alias_keeps_threshold_restoration() {
    let fixture = SystemFixture::new();
    let battery = fixture.path("/sys/devices/battery");
    fixture.write(battery.join("type"), "Battery");
    fixture.write(battery.join("charge_control_start_threshold"), "40");
    let threshold = battery.join("charge_control_end_threshold");
    fixture.write(&threshold, "80");
    let alias = fixture.path("/sys/class/power_supply/BAT0");
    stdfs::create_dir_all(alias.parent().unwrap()).unwrap();
    symlink(&battery, &alias).unwrap();
    let mut supply = power_supply::PowerSupply::all().unwrap().pop().unwrap();
    assert_eq!(supply.name, "BAT0");
    let mut system = System::default();
    system.scan_power_supplies().unwrap();
    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    supply.set_charge_threshold_end(0.6).unwrap();
    stdfs::rename(&alias, alias.with_file_name("BAT1")).unwrap();
    system.scan_power_supplies().unwrap();
    assert_eq!(fs::applied_settings().len(), 1);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(threshold).unwrap(), "80");
  }

  #[test]
  fn partial_battery_scan_preserves_restoration_and_recovers() {
    let fixture = SystemFixture::new();
    let battery = fixture.path("/sys/class/power_supply/BAT0");
    fixture.write(battery.join("type"), "Battery");
    fixture.write(battery.join("charge_control_start_threshold"), "40");
    let threshold = battery.join("charge_control_end_threshold");
    fixture.write(&threshold, "80");
    fixture.write("/sys/class/power_supply/AC/type", "Mains");
    let mut system = System::default();
    system.scan_power_supplies().unwrap();
    let mut supply = system
      .power_supplies
      .iter()
      .find(|supply| supply.name == "BAT0")
      .unwrap()
      .as_ref()
      .clone();
    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    supply.set_charge_threshold_end(0.6).unwrap();
    fixture.fail_read(battery.join("type"), ErrorKind::PermissionDenied);
    let error = system.scan_power_supplies().unwrap_err();
    assert!(error.to_string().contains("BAT0"));
    assert_eq!(system.power_supplies.len(), 1);
    assert_eq!(system.power_supplies.iter().next().unwrap().name, "AC");
    assert_eq!(fs::applied_settings().len(), 1);
    fixture.clear_io(battery.join("type"));
    system.scan_power_supplies().unwrap();
    assert_eq!(system.power_supplies.len(), 2);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(threshold).unwrap(), "80");
  }

  #[test]
  fn shared_radeon_device_retains_ownership_after_one_card_disappears() {
    let fixture = SystemFixture::new();
    let device = fixture.path("/sys/devices/radeon");
    let method = device.join("power_method");
    fixture.write(&method, "profile");
    let drm = fixture.path("/sys/class/drm");
    for name in ["card0", "card1"] {
      let card = drm.join(name);
      stdfs::create_dir_all(&card).unwrap();
      symlink(&device, card.join("device")).unwrap();
    }
    let mut system = System::default();
    system.scan_gpus().unwrap();
    assert_eq!(system.gpus.len(), 2);
    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    for gpu in &system.gpus {
      gpu.set_radeon_powersave("dynpm").unwrap();
    }
    assert_eq!(fs::applied_settings().len(), 1);
    stdfs::remove_dir_all(drm.join("card0")).unwrap();
    system.scan_gpus().unwrap();
    assert_eq!(system.gpus.len(), 1);
    assert_eq!(fs::applied_settings().len(), 1);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(method).unwrap(), "profile");
  }

  #[test]
  fn shutdown_restores_audio_and_platform_controls() {
    let fixture = SystemFixture::new();
    let timeout =
      fixture.path("/sys/module/snd_hda_intel/parameters/power_save");
    let reset = fixture
      .path("/sys/module/snd_hda_intel/parameters/power_save_controller");
    let profile = fixture.path("/sys/firmware/acpi/platform_profile");
    fixture.write(&timeout, "0");
    fixture.write(&reset, "N");
    fixture.write(&profile, "balanced");
    fixture.write(
      "/sys/firmware/acpi/platform_profile_choices",
      "balanced performance",
    );
    {
      let _managed = fs::manage_settings();
      fs::begin_settings_iteration();
      audio::Delta {
        timeout_seconds:  Some(10),
        reset_controller: Some(true),
      }
      .apply()
      .unwrap();
      power_supply::PowerSupply::set_platform_profile("performance").unwrap();
      assert_eq!(stdfs::read_to_string(&timeout).unwrap(), "10");
      assert_eq!(stdfs::read_to_string(&reset).unwrap(), "Y");
      assert_eq!(stdfs::read_to_string(&profile).unwrap(), "performance");
    }
    assert_eq!(stdfs::read_to_string(timeout).unwrap(), "0");
    assert_eq!(stdfs::read_to_string(reset).unwrap(), "N");
    assert_eq!(stdfs::read_to_string(profile).unwrap(), "balanced");
    assert!(fs::applied_settings().is_empty());
  }

  #[test]
  fn failed_rule_does_not_override_lower_priority_actions_and_recovers() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0]);
    let system = System {
      cpus: cpu::Cpu::all().unwrap().into_iter().map(Arc::new).collect(),
      ..System::default()
    };
    let now = Instant::now();
    let eval = system.eval_state(now, now, PowerProfile::Balanced, false);
    let mut high: config::Rule = toml::from_str(
      "name='high'\npriority=2\ncpu.governor='performance'\naudio.\
       timeout-seconds='invalid'",
    )
    .unwrap();
    let low: config::Rule =
      toml::from_str("name='low'\npriority=1\ncpu.governor='powersave'")
        .unwrap();
    let mut failures = RuntimeFailures::default();
    for expected in ["powersave", "performance"] {
      failures.begin_iteration();
      let (actions, applied) =
        evaluate_rules(&[low.clone(), high.clone()], &eval, &mut failures);
      assert_eq!(applied.contains(&high.name), expected == "performance");
      apply_actions(actions, &mut failures, &mut cpu::DmaLatency::default());
      failures.finish_iteration();
      assert_eq!(
        stdfs::read_to_string(policy.join("scaling_governor")).unwrap(),
        expected
      );
      assert_eq!(failures.errors.is_empty(), expected == "performance");
      high.audio = Default::default();
    }
  }

  #[test]
  fn invalid_arithmetic_rule_leaves_lower_priority_rule_eligible() {
    let system = System::default();
    let now = Instant::now();
    let eval = system.eval_state(now, now, PowerProfile::Balanced, false);
    let low: config::Rule =
      toml::from_str("name='low'\npriority=1\ncpu.turbo=false").unwrap();
    for expression in [
      "{ value=1, divide=0 }",
      "{ value=0, divide=0 }",
      "{ value=1e308, multiply=1e308 }",
      "{ value=-1, power=0.5 }",
    ] {
      let high: config::Rule = toml::from_str(&format!(
        "name='high'\npriority=2\ncpu.turbo=true\naudio.\
         timeout-seconds={expression}",
      ))
      .unwrap();
      let mut failures = RuntimeFailures::default();
      let (actions, applied) =
        evaluate_rules(&[low.clone(), high], &eval, &mut failures);
      assert_eq!(actions.cpu_global.turbo, Some(false));
      assert_eq!(applied, vec!["low"]);
      assert_eq!(failures.errors.len(), 1);
      let message = failures.latest_message().unwrap();
      assert!(message.contains("non-finite"), "{expression}: {message}");
    }
  }

  #[test]
  fn reload_keeps_active_rules_until_the_candidate_is_valid() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let mut active = config::DaemonConfig::load_from(None).unwrap();
    let original = active.clone();
    for contents in [
      "invalid",
      "[[rule]]\nname='bad'\npriority=1\ncpu.frequency-mhz-minimum=-1\n",
    ] {
      stdfs::write(&path, contents).unwrap();
      assert!(reload_config(&path, &mut active).is_err());
      assert_eq!(active, original);
    }
    stdfs::write(&path, "[[rule]]\nname='replacement'\npriority=1\n").unwrap();
    let serialized = reload_config(&path, &mut active).unwrap();
    assert_eq!(active.rules.len(), 1);
    assert_eq!(active.rules[0].name, "replacement");
    assert_eq!(
      toml::from_str::<config::DaemonConfig>(&serialized).unwrap(),
      active
    );
  }

  #[tokio::test]
  async fn reload_restores_a_cpu_control_removed_from_the_rules() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0]);
    let governor = policy.join("scaling_governor");
    let path = fixture.path("config.toml");
    let _managed = fs::manage_settings();
    let mut active = config::DaemonConfig::load_from(None).unwrap();
    let state = RwLock::new(DaemonState::new(String::new(), 0));
    let mut polling = PollingState::new();
    let (applied_rules, _receiver) = watch::channel(Vec::new());

    for (action, expected) in [
      ("cpu.governor = 'performance'", "performance"),
      ("", "powersave"),
    ] {
      fixture.write(
        &path,
        format!("[[rule]]\nname='test'\npriority=1\n{action}\n"),
      );
      reload_daemon_config(Some(&path), &mut active, &state).await;
      polling.poll(&active, &state, &applied_rules).await;
      assert_eq!(state.read().await.rule_count(), 1);
      assert_eq!(stdfs::read_to_string(&governor).unwrap(), expected);
    }
    assert_eq!(fixture.write_count(&governor), 2);
    assert!(fs::applied_settings().is_empty());
  }

  #[cfg(feature = "metrics")]
  #[test]
  fn reload_rejects_metrics_listener_changes_without_replacing_rules() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let mut active = config::DaemonConfig::load_from(None).unwrap();
    let original = active.clone();
    stdfs::write(
      &path,
      r#"
[metrics]
listen-addr = "127.0.0.1"
port = 9999
[[rule]]
name = "replacement"
priority = 1
"#,
    )
    .unwrap();
    let error = reload_config(&path, &mut active).unwrap_err();
    assert!(error.to_string().contains("requires restarting"));
    assert_eq!(active, original);
  }

  #[test]
  fn shared_policy_failure_does_not_repeat_or_skip_local_controls() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0, 1], "intel_pstate");
    fixture.cpu_telemetry(&[0, 1]);
    for number in [0, 1] {
      fixture.write(
        format!(
          "/sys/devices/system/cpu/cpu{number}/power/pm_qos_resume_latency_us"
        ),
        "0",
      );
    }
    let governor = policy.join("scaling_governor");
    fixture.write_result(&governor, Err(ErrorKind::PermissionDenied));
    let deltas = cpu::Cpu::all()
      .unwrap()
      .into_iter()
      .map(|cpu| {
        (Arc::new(cpu), cpu::Delta {
          governor: Some("performance".into()),
          pm_qos_resume_latency_us: Some("100".into()),
          ..cpu::Delta::default()
        })
      })
      .collect();
    let mut failures = RuntimeFailures::default();
    failures.apply_cpu_deltas(&deltas);
    assert_eq!(fixture.write_count(&governor), 1);
    assert_eq!(failures.errors.len(), 1);
    for number in [0, 1] {
      let path = fixture.path(format!(
        "/sys/devices/system/cpu/cpu{number}/power/pm_qos_resume_latency_us"
      ));
      assert_eq!(stdfs::read_to_string(path).unwrap(), "100");
    }
    fixture.clear_io(&governor);
    failures.begin_iteration();
    failures.apply_cpu_deltas(&deltas);
    failures.finish_iteration();
    assert!(failures.errors.is_empty());
    assert_eq!(fixture.write_count(&governor), 1);
    assert_eq!(stdfs::read_to_string(governor).unwrap(), "performance");
  }

  #[test]
  fn a_failed_scan_keeps_other_scan_failures_local() {
    let root =
      env::temp_dir().join(format!("watt-empty-system-{}", process::id(),));
    stdfs::create_dir_all(&root).unwrap();
    let _root = fs::set_system_root_for_tests(&root);
    let mut system = System::default();
    let mut failures = RuntimeFailures::default();

    failures.begin_iteration();
    system.scan(&mut failures);

    assert!(failures.errors.len() > 1);
    assert!(system.cpu_log.is_empty());

    drop(_root);
    stdfs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_failed_operation_does_not_skip_the_next_operation() {
    let mut failures = RuntimeFailures::default();
    let mut applied = false;

    failures.begin_iteration();
    assert!(
      failures
        .attempt("first operation", || {
          Err::<(), _>(anyhow::anyhow!("rejected"))
        })
        .is_none()
    );
    assert!(
      failures
        .attempt("second operation", || {
          applied = true;
          Ok(())
        })
        .is_some()
    );
    failures.finish_iteration();

    assert!(applied);
    assert_eq!(failures.errors.len(), 1);
  }

  #[test]
  fn daemon_state_retains_recent_errors_in_latest_first_order() {
    let mut failures = RuntimeFailures::default();
    failures.begin_iteration();
    failures.record("first", anyhow::anyhow!("one"));
    failures.record("second", anyhow::anyhow!("two"));
    let mut state = DaemonState::new(String::new(), 0);
    state.update_system(&System::default(), Vec::new(), None, &failures);
    assert_eq!(state.recent_errors().len(), 2);
    assert!(state.recent_errors()[0].contains("second"));
    assert!(state.recent_errors()[1].contains("first"));
  }

  #[test]
  fn runtime_failures_keep_a_bounded_latest_summary() {
    let mut failures = RuntimeFailures::default();

    failures.begin_iteration();
    failures.record("CPU 0 governor", anyhow::anyhow!("rejected"));
    failures.record("CPU 0 governor", anyhow::anyhow!("rejected"));

    assert_eq!(failures.errors.len(), 1);
    assert_eq!(
      failures.latest_message(),
      Some("CPU 0 governor: rejected (2 occurrence(s), active for 0s)".into())
    );
  }

  #[test]
  fn rescans_reappearing_cpus_without_stale_capabilities() {
    let fixture = SystemFixture::new();
    let mut system = System::default();
    let mut failures = RuntimeFailures::default();

    fixture.policy(0, &[0], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0]);
    failures.begin_iteration();
    system.scan(&mut failures);
    assert_eq!(system.cpus.len(), 1);

    stdfs::remove_dir_all(fixture.path("/sys/devices/system/cpu/cpu0"))
      .unwrap();
    failures.begin_iteration();
    system.scan(&mut failures);
    assert!(system.cpus.is_empty());

    let policy = fixture.policy(0, &[0], "acpi-cpufreq");
    fixture.write(policy.join("scaling_governor"), "performance");
    failures.begin_iteration();
    system.scan(&mut failures);
    assert_eq!(system.cpus.len(), 1);
    assert_eq!(
      system.cpus.iter().next().unwrap().governor.as_deref(),
      Some("performance"),
    );
  }

  #[test]
  fn policy_ownership_survives_one_member_but_not_policy_removal() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0, 1], "intel_pstate");
    fixture.cpu_telemetry(&[0, 1]);
    let _settings = fs::manage_settings();
    fs::begin_settings_iteration();
    let mut system = System::default();
    system.scan_cpus().unwrap();
    let mut cpu = (**system.cpus.iter().next().unwrap()).clone();
    cpu.set_governor("performance").unwrap();
    stdfs::remove_dir_all(fixture.path("/sys/devices/system/cpu/cpu0"))
      .unwrap();
    system.scan_cpus().unwrap();
    assert_eq!(fs::applied_settings().len(), 1);
    stdfs::remove_dir_all(fixture.path("/sys/devices/system/cpu/cpu1"))
      .unwrap();
    system.scan_cpus().unwrap();
    assert!(fs::applied_settings().is_empty());

    fixture.policy(0, &[0], "intel_pstate");
    fixture.write(policy.join("scaling_governor"), "schedutil");
    system.scan_cpus().unwrap();
    let mut cpu = (**system.cpus.iter().next().unwrap()).clone();
    cpu.set_governor("performance").unwrap();
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(
      stdfs::read_to_string(policy.join("scaling_governor")).unwrap(),
      "schedutil"
    );
  }

  #[test]
  fn partial_cpu_scan_preserves_original_settings_for_restoration() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0], "acpi-cpufreq");
    fixture.policy(1, &[1], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0, 1]);
    let governor = policy.join("scaling_governor");
    fixture.write(&governor, "powersave");
    let latency = "/sys/devices/system/cpu/cpu0/power/pm_qos_resume_latency_us";
    fixture.write(latency, "0");
    let _settings = fs::manage_settings();
    fs::begin_settings_iteration();
    let mut system = System::default();
    system.scan_cpus().unwrap();
    fs::write(&governor, "performance").unwrap();
    fs::observe(&governor, "performance");
    fs::write(latency, "100").unwrap();
    fs::observe(latency, "100");

    fixture.fail_read(&governor, ErrorKind::PermissionDenied);
    let mut failures = RuntimeFailures::default();
    system.scan_component(&mut failures, "CPU scan", System::scan_cpus);
    let mut state = DaemonState::new(String::new(), 0);
    state.update_system(&system, Vec::new(), None, &failures);
    assert_eq!(state.error_count(), 1);
    assert!(state.latest_error().unwrap().contains("cpu0"));
    assert_eq!(system.cpus.len(), 1);
    fixture.clear_io(&governor);
    failures.begin_iteration();
    system.scan_component(&mut failures, "CPU scan", System::scan_cpus);
    failures.finish_iteration();
    state.update_system(&system, Vec::new(), None, &failures);
    assert_eq!(state.error_count(), 0);
    assert_eq!(state.cpu_count(), 2);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(governor).unwrap(), "powersave");
    assert_eq!(fs::read(latency).unwrap().as_deref(), Some("0"));
  }

  #[test]
  fn status_lists_shared_policies_once() {
    let fixture = SystemFixture::new();
    fixture.policy(0, &[0, 1], "intel_pstate");
    fixture.policy(2, &[2], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0, 1, 2]);
    let mut state = DaemonState::new(String::new(), 0);
    state.system.scan_cpus().unwrap();
    let policies = state.cpu_policies();
    assert_eq!(policies.len(), 2);
    assert_eq!(policies[0].name, "policy0");
    assert_eq!(policies[0].related_cpus, [0, 1]);
    assert_eq!(policies[1].name, "policy2");
    assert_eq!(policies[1].related_cpus, [2]);
  }

  #[test]
  fn capabilities_include_per_cpu_controls_without_cpufreq() {
    let fixture = SystemFixture::new();
    let policy = fixture.policy(0, &[0, 1], "acpi-cpufreq");
    fixture.cpu_telemetry(&[0, 1, 2]);
    fixture.write(
      policy.join("scaling_available_governors"),
      "powersave performance",
    );
    fixture.write("/sys/devices/system/cpu/cpu2/power/energy_perf_bias", "6");
    fixture.write(
      policy.join("energy_performance_preference"),
      "balance_power",
    );
    fixture.write(
      policy.join("energy_performance_available_preferences"),
      "performance balance_power power",
    );
    let mut state = DaemonState::new(String::new(), 0);
    state.system.scan_cpus().unwrap();
    let cpus = state.cpu_capabilities();
    assert_eq!(cpus.iter().map(|cpu| cpu.number).collect::<Vec<_>>(), [
      0, 1, 2
    ]);
    assert_eq!(cpus[0].policy, cpus[1].policy);
    assert_eq!(cpus[0].governors, ["powersave", "performance"]);
    assert_eq!(cpus[0].governors, cpus[1].governors);
    assert_eq!(cpus[0].epps, ["performance", "balance_power", "power"]);
    assert_eq!(cpus[0].epps, cpus[1].epps);
    assert!(cpus[2].policy.is_none());
    assert!(!cpus[2].frequency_control);
    assert!(cpus[2].governors.is_empty());
    assert!(!cpus[2].epbs.is_empty());
  }

  fn apply_rules(system: &System, rules: &[config::Rule]) {
    let now = Instant::now();
    let eval = system.eval_state(now, now, PowerProfile::Balanced, false);
    let mut failures = RuntimeFailures::default();
    failures.begin_iteration();
    let (actions, _) = evaluate_rules(rules, &eval, &mut failures);
    apply_actions(actions, &mut failures, &mut cpu::DmaLatency::default());
    failures.finish_iteration();
  }

  #[test]
  fn repeated_partial_inventory_keeps_original_until_rediscovery() {
    let fixture = SystemFixture::new();
    let root = fixture.path("/sys/block/sda");
    fixture.write(root.join("queue/scheduler"), "mq-deadline");
    fixture.write(root.join("removable"), "0");
    let mut system = System::default();
    system.scan_disks().unwrap();
    let _managed = fs::manage_settings();
    let rule: config::Rule =
      toml::from_str("name='disk-owner'\npriority=5\ndisk.scheduler='none'")
        .unwrap();
    apply_rules(&system, &[rule]);
    fixture.fail_read(root.join("removable"), ErrorKind::PermissionDenied);
    for _ in 0..2 {
      assert!(system.scan_disks().is_err());
      apply_rules(&system, &[]);
      assert_eq!(
        stdfs::read_to_string(root.join("queue/scheduler")).unwrap(),
        "none"
      );
    }
    fixture.clear_io(root.join("removable"));
    system.scan_disks().unwrap();
    apply_rules(&system, &[]);
    assert_eq!(
      stdfs::read_to_string(root.join("queue/scheduler")).unwrap(),
      "mq-deadline"
    );
    assert!(fs::applied_settings().is_empty());
  }

  #[test]
  fn thermal_fallback_clears_readings_when_sensor_inventory_disappears() {
    let fixture = SystemFixture::new();
    stdfs::create_dir_all(fixture.path("/sys/class/hwmon")).unwrap();
    let root = fixture.path("/sys/devices/virtual/thermal/thermal_zone0");
    fixture.write(root.join("type"), "x86_pkg_temp");
    fixture.write(root.join("temp"), "53300");
    let mut system = System::default();
    system.scan_temperatures().unwrap();
    assert_eq!(
      system
        .cpu_temperatures
        .values()
        .copied()
        .collect::<Vec<_>>(),
      [53.3]
    );
    stdfs::remove_dir_all(fixture.path("/sys/devices/virtual/thermal"))
      .unwrap();
    system.scan_temperatures().unwrap();
    assert!(system.cpu_temperatures.is_empty());
  }
}

#[cfg(test)] mod telemetry_tests;
