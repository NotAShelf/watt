use std::{
  cell::LazyCell,
  collections::{
    HashMap,
    VecDeque,
  },
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

use anyhow::Context;

use crate::{
  config,
  system,
};

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

#[derive(Debug)]
struct Daemon {
  /// Last time when there was user activity.
  last_user_activity: Instant,

  /// The last computed polling delay.
  last_polling_delay: Option<Duration>,

  /// The system state.
  system: system::System,

  /// CPU usage and temperature log.
  cpu_log: VecDeque<CpuLog>,

  /// Power supply status log.
  power_supply_log: VecDeque<PowerSupplyLog>,
}

impl Daemon {
  fn rescan(&mut self) -> anyhow::Result<()> {
    self.system.rescan()?;

    log::debug!("appending to daemon logs...");

    let at = Instant::now();

    while self.cpu_log.len() > 100 {
      log::debug!("daemon CPU log was too long, popping element");
      self.cpu_log.pop_front();
    }

    let cpu_log = CpuLog {
      at,

      usage: self
        .system
        .cpus
        .iter()
        .map(|cpu| cpu.stat.usage())
        .sum::<f64>()
        / self.system.cpus.len() as f64,

      temperature: self.system.cpu_temperatures.values().sum::<f64>()
        / self.system.cpu_temperatures.len() as f64,
    };
    log::debug!("appending CPU log item: {cpu_log:?}");
    self.cpu_log.push_back(cpu_log);

    while self.power_supply_log.len() > 100 {
      log::debug!("daemon power supply log was too long, popping element");
      self.power_supply_log.pop_front();
    }

    let power_supply_log = PowerSupplyLog {
      at,
      charge: {
        let (charge_sum, charge_nr) = self.system.power_supplies.iter().fold(
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

    Ok(())
  }
}

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

impl Daemon {
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
}

#[derive(Debug)]
struct PowerSupplyLog {
  at: Instant,

  /// Charge 0-1, as a percentage.
  charge: f64,
}

impl Daemon {
  fn discharging(&self) -> bool {
    self.system.power_supplies.iter().any(|power_supply| {
      power_supply.charge_state.as_deref() == Some("Discharging")
    })
  }

  /// Calculates the discharge rate, returns a number between 0 and 1.
  ///
  /// The discharge rate is averaged per hour.
  /// So a return value of Some(0.3) means the battery has been
  /// discharging 30% per hour.
  fn power_supply_discharge_rate(&self) -> Option<f64> {
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
    let start = discharging.last().unwrap();
    // End of discharging, very close to now. Has the least charge.
    let end = discharging.first().unwrap();

    let discharging_duration_seconds = (start.at - end.at).as_secs_f64();
    let discharging_duration_hours = discharging_duration_seconds / 60.0 / 60.0;
    let discharged = start.charge - end.charge;

    Some(discharged / discharging_duration_hours)
  }
}

impl Daemon {
  fn polling_delay(&mut self) -> Duration {
    let mut delay = Duration::from_secs(5);

    // We are on battery, so we must be more conservative with our polling.
    if self.discharging() {
      match self.power_supply_discharge_rate() {
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

    if self.is_cpu_idle() {
      let idle_for = self.last_user_activity.elapsed();

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

    if let Some(volatility) = self.cpu_volatility() {
      if volatility.usage > 0.1 || volatility.temperature > 0.02 {
        delay = (delay / 2).max(Duration::from_secs(1));
      }
    }

    let delay = match self.last_polling_delay {
      Some(last_delay) => {
        Duration::from_secs_f64(
          // 30% of current computed delay, 70% of last delay.
          delay.as_secs_f64() * 0.3 + last_delay.as_secs_f64() * 0.7,
        )
      },

      None => delay,
    };

    let delay = Duration::from_secs_f64(delay.as_secs_f64().clamp(1.0, 30.0));

    self.last_polling_delay = Some(delay);

    delay
  }
}

pub fn run(config: config::DaemonConfig) -> anyhow::Result<()> {
  assert!(config.rules.is_sorted_by_key(|rule| rule.priority));

  log::info!("starting daemon...");

  let cancelled = Arc::new(AtomicBool::new(false));

  log::debug!("setting ctrl-c handler...");
  let cancelled_ = Arc::clone(&cancelled);
  ctrlc::set_handler(move || {
    log::info!("received shutdown signal");
    cancelled_.store(true, Ordering::SeqCst);
  })
  .context("failed to set ctrl-c handler")?;

  let mut daemon = Daemon {
    last_user_activity: Instant::now(),

    last_polling_delay: None,

    system: system::System::new()?,

    cpu_log:          VecDeque::new(),
    power_supply_log: VecDeque::new(),
  };

  while !cancelled.load(Ordering::SeqCst) {
    daemon.rescan()?;

    let delay = daemon.polling_delay();
    log::info!(
      "next poll will be in {seconds} seconds or {minutes} minutes, possibly \
       delayed if application of rules takes more than the polling delay",
      seconds = delay.as_secs_f64(),
      minutes = delay.as_secs_f64() / 60.0,
    );

    log::info!("filtering rules and applying them...");

    let start = Instant::now();

    let state = config::EvalState {
      cpu_usage:                   daemon.cpu_log.back().unwrap().usage,
      cpu_usage_volatility:        daemon.cpu_volatility().map(|vol| vol.usage),
      cpu_temperature:             daemon.cpu_log.back().unwrap().temperature,
      cpu_temperature_volatility:  daemon
        .cpu_volatility()
        .map(|vol| vol.temperature),
      cpu_idle_seconds:            daemon
        .last_user_activity
        .elapsed()
        .as_secs_f64(),
      power_supply_charge:         daemon
        .power_supply_log
        .back()
        .unwrap()
        .charge,
      power_supply_discharge_rate: daemon.power_supply_discharge_rate(),
      discharging:                 daemon.discharging(),
    };

    let mut cpu_delta_for = HashMap::<u32, config::CpuDelta>::new();
    let all_cpus =
      LazyCell::new(|| (0..num_cpus::get() as u32).collect::<Vec<_>>());

    for rule in &config.rules {
      let Some(condition) = rule.condition.eval(&state)? else {
        continue;
      };

      let cpu_for = rule.cpu.for_.as_ref().unwrap_or_else(|| &*all_cpus);

      for cpu in cpu_for {
        let delta = cpu_delta_for.entry(*cpu).or_default();

        delta.for_ = Some(vec![*cpu]);

        if let Some(governor) = rule.cpu.governor.as_ref() {
          delta.governor = Some(governor.clone());
        }

        if let Some(epp) = rule.cpu.energy_performance_preference.as_ref() {
          delta.energy_performance_preference = Some(epp.clone());
        }

        if let Some(epb) = rule.cpu.energy_performance_bias.as_ref() {
          delta.energy_performance_bias = Some(epb.clone());
        }

        if let Some(mhz_minimum) = rule.cpu.frequency_mhz_minimum {
          delta.frequency_mhz_minimum = Some(mhz_minimum);
        }

        if let Some(mhz_maximum) = rule.cpu.frequency_mhz_maximum {
          delta.frequency_mhz_maximum = Some(mhz_maximum);
        }

        if let Some(turbo) = rule.cpu.turbo {
          delta.turbo = Some(turbo);
        }
      }

      // TODO: Also merge this into one like CPU.
      if condition.as_boolean()? {
        rule.power.apply()?;
      }
    }

    for delta in cpu_delta_for.values() {
      delta.apply()?;
    }

    let elapsed = start.elapsed();
    log::info!(
      "filtered and applied rules in {seconds} seconds or {minutes} minutes",
      seconds = elapsed.as_secs_f64(),
      minutes = elapsed.as_secs_f64() / 60.0,
    );

    thread::sleep(delay.saturating_sub(elapsed));
  }

  log::info!("stopping polling loop and thus daemon...");

  Ok(())
}
