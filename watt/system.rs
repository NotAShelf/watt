use std::{
  collections::{
    HashMap,
    HashSet,
    VecDeque,
  },
  mem,
  path::Path,
  sync::{
    Arc,
    atomic::{
      AtomicBool,
      Ordering,
    },
  },
  thread,
  time::{
    Duration,
    Instant,
  },
};

use anyhow::{
  Context,
  bail,
};

use crate::{
  config,
  cpu,
  fs,
  power_supply,
};

#[derive(Debug)]
struct CpuLog {
  at: Instant,

  /// CPU usage between 0-1, a percentage.
  usage: f64,

  /// CPU temperature in celsius.
  temperature: f64,
}

#[derive(Debug)]
struct CpuVolatility {
  usage: f64,

  temperature: f64,
}

#[derive(Debug)]
struct PowerSupplyLog {
  at: Instant,

  /// Charge 0-1, as a percentage.
  charge: f64,
}

#[derive(Default, Debug)]
struct System {
  is_ac: bool,

  load_average_1min:  f64,
  load_average_5min:  f64,
  load_average_15min: f64,

  /// All CPUs.
  cpus:             HashSet<Arc<cpu::Cpu>>,
  /// CPU usage and temperature log.
  cpu_log:          VecDeque<CpuLog>,
  cpu_temperatures: HashMap<u32, f64>,

  /// All power supplies.
  power_supplies:   HashSet<Arc<power_supply::PowerSupply>>,
  /// Power supply status log.
  power_supply_log: VecDeque<PowerSupplyLog>,
}

impl System {
  fn scan(&mut self) -> anyhow::Result<()> {
    log::info!("scanning view of system hardware...");

    {
      let start = Instant::now();
      self.cpus = cpu::Cpu::all()
        .context("failed to scan CPUs")?
        .into_iter()
        .map(Arc::from)
        .collect();
      log::info!(
        "scanned all CPUs in {millis}ms",
        millis = start.elapsed().as_millis(),
      );
    }

    {
      let start = Instant::now();
      self.power_supplies = power_supply::PowerSupply::all()
        .context("failed to scan power supplies")?
        .into_iter()
        .map(Arc::from)
        .collect();
      log::info!(
        "scanned all power supplies in {millis}ms",
        millis = start.elapsed().as_millis(),
      );
    }

    self.is_ac = self
      .power_supplies
      .iter()
      .any(|power_supply| power_supply.is_ac())
      || {
        log::debug!(
          "checking whether if this device is a desktop to determine if it is \
           AC as no power supplies are"
        );

        let start = Instant::now();
        let is_desktop = self.is_desktop()?;
        log::debug!(
          "checked if is a desktop in {millis}ms",
          millis = start.elapsed().as_millis(),
        );

        log::debug!(
          "scan result: {elaborate}",
          elaborate = if is_desktop {
            "is a desktop, therefore is AC"
          } else {
            "not a desktop, and not AC"
          },
        );

        is_desktop
      };

    {
      let start = Instant::now();
      self.scan_load_average()?;
      log::info!(
        "scanned load average in {millis}ms",
        millis = start.elapsed().as_millis(),
      );
    }

    {
      let start = Instant::now();
      self.scan_temperatures()?;
      log::info!(
        "scanned temperatures in {millis}ms",
        millis = start.elapsed().as_millis(),
      );
    }

    log::debug!("appending to system logs...");

    let at = Instant::now();

    while self.cpu_log.len() > 100 {
      log::debug!("daemon CPU log was too long, popping element");
      self.cpu_log.pop_front();
    }

    let cpu_log = CpuLog {
      at,

      usage: self.cpus.iter().map(|cpu| cpu.stat.usage()).sum::<f64>()
        / self.cpus.len() as f64,

      temperature: self.cpu_temperatures.values().sum::<f64>()
        / self.cpu_temperatures.len() as f64,
    };
    log::debug!("appending CPU log item: {cpu_log:?}");
    self.cpu_log.push_back(cpu_log);

    while self.power_supply_log.len() > 100 {
      log::debug!("daemon power supply log was too long, popping element");
      self.power_supply_log.pop_front();
    }

    if !self.power_supplies.is_empty() {
      let power_supply_log = PowerSupplyLog {
        at,
        charge: {
          let (charge_sum, charge_nr) = self.power_supplies.iter().fold(
            (0.0, 0u32),
            |(sum, count), power_supply| {
              if let Some(charge_percent) = power_supply.charge_percent {
                (sum + charge_percent, count + 1)
              } else {
                (sum, count)
              }
            },
          );

          charge_sum / charge_nr as f64
        },
      };
      log::debug!("appending power supply log item: {power_supply_log:?}");
      self.power_supply_log.push_back(power_supply_log);
    }

    Ok(())
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

      let Ok(number) = number.parse::<u32>() else {
        log::debug!("stripped content not a valid number, skipping");
        continue;
      };
      log::debug!(
        "stripped content is a valid number, taking it as the core number"
      );
      log::debug!(
        "it is fine if this number doesn't seem accurate due to CPU binning, see a more detailed explanation at: https://rgbcu.be/blog/why-cores"
      );

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

      temperatures.insert(number, temperature_mc as f64 / 1000.0);
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
      return Ok(true); // Likely a desktop.
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

    for index in 0..change_count {
      let usage_change =
        self.cpu_log[index + 1].usage - self.cpu_log[index].usage;
      usage_change_sum += usage_change.abs();

      let temperature_change =
        self.cpu_log[index + 1].temperature - self.cpu_log[index].temperature;
      temperature_change_sum += temperature_change.abs();
    }

    Some(CpuVolatility {
      usage:       usage_change_sum / change_count as f64,
      temperature: temperature_change_sum / change_count as f64,
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

pub fn run_daemon(config: config::DaemonConfig) -> anyhow::Result<()> {
  assert!(config.rules.is_sorted_by_key(|rule| rule.priority));

  log::info!("starting daemon...");

  let cancelled = Arc::new(AtomicBool::new(false));

  {
    log::debug!("setting ctrl-c handler...");
    ctrlc::set_handler({
      let cancelled = Arc::clone(&cancelled);

      move || {
        log::info!("received shutdown signal");
        cancelled.store(true, Ordering::SeqCst);
      }
    })
    .context("failed to set ctrl-c handler")?;
  }

  let mut system = System::default();
  let mut last_polling_delay = None::<Duration>;
  // TODO: Set this somewhere.
  let last_user_activity = Instant::now();

  while !cancelled.load(Ordering::SeqCst) {
    log::debug!("starting main polling loop iteration");

    system.scan()?;

    let delay = {
      let mut delay = Duration::from_secs(5);

      // We are on battery, so we must be more conservative with our polling.
      if system.is_discharging() {
        match system.power_supply_discharge_rate() {
          Some(discharge_rate) => {
            if discharge_rate > 0.2 {
              delay *= 3;
            } else if discharge_rate > 0.1 {
              delay *= 2;
            } else {
              // *= 1.5;
              delay /= 2;
              delay *= 3;
            }
          },

          // If we can't determine the discharge rate, that means that
          // we were very recently started. Which is user activity.
          None => {
            delay *= 2;
          },
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
        && (volatility.usage > 0.1 || volatility.temperature > 0.02)
      {
        delay = (delay / 2).max(Duration::from_secs(1));
      }

      let delay = match last_polling_delay {
        Some(last_delay) => {
          Duration::from_secs_f64(
            // 30% of current computed delay, 70% of last delay.
            delay.as_secs_f64() * 0.3 + last_delay.as_secs_f64() * 0.7,
          )
        },

        None => delay,
      };

      let delay = Duration::from_secs_f64(delay.as_secs_f64().clamp(1.0, 30.0));

      last_polling_delay = Some(delay);

      delay
    };
    log::info!(
      "next poll will be in {seconds} seconds or {minutes} minutes, possibly \
       delayed if application of rules takes more than the polling delay",
      seconds = delay.as_secs_f64(),
      minutes = delay.as_secs_f64() / 60.0,
    );

    log::info!("filtering rules and applying them...");

    let start = Instant::now();

    let state = config::EvalState {
      frequency_available: system
        .cpus
        .iter()
        .any(|cpu| cpu.frequency_mhz.is_some()),
      turbo_available:     cpu::Cpu::turbo()
        .context(
          "failed to read CPU turbo boost status for `is-turbo-available`",
        )?
        .is_some(),

      cpu_usage:                  system
        .cpu_log
        .back()
        .context("CPU log is empty")?
        .usage,
      cpu_usage_volatility:       system.cpu_volatility().map(|vol| vol.usage),
      cpu_temperature:            system
        .cpu_log
        .back()
        .map(|log| log.temperature),
      cpu_temperature_volatility: system
        .cpu_volatility()
        .map(|vol| vol.temperature),
      cpu_idle_seconds:           last_user_activity.elapsed().as_secs_f64(),
      cpu_frequency_maximum:      cpu::Cpu::hardware_frequency_mhz_maximum()
        .context("failed to read CPU hardware maximum frequency")?
        .map(|u64| u64 as f64),
      cpu_frequency_minimum:      cpu::Cpu::hardware_frequency_mhz_minimum()
        .context("failed to read CPU hardware minimum frequency")?
        .map(|u64| u64 as f64),

      power_supply_charge:         system
        .power_supply_log
        .back()
        .map(|log| log.charge),
      power_supply_discharge_rate: system.power_supply_discharge_rate(),

      discharging: system.is_discharging(),

      context: config::EvalContext::WidestPossible,

      cpus:           &system.cpus,
      power_supplies: &system.power_supplies,
    };

    let mut cpu_deltas: HashMap<Arc<cpu::Cpu>, cpu::Delta> = system
      .cpus
      .iter()
      .map(|cpu| (Arc::clone(cpu), cpu::Delta::default()))
      .collect();
    let mut cpu_turbo: Option<bool> = None;

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
    for rule in config.rules.iter().rev() {
      let Some(condition) = rule.condition.eval(&state)? else {
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

        let cpu_some = {
          let (cpu_deltas_lo, cpu_turbo_lo) = rule.cpu.eval(&state)?;

          let deltas_some = cpu_deltas.iter_mut().all(|(cpu, delta)| {
            let delta_lo = cpu_deltas_lo
              .get(cpu)
              .expect("cpu deltas and cpus should match");

            *delta = mem::take(delta).or(delta_lo);

            delta.is_some()
          });

          cpu_turbo = cpu_turbo.or(cpu_turbo_lo);

          deltas_some && cpu_turbo.is_some()
        };

        let power_some = {
          let (power_deltas_lo, power_platform_profile_lo) =
            rule.power.eval(&state)?;

          let deltas_some = power_deltas.iter_mut().all(|(power, delta)| {
            let delta_lo = power_deltas_lo
              .get(power)
              .expect("power deltas and power supplies should match");

            *delta = mem::take(delta).or(delta_lo);

            delta.is_some()
          });

          power_platform_profile =
            power_platform_profile.or(power_platform_profile_lo);

          deltas_some && power_platform_profile.is_some()
        };

        if cpu_some && power_some {
          log::debug!(
            "got a full delta from rules, short circuting evaluation"
          );
          break;
        }
      }
    }

    for (cpu, delta) in &cpu_deltas {
      delta
        .apply(&mut (**cpu).clone())
        .with_context(|| format!("failed to apply delta to {cpu}"))?;
    }

    log::info!("applying CPU deltas to {len} CPUs", len = cpu_deltas.len());

    if let Some(turbo) = cpu_turbo {
      cpu::Cpu::set_turbo(turbo, cpu_deltas.keys().map(|arc| &**arc))
        .context("failed to set CPU turbo")?;
    }

    log::info!(
      "applying power supply deltas to {len} devices",
      len = power_deltas.len(),
    );

    for (power, delta) in power_deltas {
      delta
        .apply(&mut (*power).clone())
        .with_context(|| format!("failed to apply delta to {power}"))?;
    }

    if let Some(platform_profile) = power_platform_profile {
      power_supply::PowerSupply::set_platform_profile(&platform_profile)
        .context("failed to set power supply platform profile")?;
    }

    let elapsed = start.elapsed();
    log::info!(
      "filtered and applied rules in {seconds} seconds or {minutes} minutes",
      seconds = elapsed.as_secs_f64(),
      minutes = elapsed.as_secs_f64() / 60.0,
    );

    thread::sleep(delay.saturating_sub(elapsed));
  }

  log::info!("stopping polling loop and thus ..");

  Ok(())
}
