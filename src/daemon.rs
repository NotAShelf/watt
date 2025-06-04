use std::{
    collections::VecDeque,
    ops,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;

use crate::{config, system};

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
        }
    };

    // Clamp the multiplier to avoid excessive intervals.
    (1.0 + factor).clamp(1.0, 5.0)
}

struct Daemon {
    /// Last time when there was user activity.
    last_user_activity: Instant,

    /// The last computed polling interval.
    last_polling_interval: Option<Duration>,

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

        while self.cpu_log.len() > 99 {
            self.cpu_log.pop_front();
        }

        self.cpu_log.push_back(CpuLog {
            at: Instant::now(),

            usage: self
                .system
                .cpus
                .iter()
                .map(|cpu| cpu.stat.usage())
                .sum::<f64>()
                / self.system.cpus.len() as f64,

            temperature: self.system.cpu_temperatures.values().sum::<f64>()
                / self.system.cpu_temperatures.len() as f64,
        });

        let at = Instant::now();

        let (charge_sum, charge_nr) =
            self.system
                .power_supplies
                .iter()
                .fold((0.0, 0u32), |(sum, count), power_supply| {
                    if let Some(charge_percent) = power_supply.charge_percent {
                        (sum + charge_percent, count + 1)
                    } else {
                        (sum, count)
                    }
                });

        while self.power_supply_log.len() > 99 {
            self.power_supply_log.pop_front();
        }

        self.power_supply_log.push_back(PowerSupplyLog {
            at,
            charge: charge_sum / charge_nr as f64,
        });

        Ok(())
    }
}

struct CpuLog {
    at: Instant,

    /// CPU usage between 0-1, a percentage.
    usage: f64,

    /// CPU temperature in celcius.
    temperature: f64,
}

struct CpuVolatility {
    at: ops::Range<Instant>,

    usage: f64,

    temperature: f64,
}

impl Daemon {
    fn cpu_volatility(&self) -> Option<CpuVolatility> {
        if self.cpu_log.len() < 2 {
            return None;
        }

        let change_count = self.cpu_log.len() - 1;

        let mut usage_change_sum = 0.0;
        let mut temperature_change_sum = 0.0;

        for index in 0..change_count {
            let usage_change = self.cpu_log[index + 1].usage - self.cpu_log[index].usage;
            usage_change_sum += usage_change.abs();

            let temperature_change =
                self.cpu_log[index + 1].temperature - self.cpu_log[index].temperature;
            temperature_change_sum += temperature_change.abs();
        }

        Some(CpuVolatility {
            at: self.cpu_log.front().unwrap().at..self.cpu_log.back().unwrap().at,

            usage: usage_change_sum / change_count as f64,
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

struct PowerSupplyLog {
    at: Instant,

    /// Charge 0-1, as a percentage.
    charge: f64,
}

impl Daemon {
    fn discharging(&self) -> bool {
        self.system
            .power_supplies
            .iter()
            .any(|power_supply| power_supply.charge_state.as_deref() == Some("Discharging"))
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
    fn polling_interval(&mut self) -> Duration {
        let mut interval = Duration::from_secs(5);

        // We are on battery, so we must be more conservative with our polling.
        if self.discharging() {
            match self.power_supply_discharge_rate() {
                Some(discharge_rate) => {
                    if discharge_rate > 0.2 {
                        interval *= 3;
                    } else if discharge_rate > 0.1 {
                        interval *= 2;
                    } else {
                        // *= 1.5;
                        interval /= 2;
                        interval *= 3;
                    }
                }

                // If we can't deterine the discharge rate, that means that
                // we were very recently started. Which is user activity.
                None => {
                    interval *= 2;
                }
            }
        }

        if self.is_cpu_idle() {
            let idle_for = self.last_user_activity.elapsed();

            if idle_for > Duration::from_secs(30) {
                let factor = idle_multiplier(idle_for);

                log::debug!(
                    "system has been idle for {seconds} seconds (approx {minutes} minutes), applying idle factor: {factor:.2}x",
                    seconds = idle_for.as_secs(),
                    minutes = idle_for.as_secs() / 60,
                );

                interval = Duration::from_secs_f64(interval.as_secs_f64() * factor);
            }
        }

        if let Some(volatility) = self.cpu_volatility() {
            if volatility.usage > 0.1 || volatility.temperature > 0.02 {
                interval = (interval / 2).max(Duration::from_secs(1));
            }
        }

        let interval = match self.last_polling_interval {
            Some(last_interval) => Duration::from_secs_f64(
                // 30% of current computed interval, 70% of last interval.
                interval.as_secs_f64() * 0.3 + last_interval.as_secs_f64() * 0.7,
            ),

            None => interval,
        };

        let interval = Duration::from_secs_f64(interval.as_secs_f64().clamp(1.0, 30.0));

        self.last_polling_interval = Some(interval);

        interval
    }
}

impl Daemon {
    fn eval(&self, expression: &config::Expression) -> anyhow::Result<Option<config::Expression>> {
        use config::Expression::*;

        macro_rules! try_ok {
            ($expression:expr) => {
                match $expression {
                    Some(value) => value,
                    None => return Ok(None),
                }
            };
        }

        Ok(Some(match expression {
            CpuUsage => Number(self.cpu_log.back().unwrap().usage),
            CpuUsageVolatility => Number(try_ok!(self.cpu_volatility()).usage),
            CpuTemperature => Number(self.cpu_log.back().unwrap().temperature),
            CpuTemperatureVolatility => Number(try_ok!(self.cpu_volatility()).temperature),
            CpuIdleSeconds => Number(self.last_user_activity.elapsed().as_secs_f64()),
            PowerSupplyCharge => Number(self.power_supply_log.back().unwrap().charge),
            PowerSupplyDischargeRate => Number(try_ok!(self.power_supply_discharge_rate())),

            Charging => Boolean(!self.discharging()),
            OnBattery => Boolean(self.discharging()),

            literal @ Boolean(_) | literal @ Number(_) => literal.clone(),

            Plus { value, plus } => Number(
                try_ok!(self.eval(value)?).as_number()? + try_ok!(self.eval(plus)?).as_number()?,
            ),
            Minus { value, minus } => Number(
                try_ok!(self.eval(value)?).as_number()? - try_ok!(self.eval(minus)?).as_number()?,
            ),
            Multiply { value, multiply } => Number(
                try_ok!(self.eval(value)?).as_number()?
                    * try_ok!(self.eval(multiply)?).as_number()?,
            ),
            Power { value, power } => Number(
                try_ok!(self.eval(value)?)
                    .as_number()?
                    .powf(try_ok!(self.eval(power)?).as_number()?),
            ),
            Divide { value, divide } => Number(
                try_ok!(self.eval(value)?).as_number()?
                    / try_ok!(self.eval(divide)?).as_number()?,
            ),

            LessThan {
                value,
                is_less_than,
            } => Boolean(
                try_ok!(self.eval(value)?).as_number()?
                    < try_ok!(self.eval(is_less_than)?).as_number()?,
            ),
            MoreThan {
                value,
                is_more_than,
            } => Boolean(
                try_ok!(self.eval(value)?).as_number()?
                    > try_ok!(self.eval(is_more_than)?).as_number()?,
            ),
            Equal {
                value,
                is_equal,
                leeway,
            } => {
                let value = try_ok!(self.eval(value)?).as_number()?;
                let leeway = try_ok!(self.eval(leeway)?).as_number()?;

                let is_equal = try_ok!(self.eval(is_equal)?).as_number()?;

                let minimum = value - leeway;
                let maximum = value + leeway;

                Boolean(minimum < is_equal && is_equal < maximum)
            }

            And { value, and } => Boolean(
                try_ok!(self.eval(value)?).as_boolean()?
                    && try_ok!(self.eval(and)?).as_boolean()?,
            ),
            All { all } => {
                let mut result = true;

                for value in all {
                    result = result && try_ok!(self.eval(value)?).as_boolean()?;

                    if !result {
                        break;
                    }
                }

                Boolean(result)
            }
            Or { value, or } => Boolean(
                try_ok!(self.eval(value)?).as_boolean()? || try_ok!(self.eval(or)?).as_boolean()?,
            ),
            Any { any } => {
                let mut result = false;

                for value in any {
                    result = result || try_ok!(self.eval(value)?).as_boolean()?;

                    if result {
                        break;
                    }
                }

                Boolean(result)
            }
            Not { not } => Boolean(!try_ok!(self.eval(not)?).as_boolean()?),
        }))
    }
}

pub fn run(config: config::DaemonConfig) -> anyhow::Result<()> {
    assert!(config.rules.is_sorted_by_key(|rule| rule.priority));

    log::info!("starting daemon...");

    let cancelled = Arc::new(AtomicBool::new(false));

    let cancelled_ = Arc::clone(&cancelled);
    ctrlc::set_handler(move || {
        log::info!("received shutdown signal");
        cancelled_.store(true, Ordering::SeqCst);
    })
    .context("failed to set Ctrl-C handler")?;

    let mut daemon = Daemon {
        last_user_activity: Instant::now(),

        last_polling_interval: None,

        system: system::System::new()?,

        cpu_log: VecDeque::new(),
        power_supply_log: VecDeque::new(),
    };

    while !cancelled.load(Ordering::SeqCst) {
        daemon.rescan()?;

        let sleep_until = Instant::now() + daemon.polling_interval();

        for rule in &config.rules {
            let Some(condition) = daemon.eval(&rule.if_)? else {
                continue;
            };

            if condition.as_boolean()? {
                rule.cpu.apply()?;
                rule.power.apply()?;
            }
        }

        if let Some(delay) = sleep_until.checked_duration_since(Instant::now()) {
            thread::sleep(delay);
        }
    }

    log::info!("exiting...");

    Ok(())
}
