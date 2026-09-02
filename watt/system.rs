use std::{
  collections::{
    HashMap,
    HashSet,
    VecDeque,
  },
  mem,
  path::Path,
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
};

use crate::{
  audio,
  config,
  cpu,
  disk,
  fs,
  gpu,
  power_supply,
  profile,
  uncore,
  usb,
  vm,
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

#[derive(Default, Debug)]
struct RuntimeFailures {
  errors: HashMap<String, RuntimeFailure>,
  seen:   HashSet<String>,
}

#[derive(Debug, Clone)]
struct RuntimeFailure {
  first:       Instant,
  latest:      Instant,
  occurrences: u64,
}

impl RuntimeFailures {
  fn begin_iteration(&mut self) {
    self.seen.clear();
  }

  fn record(&mut self, context: impl Into<String>, error: anyhow::Error) {
    const MAX_FAILURES: usize = 32;

    let context = context.into();
    let message = error.to_string();
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
  previous: Vec<std::path::PathBuf>,
  current: &HashSet<Arc<T>>,
  path: impl Fn(&T) -> &Path,
) {
  for previous_path in previous {
    if !current.iter().any(|device| path(device) == previous_path) {
      fs::retire_settings_under(previous_path);
    }
  }
}

impl System {
  fn scan(&mut self, failures: &mut RuntimeFailures) {
    log::info!("scanning view of system hardware...");

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
    let cpus = cpu::Cpu::all()?
      .into_iter()
      .map(|mut cpu| {
        cpu.previous_stat = previous_stats.get(&cpu.number).cloned();
        Arc::from(cpu)
      })
      .collect::<HashSet<_>>();
    for number in previous_stats.keys() {
      if !cpus.iter().any(|cpu| cpu.number == *number) {
        fs::retire_settings_under(format!(
          "/sys/devices/system/cpu/cpu{number}",
        ));
      }
    }
    self.cpus = cpus;
    Ok(())
  }

  fn scan_power_supplies(&mut self) -> anyhow::Result<()> {
    let previous = self
      .power_supplies
      .iter()
      .map(|power_supply| power_supply.path.clone())
      .collect::<Vec<_>>();
    let power_supplies = power_supply::PowerSupply::all()?
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices(previous, &power_supplies, |power_supply| {
      &power_supply.path
    });
    self.power_supplies = power_supplies;
    Ok(())
  }

  fn scan_uncores(&mut self) -> anyhow::Result<()> {
    let previous = self
      .uncores
      .iter()
      .map(|uncore| uncore.path.clone())
      .collect::<Vec<_>>();
    let uncores = uncore::Uncore::all()?
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices(previous, &uncores, |uncore| &uncore.path);
    self.uncores = uncores;
    Ok(())
  }

  fn scan_disks(&mut self) -> anyhow::Result<()> {
    let previous = self
      .disks
      .iter()
      .map(|disk| disk.path.clone())
      .collect::<Vec<_>>();
    let disks = disk::Disk::all()?
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices(previous, &disks, |disk| &disk.path);
    self.disks = disks;
    Ok(())
  }

  fn scan_usb_devices(&mut self) -> anyhow::Result<()> {
    let previous = self
      .usb_devices
      .iter()
      .map(|device| device.path.clone())
      .collect::<Vec<_>>();
    let usb_devices = usb::UsbDevice::all()?
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices(previous, &usb_devices, |device| &device.path);
    self.usb_devices = usb_devices;
    Ok(())
  }

  fn scan_gpus(&mut self) -> anyhow::Result<()> {
    let previous = self
      .gpus
      .iter()
      .map(|gpu| gpu.path.clone())
      .collect::<Vec<_>>();
    let gpus = gpu::Gpu::all()?
      .into_iter()
      .map(Arc::from)
      .collect::<HashSet<_>>();
    retire_missing_devices(previous, &gpus, |gpu| &gpu.path);
    self.gpus = gpus;
    Ok(())
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

    const PATH: &str = "/sys/class/hwmon";

    let mut temperatures = HashMap::new();

    for entry in fs::read_dir(PATH)
      .context("failed to read hardware information")?
      .with_context(|| format!("'{PATH}' doesn't exist, are you on linux?"))?
    {
      let entry =
        entry.with_context(|| format!("failed to read entry of '{PATH}'"))?;

      let entry_path = entry.path();

      let Some(name) =
        fs::read(entry_path.join("name")).with_context(|| {
          format!(
            "failed to read name of hardware entry at '{path}'",
            path = entry_path.display(),
          )
        })?
      else {
        continue;
      };

      match &*name {
        // TODO: 'zenergy' can also report those stats, I think?
        "coretemp" | "k10temp" | "zenpower" | "amdgpu" => {
          Self::get_temperatures(&entry_path, &mut temperatures)?;
        },

        // Other CPU temperature drivers.
        _ if name.contains("cpu") || name.contains("temp") => {
          Self::get_temperatures(&entry_path, &mut temperatures)?;
        },

        _ => {},
      }
    }

    if temperatures.is_empty() {
      const PATH: &str = "/sys/devices/virtual/thermal";

      log::warn!(
        "failed to get CPU temperature information by using hwmon, falling \
         back to '{PATH}'"
      );

      let Some(thermal_zones) =
        fs::read_dir(PATH).context("failed to read thermal information")?
      else {
        return Ok(());
      };

      let mut counter = 0;

      for entry in thermal_zones {
        let entry =
          entry.with_context(|| format!("failed to read entry of '{PATH}'"))?;

        let entry_path = entry.path();

        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy();

        if !entry_name.starts_with("thermal_zone") {
          continue;
        }

        let Some(entry_type) =
          fs::read(entry_path.join("type")).with_context(|| {
            format!(
              "failed to read type of zone at '{path}'",
              path = entry_path.display(),
            )
          })?
        else {
          continue;
        };

        if !entry_type.contains("cpu")
          && !entry_type.contains("x86")
          && !entry_type.contains("core")
        {
          continue;
        }

        let Some(temperature_mc) = fs::read_n::<i64>(entry_path.join("temp"))
          .with_context(|| {
          format!(
            "failed to read temperature of zone at '{path}'",
            path = entry_path.display(),
          )
        })?
        else {
          continue;
        };

        // Magic value to see that it is from the thermal zones.
        temperatures.insert(777 + counter, temperature_mc as f64 / 1000.0);
        counter += 1;
      }
    }

    self.cpu_temperatures = temperatures;

    Ok(())
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

  fn cpu_volatility(&self) -> Option<CpuVolatility> {
    let recent_log_count = self
      .cpu_log
      .iter()
      .rev()
      .take_while(|log| log.at.elapsed() < Duration::from_secs(5 * 60))
      .count();

    if recent_log_count < 2 {
      return None;
    }

    if self.cpu_log.len() < 2 {
      return None;
    }

    let change_count = self.cpu_log.len() - 1;

    let mut usage_change_sum = 0.0;
    let mut temperature_change_sum = 0.0;
    let mut temperature_change_count = 0;

    for index in 0..change_count {
      let usage_change =
        self.cpu_log[index + 1].usage - self.cpu_log[index].usage;
      usage_change_sum += usage_change.abs();

      if let (Some(t1), Some(t2)) = (
        self.cpu_log[index].temperature,
        self.cpu_log[index + 1].temperature,
      ) {
        temperature_change_sum += (t2 - t1).abs();
        temperature_change_count += 1;
      }
    }

    Some(CpuVolatility {
      usage:       usage_change_sum / change_count as f64,
      temperature: (temperature_change_count > 0)
        .then(|| temperature_change_sum / temperature_change_count as f64),
    })
  }

  fn is_cpu_idle(&self) -> bool {
    let recent_log_count = self
      .cpu_log
      .iter()
      .rev()
      .take_while(|log| log.at.elapsed() < Duration::from_secs(5 * 60))
      .count();

    if recent_log_count < 2 {
      return false;
    }

    let recent_average = self
      .cpu_log
      .iter()
      .rev()
      .take(recent_log_count)
      .map(|log| log.usage)
      .sum::<f64>()
      / recent_log_count as f64;

    recent_average < 0.1
      && self
        .cpu_volatility()
        .is_none_or(|volatility| volatility.usage < 0.05)
  }

  fn is_discharging(&self) -> bool {
    self.power_supplies.iter().any(|power_supply| {
      power_supply.charge_state.as_deref() == Some("Discharging")
    })
  }

  /// Calculates the discharge rate, returns a number between 0 and 1.
  ///
  /// The discharge rate is averaged per hour.
  /// So a return value of Some(0.3) means the battery has been
  /// discharging 30% per hour.
  fn power_supply_discharge_rate(&self) -> Option<f64> {
    log::trace!("calculating power supply discharge rate");

    let mut last_charge = None;

    // A list of increasing charge percentages.
    let discharging: Vec<&PowerSupplyLog> = self
      .power_supply_log
      .iter()
      .rev()
      .take_while(move |log| {
        let Some(last_charge_value) = last_charge else {
          last_charge = Some(log.charge);
          return true;
        };

        last_charge = Some(log.charge);

        log.charge > last_charge_value
      })
      .collect();

    if discharging.len() < 2 {
      return None;
    }

    // Start of discharging. Has the most charge.
    let start = discharging.last()?;
    // End of discharging, very close to now. Has the least charge.
    let end = discharging.first()?;

    let discharging_duration_seconds = (start.at - end.at).as_secs_f64();
    let discharging_duration_hours = discharging_duration_seconds / 60.0 / 60.0;
    let discharged = start.charge - end.charge;

    Some(discharged / discharging_duration_hours)
  }
}

/// Calculate the idle time multiplier based on system idle time.
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
  last_user_activity: Instant,
) -> Duration {
  let mut delay = Duration::from_secs(5);

  if system.is_discharging() {
    match system.power_supply_discharge_rate() {
      Some(discharge_rate) if discharge_rate > 0.2 => delay *= 3,
      Some(discharge_rate) if discharge_rate > 0.1 => delay *= 2,
      Some(_) => {
        delay /= 2;
        delay *= 3;
      },
      None => delay *= 2,
    }
  }

  if system.is_cpu_idle() {
    let idle_for = last_user_activity.elapsed();

    if idle_for > Duration::from_secs(30) {
      let factor = idle_multiplier(idle_for);

      log::debug!(
        "system has been idle for {seconds} seconds (approx {minutes} \
         minutes), applying idle factor: {factor:.2}x",
        seconds = idle_for.as_secs(),
        minutes = idle_for.as_secs() / 60,
      );

      delay = Duration::from_secs_f64(delay.as_secs_f64() * factor);
    }
  }

  if let Some(volatility) = system.cpu_volatility()
    && (volatility.usage > 0.1
      || volatility
        .temperature
        .is_some_and(|temperature| temperature > 0.02))
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
  _system: &System,
  failures: &RuntimeFailures,
) -> Option<String> {
  (!failures.errors.is_empty())
    .then(|| "Watt could not apply one or more requested settings".to_owned())
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

#[derive(Debug)]
pub struct DaemonState {
  config:               String,
  system:               System,
  rule_count:           usize,
  profile:              profile::ProfileState,
  last_applied_rules:   Vec<String>,
  performance_degraded: Option<String>,
  error_count:          usize,
  latest_error:         Option<String>,
}

impl DaemonState {
  fn new(config: String, rule_count: usize) -> Self {
    Self {
      config,
      system: System::default(),
      rule_count,
      profile: profile::ProfileState::new(),
      last_applied_rules: Vec::new(),
      performance_degraded: None,
      error_count: 0,
      latest_error: None,
    }
  }

  pub fn config(&self) -> &str {
    &self.config
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
  }

  pub fn active_profile(&self) -> profile::PowerProfile {
    self.profile.get_effective_profile()
  }

  pub fn set_active_profile(&mut self, profile: profile::PowerProfile) {
    self.profile.set_preference(profile);
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

  pub fn release_profile_hold(&mut self, cookie: u32) -> anyhow::Result<()> {
    self.profile.release_hold(cookie)
  }

  pub fn rule_count(&self) -> usize {
    self.rule_count
  }

  pub fn cpu_count(&self) -> usize {
    self.system.cpus.len()
  }

  pub fn latest_cpu_log(&self) -> Option<CpuLog> {
    self.system.cpu_log.back().cloned()
  }

  pub fn is_discharging(&self) -> bool {
    self.system.is_discharging()
  }

  pub fn performance_degraded(&self) -> Option<&str> {
    self.performance_degraded.as_deref()
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
}

pub async fn run_daemon(config: config::DaemonConfig) -> anyhow::Result<()> {
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
    crate::metrics::start(metrics_config, Arc::clone(&state))?;
  }

  tokio::spawn({
    let state = Arc::clone(&state);
    async move {
      if let Err(error) =
        crate::dbus::server::start(state, applied_rules_rx).await
      {
        log::error!("D-Bus server exited with error: {error}");
      }
    }
  });

  let mut last_polling_delay = None::<Duration>;
  let mut last_user_activity = Instant::now();
  let mut system = System::default();
  let mut runtime_failures = RuntimeFailures::default();
  let mut dma_latency = cpu::DmaLatency::default();
  let _managed_settings = fs::manage_settings();
  let shutdown_signal = signal::ctrl_c();
  tokio::pin!(shutdown_signal);
  let mut sleep_for = Duration::ZERO;

  loop {
    tokio::select! {
      result = &mut shutdown_signal => {
        result.context("failed to listen for shutdown signal")?;
        log::info!("received shutdown signal");
        break;
      },
      () = tokio::time::sleep(sleep_for) => {},
    }

    log::debug!("starting main polling loop iteration");
    let start = Instant::now();

    runtime_failures.begin_iteration();
    system.scan(&mut runtime_failures);

    if !system.is_cpu_idle() {
      last_user_activity = Instant::now();
    }

    let power_profile_preference = state.read().await.active_profile();
    let delay = {
      let eval_state = config::EvalState {
        frequency_available: system
          .cpus
          .iter()
          .any(|cpu| cpu.frequency_available()),
        turbo_available: runtime_failures
          .attempt("turbo capability scan", cpu::Cpu::turbo)
          .flatten()
          .is_some(),

        cpu_usage: system.cpu_log.back().map_or(0.0, |log| log.usage),
        cpu_usage_volatility: system.cpu_volatility().map(|vol| vol.usage),
        cpu_temperature: system.cpu_log.back().and_then(|log| log.temperature),
        cpu_temperature_volatility: system
          .cpu_volatility()
          .and_then(|vol| vol.temperature),
        cpu_idle_seconds: last_user_activity.elapsed().as_secs_f64(),
        cpu_frequency_maximum: system
          .cpus
          .iter()
          .filter_map(|cpu| cpu.frequency_maximum)
          .max()
          .map(|frequency| frequency.as_mhz()),
        cpu_frequency_minimum: system
          .cpus
          .iter()
          .filter_map(|cpu| cpu.frequency_minimum)
          .min()
          .map(|frequency| frequency.as_mhz()),

        lid_closed: system.lid_closed,
        virtual_machine: system.virtual_machine,
        chassis_type: system.chassis_type.as_deref(),

        power_supply_charge: system
          .power_supply_log
          .back()
          .map(|log| log.charge),
        power_supply_discharge_rate: system.power_supply_discharge_rate(),

        battery_cycles: system.battery_cycles,
        battery_health: system.battery_health,

        discharging: system.is_discharging(),
        power_profile_preference,

        context: config::EvalContext::WidestPossible,

        cpus: &system.cpus,
        uncores: &system.uncores,
        disks: &system.disks,
        usb_devices: &system.usb_devices,
        gpus: &system.gpus,
        power_supplies: &system.power_supplies,
        cpu_log: &system.cpu_log,
      };

      let mut cpu_deltas: HashMap<Arc<cpu::Cpu>, cpu::Delta> = system
        .cpus
        .iter()
        .map(|cpu| (Arc::clone(cpu), cpu::Delta::default()))
        .collect();
      let mut cpu_global_delta = cpu::GlobalDelta::default();

      let mut uncore_deltas: HashMap<Arc<uncore::Uncore>, uncore::Delta> =
        system
          .uncores
          .iter()
          .map(|uncore| (Arc::clone(uncore), uncore::Delta::default()))
          .collect();
      let mut vm_delta = vm::Delta::default();
      let mut disk_deltas: HashMap<Arc<disk::Disk>, disk::Delta> = system
        .disks
        .iter()
        .map(|disk| (Arc::clone(disk), disk::Delta::default()))
        .collect();
      let mut disk_global_delta = disk::GlobalDelta::default();
      let mut usb_deltas: HashMap<Arc<usb::UsbDevice>, usb::Delta> = system
        .usb_devices
        .iter()
        .map(|device| (Arc::clone(device), usb::Delta::default()))
        .collect();
      let mut audio_delta = audio::Delta::default();
      let mut gpu_deltas: HashMap<Arc<gpu::Gpu>, gpu::Delta> = system
        .gpus
        .iter()
        .map(|gpu| (Arc::clone(gpu), gpu::Delta::default()))
        .collect();

      let mut power_deltas: HashMap<
        Arc<power_supply::PowerSupply>,
        power_supply::Delta,
      > = system
        .power_supplies
        .iter()
        .map(|power_supply| {
          (Arc::clone(power_supply), power_supply::Delta::default())
        })
        .collect();
      let mut power_platform_profile: Option<String> = None;

      // Higher priority rule first, so we can short-circuit.
      let mut last_applied_rules = Vec::new();

      for rule in config.rules.iter().rev() {
        let Some(condition) = rule.condition.eval(&eval_state)? else {
          continue;
        };

        let condition = condition
          .try_into_boolean()
          .context("`if` was not a boolean")?;

        if condition {
          log::info!(
            "rule '{name}' condition evaluated to true! evaluating members...",
            name = rule.name,
          );

          last_applied_rules.push(rule.name.clone());

          let cpu_some = {
            let (cpu_deltas_lo, cpu_global_delta_lo) =
              rule.cpu.eval(&eval_state)?;

            for (cpu, delta) in cpu_deltas.iter_mut() {
              if let Some(delta_lo) = cpu_deltas_lo.get(cpu) {
                *delta = mem::take(delta).or(delta_lo);
              }
            }

            cpu_global_delta =
              mem::take(&mut cpu_global_delta).or(&cpu_global_delta_lo);

            let deltas_some = cpu_deltas.values().all(|delta| delta.is_some());
            deltas_some && cpu_global_delta.is_some()
          };

          let power_some = {
            let uncore_deltas_lo = rule.uncore.eval(&eval_state)?;
            let vm_delta_lo = rule.vm.eval(&eval_state)?;
            let (disk_deltas_lo, disk_global_delta_lo) =
              rule.disk.eval(&eval_state)?;
            let usb_deltas_lo = rule.usb.eval(&eval_state)?;
            let audio_delta_lo = rule.audio.eval(&eval_state)?;
            let gpu_deltas_lo = rule.gpu.eval(&eval_state)?;

            for (uncore, delta) in uncore_deltas.iter_mut() {
              if let Some(delta_lo) = uncore_deltas_lo.get(uncore) {
                *delta = mem::take(delta).or(delta_lo);
              }
            }
            vm_delta = mem::take(&mut vm_delta).or(&vm_delta_lo);

            for (disk, delta) in disk_deltas.iter_mut() {
              if let Some(delta_lo) = disk_deltas_lo.get(disk) {
                *delta = mem::take(delta).or(delta_lo);
              }
            }
            disk_global_delta =
              mem::take(&mut disk_global_delta).or(&disk_global_delta_lo);

            for (device, delta) in usb_deltas.iter_mut() {
              if let Some(delta_lo) = usb_deltas_lo.get(device) {
                *delta = mem::take(delta).or(delta_lo);
              }
            }

            audio_delta = mem::take(&mut audio_delta).or(&audio_delta_lo);

            for (gpu, delta) in gpu_deltas.iter_mut() {
              if let Some(delta_lo) = gpu_deltas_lo.get(gpu) {
                *delta = mem::take(delta).or(delta_lo);
              }
            }

            let (power_deltas_lo, power_platform_profile_lo) =
              rule.power.eval(&eval_state)?;

            for (power, delta) in power_deltas.iter_mut() {
              if let Some(delta_lo) = power_deltas_lo.get(power) {
                *delta = mem::take(delta).or(delta_lo);
              }
            }

            power_platform_profile =
              power_platform_profile.or(power_platform_profile_lo);

            let deltas_some =
              power_deltas.values().all(|delta| delta.is_some());
            let uncore_some =
              uncore_deltas.values().all(|delta| delta.is_some());
            let disk_some = disk_deltas.values().all(|delta| delta.is_some())
              && disk_global_delta.is_some();
            let usb_some = usb_deltas.values().all(|delta| delta.is_some());
            let gpu_some = gpu_deltas.values().all(|delta| delta.is_some());
            deltas_some
              && power_platform_profile.is_some()
              && uncore_some
              && vm_delta.is_some()
              && disk_some
              && usb_some
              && audio_delta.is_some()
              && gpu_some
          };

          if cpu_some && power_some {
            log::debug!(
              "got a full delta from rules, short circuting evaluation"
            );
            break;
          }
        }
      }

      fs::begin_settings_iteration();
      for (cpu, delta) in &cpu_deltas {
        if runtime_failures
          .attempt(format!("CPU validation for {cpu}"), || delta.validate(cpu))
          .is_some()
        {
          runtime_failures
            .attempt(format!("CPU application for {cpu}"), || {
              delta.apply(&mut (**cpu).clone())
            });
        }
      }

      log::info!("applying CPU deltas to {len} CPUs", len = cpu_deltas.len());

      runtime_failures.attempt("global CPU application", || {
        cpu_global_delta
          .apply(cpu_deltas.keys().map(|arc| &**arc), &mut dma_latency)
      });

      log::info!(
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

      log::info!(
        "applying disk deltas to {len} devices",
        len = disk_deltas.len(),
      );
      for (disk, delta) in disk_deltas {
        runtime_failures
          .attempt(format!("disk application for {disk}"), || {
            delta.apply(&disk)
          });
      }
      runtime_failures
        .attempt("global disk application", || disk_global_delta.apply());

      log::info!(
        "applying USB deltas to {len} devices",
        len = usb_deltas.len(),
      );
      for (device, delta) in usb_deltas {
        runtime_failures
          .attempt(format!("USB application for {device}"), || {
            delta.apply(&device)
          });
      }

      runtime_failures.attempt("audio application", || audio_delta.apply());

      log::info!(
        "applying GPU deltas to {len} devices",
        len = gpu_deltas.len(),
      );
      for (gpu, delta) in gpu_deltas {
        runtime_failures
          .attempt(format!("GPU application for {gpu}"), || delta.apply(&gpu));
      }

      log::info!(
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

      let delay =
        compute_poll_delay(&system, last_polling_delay, last_user_activity);
      runtime_failures.finish_iteration();
      let performance_degraded =
        detect_performance_degradation(&system, &runtime_failures);
      state.write().await.update_system(
        &system,
        last_applied_rules.clone(),
        performance_degraded,
        &runtime_failures,
      );
      applied_rules_tx.send_if_modified(|rules| {
        if *rules == last_applied_rules {
          false
        } else {
          *rules = last_applied_rules;
          true
        }
      });
      last_polling_delay = Some(delay);
      delay
    };

    let elapsed = start.elapsed();
    log::info!(
      "filtered and applied rules in {seconds} seconds or {minutes} minutes",
      seconds = elapsed.as_secs_f64(),
      minutes = elapsed.as_secs_f64() / 60.0,
    );

    log::info!(
      "next poll will be in {seconds} seconds or {minutes} minutes, possibly \
       delayed if application of rules takes more than the polling delay",
      seconds = delay.as_secs_f64(),
      minutes = delay.as_secs_f64() / 60.0,
    );

    sleep_for = delay.saturating_sub(elapsed);
  }

  log::info!("stopping polling loop and shutting down");

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn write_cpu_fixture(root: &Path, governor: &str) {
    let cpu = root.join("sys/devices/system/cpu/cpu0/cpufreq");
    std::fs::create_dir_all(&cpu).unwrap();
    for (name, value) in [
      ("scaling_governor", governor),
      ("scaling_available_governors", "powersave performance"),
      ("cpuinfo_cur_freq", "1000000"),
      ("cpuinfo_min_freq", "500000"),
      ("cpuinfo_max_freq", "2000000"),
      ("scaling_min_freq", "500000"),
      ("scaling_max_freq", "2000000"),
    ] {
      std::fs::write(cpu.join(name), value).unwrap();
    }
    std::fs::create_dir_all(root.join("proc")).unwrap();
    std::fs::write(
      root.join("proc/stat"),
      "cpu 0 0 0 0 0 0 0 0\ncpu0 1 0 1 1 0 0 0 0\n",
    )
    .unwrap();
    std::fs::write(root.join("proc/cpuinfo"), "processor : 0\n").unwrap();
  }

  #[test]
  fn a_failed_scan_keeps_other_scan_failures_local() {
    let root = std::env::temp_dir()
      .join(format!("watt-empty-system-{}", std::process::id(),));
    std::fs::create_dir_all(&root).unwrap();
    let _root = crate::fs::set_system_root_for_tests(&root);
    let mut system = System::default();
    let mut failures = RuntimeFailures::default();

    failures.begin_iteration();
    system.scan(&mut failures);

    assert!(failures.errors.len() > 1);
    assert!(system.cpu_log.is_empty());

    drop(_root);
    std::fs::remove_dir_all(root).unwrap();
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
    let root = std::env::temp_dir()
      .join(format!("watt-hotplug-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let _root = crate::fs::set_system_root_for_tests(&root);
    let mut system = System::default();
    let mut failures = RuntimeFailures::default();

    write_cpu_fixture(&root, "powersave");
    failures.begin_iteration();
    system.scan(&mut failures);
    assert_eq!(system.cpus.len(), 1);

    std::fs::remove_dir_all(root.join("sys/devices/system/cpu/cpu0")).unwrap();
    failures.begin_iteration();
    system.scan(&mut failures);
    assert!(system.cpus.is_empty());

    write_cpu_fixture(&root, "performance");
    failures.begin_iteration();
    system.scan(&mut failures);
    assert_eq!(system.cpus.len(), 1);
    assert_eq!(
      system.cpus.iter().next().unwrap().governor.as_deref(),
      Some("performance"),
    );

    drop(_root);
    std::fs::remove_dir_all(root).unwrap();
  }
}
