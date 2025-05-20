use anyhow::Context;
use anyhow::bail;

use crate::config::AppConfig;
use crate::core::SystemReport;
use crate::engine;
use crate::monitor;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Tracks historical system data for "advanced" adaptive polling
#[derive(Debug)]
struct SystemHistory {
    /// Last several CPU usage measurements
    cpu_usage_history: VecDeque<f32>,
    /// Last several temperature readings
    temperature_history: VecDeque<f32>,
    /// Time of last detected user activity
    last_user_activity: Instant,
    /// Previous battery percentage (to calculate discharge rate)
    last_battery_percentage: Option<f32>,
    /// Timestamp of last battery reading
    last_battery_timestamp: Option<Instant>,
    /// Battery discharge rate (%/hour)
    battery_discharge_rate: Option<f32>,
    /// Time spent in each system state
    state_durations: std::collections::HashMap<SystemState, Duration>,
    /// Last time a state transition happened
    last_state_change: Instant,
    /// Current system state
    current_state: SystemState,
    /// Last computed optimal polling interval
    last_computed_interval: Option<u64>,
}

impl SystemHistory {
    /// Update system history with new report data
    fn update(&mut self, report: &SystemReport) {
        // Update CPU usage history
        if !report.cpu_cores.is_empty() {
            let mut total_usage: f32 = 0.0;
            let mut core_count: usize = 0;

            for core in &report.cpu_cores {
                if let Some(usage) = core.usage_percent {
                    total_usage += usage;
                    core_count += 1;
                }
            }

            if core_count > 0 {
                let avg_usage = total_usage / core_count as f32;

                // Keep only the last 5 measurements
                if self.cpu_usage_history.len() >= 5 {
                    self.cpu_usage_history.pop_front();
                }
                self.cpu_usage_history.push_back(avg_usage);

                // Update last_user_activity if CPU usage indicates activity
                // Consider significant CPU usage or sudden change as user activity
                if avg_usage > 20.0
                    || (self.cpu_usage_history.len() > 1
                        && (avg_usage - self.cpu_usage_history[self.cpu_usage_history.len() - 2])
                            .abs()
                            > 15.0)
                {
                    self.last_user_activity = Instant::now();
                    log::debug!("User activity detected based on CPU usage");
                }
            }
        }

        // Update temperature history
        if let Some(temp) = report.cpu_global.average_temperature_celsius {
            if self.temperature_history.len() >= 5 {
                self.temperature_history.pop_front();
            }
            self.temperature_history.push_back(temp);

            // Significant temperature increase can indicate user activity
            if self.temperature_history.len() > 1 {
                let temp_change =
                    temp - self.temperature_history[self.temperature_history.len() - 2];
                if temp_change > 5.0 {
                    // 5°C rise in temperature
                    self.last_user_activity = Instant::now();
                    log::debug!("User activity detected based on temperature change");
                }
            }
        }

        // Update battery discharge rate
        if let Some(battery) = report.batteries.first() {
            // Reset when we are charging or have just connected AC
            if battery.ac_connected {
                // Reset discharge tracking but continue updating the rest of
                // the history so we still detect activity/load changes on AC.
                self.battery_discharge_rate = None;
                self.last_battery_percentage = None;
                self.last_battery_timestamp = None;
            }

            if let Some(current_percentage) = battery.capacity_percent {
                let current_percent = f32::from(current_percentage);

                if let (Some(last_percentage), Some(last_timestamp)) =
                    (self.last_battery_percentage, self.last_battery_timestamp)
                {
                    let elapsed_hours = last_timestamp.elapsed().as_secs_f32() / 3600.0;
                    // Only calculate discharge rate if at least 30 seconds have passed
                    // and we're not on AC power
                    if elapsed_hours > 0.0083 && !battery.ac_connected {
                        // 0.0083 hours = 30 seconds
                        // Calculate discharge rate in percent per hour
                        let percent_change = last_percentage - current_percent;
                        if percent_change > 0.0 {
                            // Only if battery is discharging
                            let hourly_rate = percent_change / elapsed_hours;
                            // Clamp the discharge rate to a reasonable maximum value (100%/hour)
                            let clamped_rate = hourly_rate.min(100.0);
                            self.battery_discharge_rate = Some(clamped_rate);
                        }
                    }
                }

                self.last_battery_percentage = Some(current_percent);
                self.last_battery_timestamp = Some(Instant::now());
            }
        }

        // Update system state tracking
        let new_state = determine_system_state(report, self);
        if new_state != self.current_state {
            // Record time spent in previous state
            let time_in_state = self.last_state_change.elapsed();
            *self
                .state_durations
                .entry(self.current_state.clone())
                .or_insert(Duration::ZERO) += time_in_state;

            // State changes (except to Idle) likely indicate user activity
            if new_state != SystemState::Idle && new_state != SystemState::LowLoad {
                self.last_user_activity = Instant::now();
                log::debug!("User activity detected based on system state change to {new_state:?}");
            }

            // Update state
            self.current_state = new_state;
            self.last_state_change = Instant::now();
        }

        // Check for significant load changes
        if report.system_load.load_avg_1min > 1.0 {
            self.last_user_activity = Instant::now();
            log::debug!("User activity detected based on system load");
        }
    }

    /// Calculate CPU usage volatility (how much it's changing)
    fn get_cpu_volatility(&self) -> f32 {
        if self.cpu_usage_history.len() < 2 {
            return 0.0;
        }

        let mut sum_of_changes = 0.0;
        for i in 1..self.cpu_usage_history.len() {
            sum_of_changes += (self.cpu_usage_history[i] - self.cpu_usage_history[i - 1]).abs();
        }

        sum_of_changes / (self.cpu_usage_history.len() - 1) as f32
    }

    /// Calculate temperature volatility
    fn get_temperature_volatility(&self) -> f32 {
        if self.temperature_history.len() < 2 {
            return 0.0;
        }

        let mut sum_of_changes = 0.0;
        for i in 1..self.temperature_history.len() {
            sum_of_changes += (self.temperature_history[i] - self.temperature_history[i - 1]).abs();
        }

        sum_of_changes / (self.temperature_history.len() - 1) as f32
    }

    /// Determine if the system appears to be idle
    fn is_system_idle(&self) -> bool {
        if self.cpu_usage_history.is_empty() {
            return false;
        }

        // System considered idle if the average CPU usage of last readings is below 10%
        let recent_avg =
            self.cpu_usage_history.iter().sum::<f32>() / self.cpu_usage_history.len() as f32;
        recent_avg < 10.0 && self.get_cpu_volatility() < 5.0
    }
}

/// Run the daemon
pub fn run_daemon(config: AppConfig) -> anyhow::Result<()> {
    log::info!("Starting superfreq daemon...");

    // Validate critical configuration values before proceeding
    validate_poll_intervals(
        config.daemon.min_poll_interval_sec,
        config.daemon.max_poll_interval_sec,
    )?;

    // Create a flag that will be set to true when a signal is received
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Set up signal handlers
    ctrlc::set_handler(move || {
        log::info!("Received shutdown signal, exiting...");
        r.store(false, Ordering::SeqCst);
    })
    .context("failed to set Ctrl-C handler")?;

    log::info!(
        "Daemon initialized with poll interval: {}s",
        config.daemon.poll_interval_sec
    );

    // Set up stats file if configured
    if let Some(stats_path) = &config.daemon.stats_file_path {
        log::info!("Stats will be written to: {stats_path}");
    }

    // Variables for adaptive polling
    // Make sure that the poll interval is *never* zero to prevent a busy loop
    let mut current_poll_interval = config.daemon.poll_interval_sec.max(1);
    if config.daemon.poll_interval_sec == 0 {
        log::warn!(
            "Poll interval is set to zero in config, using 1s minimum to prevent a busy loop"
        );
    }
    let mut system_history = SystemHistory::default();

    // Main loop
    while running.load(Ordering::SeqCst) {
        let start_time = Instant::now();

        match monitor::collect_system_report(&config) {
            Ok(report) => {
                log::debug!("Collected system report, applying settings...");

                // Store the current state before updating history
                let previous_state = system_history.current_state.clone();

                // Update system history with new data
                system_history.update(&report);

                // Update the stats file if configured
                if let Some(stats_path) = &config.daemon.stats_file_path {
                    if let Err(e) = write_stats_file(stats_path, &report) {
                        log::error!("Failed to write stats file: {e}");
                    }
                }

                match engine::determine_and_apply_settings(&report, &config, None) {
                    Ok(()) => {
                        log::debug!("Successfully applied system settings");

                        // If system state changed, log the new state
                        if system_history.current_state != previous_state {
                            log::info!(
                                "System state changed to: {:?}",
                                system_history.current_state
                            );
                        }
                    }
                    Err(e) => {
                        log::error!("Error applying system settings: {e}");
                    }
                }

                // Check if we're on battery
                let on_battery = !report.batteries.is_empty()
                    && report.batteries.first().is_some_and(|b| !b.ac_connected);

                // Calculate optimal polling interval if adaptive polling is enabled
                if config.daemon.adaptive_interval {
                    match system_history.calculate_optimal_interval(&config, on_battery) {
                        Ok(optimal_interval) => {
                            // Store the new interval
                            system_history.last_computed_interval = Some(optimal_interval);

                            log::debug!("Recalculated optimal interval: {optimal_interval}s");

                            // Don't change the interval too dramatically at once
                            match optimal_interval.cmp(&current_poll_interval) {
                                std::cmp::Ordering::Greater => {
                                    current_poll_interval =
                                        (current_poll_interval + optimal_interval) / 2;
                                }
                                std::cmp::Ordering::Less => {
                                    current_poll_interval = current_poll_interval
                                        - ((current_poll_interval - optimal_interval) / 2).max(1);
                                }
                                std::cmp::Ordering::Equal => {
                                    // No change needed when they're equal
                                }
                            }
                        }
                        Err(e) => {
                            // Log the error and stop the daemon when an invalid configuration is detected
                            log::error!("Critical configuration error: {e}");
                            running.store(false, Ordering::SeqCst);
                            break;
                        }
                    }

                    // Make sure that we respect the (user) configured min and max limits
                    current_poll_interval = current_poll_interval.clamp(
                        config.daemon.min_poll_interval_sec,
                        config.daemon.max_poll_interval_sec,
                    );

                    log::debug!("Adaptive polling: set interval to {current_poll_interval}s");
                } else {
                    // If adaptive polling is disabled, still apply battery-saving adjustment
                    if config.daemon.throttle_on_battery && on_battery {
                        let battery_multiplier = 2; // poll half as often on battery

                        // We need to make sure `poll_interval_sec` is *at least* 1
                        // before multiplying.
                        let safe_interval = config.daemon.poll_interval_sec.max(1);
                        current_poll_interval = (safe_interval * battery_multiplier)
                            .min(config.daemon.max_poll_interval_sec);

                        log::debug!(
                            "On battery power, increased poll interval to {current_poll_interval}s"
                        );
                    } else {
                        // Use the configured poll interval
                        current_poll_interval = config.daemon.poll_interval_sec.max(1);
                        if config.daemon.poll_interval_sec == 0 {
                            log::debug!(
                                "Using minimum poll interval of 1s instead of configured 0s"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("Error collecting system report: {e}");
            }
        }

        // Sleep for the remaining time in the poll interval
        let elapsed = start_time.elapsed();
        let poll_duration = Duration::from_secs(current_poll_interval);
        if elapsed < poll_duration {
            let sleep_time = poll_duration - elapsed;
            log::debug!("Sleeping for {}s until next cycle", sleep_time.as_secs());
            std::thread::sleep(sleep_time);
        }
    }

    log::info!("Daemon stopped");
    Ok(())
}

/// Simplified system state used for determining when to adjust polling interval
#[derive(Debug, PartialEq, Eq, Clone, Hash, Default)]
enum SystemState {
    #[default]
    Unknown,
    OnAC,
    OnBattery,
    HighLoad,
    LowLoad,
    HighTemp,
    Idle,
}

/// Determine the current system state for adaptive polling
fn determine_system_state(report: &SystemReport, history: &SystemHistory) -> SystemState {
    // Check power state first
    if !report.batteries.is_empty() {
        if let Some(battery) = report.batteries.first() {
            if battery.ac_connected {
                return SystemState::OnAC;
            }
            return SystemState::OnBattery;
        }
    }

    // No batteries means desktop, so always AC
    if report.batteries.is_empty() {
        return SystemState::OnAC;
    }

    // Check temperature
    if let Some(temp) = report.cpu_global.average_temperature_celsius {
        if temp > 80.0 {
            return SystemState::HighTemp;
        }
    }

    // Check load first, as high load should take precedence over idle state
    let avg_load = report.system_load.load_avg_1min;
    if avg_load > 3.0 {
        return SystemState::HighLoad;
    }

    // Check idle state only if we don't have high load
    if history.is_system_idle() {
        return SystemState::Idle;
    }

    // Check for low load
    if avg_load < 0.5 {
        return SystemState::LowLoad;
    }

    // Default case
    SystemState::Unknown
}
