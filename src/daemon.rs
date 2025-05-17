use crate::config::watcher::ConfigWatcher;
use crate::config::{AppConfig, LogLevel};
use crate::core::SystemReport;
use crate::engine;
use crate::monitor;
use crate::util::error::AppError;
use log::{LevelFilter, debug, error, info, warn};
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Parameters for computing optimal polling interval
struct IntervalParams {
    /// Base polling interval in seconds
    base_interval: u64,
    /// Minimum allowed polling interval in seconds
    min_interval: u64,
    /// Maximum allowed polling interval in seconds
    max_interval: u64,
    /// How rapidly CPU usage is changing
    cpu_volatility: f32,
    /// How rapidly temperature is changing
    temp_volatility: f32,
    /// Battery discharge rate in %/hour if available
    battery_discharge_rate: Option<f32>,
    /// Time since last detected user activity
    last_user_activity: Duration,
    /// Whether the system appears to be idle
    is_system_idle: bool,
    /// Whether the system is running on battery power
    on_battery: bool,
}

/// Calculate the idle time multiplier based on system idle duration
///
/// Returns a multiplier between 1.0 and 5.0 (capped):
/// - For idle times < 2 minutes: Linear interpolation from 1.0 to 2.0
/// - For idle times >= 2 minutes: Logarithmic scaling (1.0 + log2(minutes))
fn idle_multiplier(idle_secs: u64) -> f32 {
    if idle_secs == 0 {
        return 1.0; // No idle time, no multiplier effect
    }

    let idle_factor = if idle_secs < 120 {
        // Less than 2 minutes (0 to 119 seconds)
        // Linear interpolation from 1.0 (at 0s) to 2.0 (at 120s)
        1.0 + (idle_secs as f32) / 120.0
    } else {
        // 2 minutes (120 seconds) or more
        let idle_time_minutes = idle_secs / 60;
        // Logarithmic scaling: 1.0 + log2(minutes)
        1.0 + (idle_time_minutes as f32).log2().max(0.5)
    };

    // Cap the multiplier to avoid excessive intervals
    idle_factor.min(5.0) // max factor of 5x
}

/// Calculate optimal polling interval based on system conditions and history
fn compute_new(params: &IntervalParams) -> u64 {
    // Start with base interval
    let mut adjusted_interval = params.base_interval;

    // If we're on battery, we want to be more aggressive about saving power
    if params.on_battery {
        // Apply a multiplier based on battery discharge rate
        if let Some(discharge_rate) = params.battery_discharge_rate {
            if discharge_rate > 20.0 {
                // High discharge rate - increase polling interval significantly (3x)
                adjusted_interval = adjusted_interval.saturating_mul(3);
            } else if discharge_rate > 10.0 {
                // Moderate discharge - double polling interval (2x)
                adjusted_interval = adjusted_interval.saturating_mul(2);
            } else {
                // Low discharge rate - increase by 50% (multiply by 3/2)
                adjusted_interval = adjusted_interval.saturating_mul(3).saturating_div(2);
            }
        } else {
            // If we don't know discharge rate, use a conservative multiplier (2x)
            adjusted_interval = adjusted_interval.saturating_mul(2);
        }
    }

    // Adjust for system idleness
    if params.is_system_idle {
        let idle_time_seconds = params.last_user_activity.as_secs();

        // Apply adjustment only if the system has been idle for a non-zero duration
        if idle_time_seconds > 0 {
            let idle_factor = idle_multiplier(idle_time_seconds);

            debug!(
                "System idle for {} seconds (approx. {} minutes), applying idle factor: {:.2}x",
                idle_time_seconds,
                (idle_time_seconds as f32 / 60.0).round(),
                idle_factor
            );

            // Convert f32 multiplier to integer-safe math
            // Multiply by a large number first, then divide to maintain precision
            // Use 1000 as the scaling factor to preserve up to 3 decimal places
            let scaling_factor = 1000;
            let scaled_factor = (idle_factor * scaling_factor as f32) as u64;
            adjusted_interval = adjusted_interval
                .saturating_mul(scaled_factor)
                .saturating_div(scaling_factor);
        }
        // If idle_time_seconds is 0, no factor is applied by this block
    }

    // Adjust for CPU/temperature volatility
    if params.cpu_volatility > 10.0 || params.temp_volatility > 2.0 {
        // For division by 2 (halving the interval), we can safely use integer division
        adjusted_interval = (adjusted_interval / 2).max(1);
    }

    // Ensure interval stays within configured bounds
    adjusted_interval.clamp(params.min_interval, params.max_interval)
}

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
}

impl Default for SystemHistory {
    fn default() -> Self {
        Self {
            cpu_usage_history: VecDeque::new(),
            temperature_history: VecDeque::new(),
            last_user_activity: Instant::now(),
            last_battery_percentage: None,
            last_battery_timestamp: None,
            battery_discharge_rate: None,
            state_durations: std::collections::HashMap::new(),
            last_state_change: Instant::now(),
            current_state: SystemState::default(),
        }
    }
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
                    debug!("User activity detected based on CPU usage");
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
                    debug!("User activity detected based on temperature change");
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
                debug!("User activity detected based on system state change to {new_state:?}");
            }

            // Update state
            self.current_state = new_state;
            self.last_state_change = Instant::now();
        }

        // Check for significant load changes
        if report.system_load.load_avg_1min > 1.0 {
            self.last_user_activity = Instant::now();
            debug!("User activity detected based on system load");
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

    /// Calculate optimal polling interval based on system conditions
    fn calculate_optimal_interval(&self, config: &AppConfig, on_battery: bool) -> u64 {
        let params = IntervalParams {
            base_interval: config.daemon.poll_interval_sec,
            min_interval: config.daemon.min_poll_interval_sec,
            max_interval: config.daemon.max_poll_interval_sec,
            cpu_volatility: self.get_cpu_volatility(),
            temp_volatility: self.get_temperature_volatility(),
            battery_discharge_rate: self.battery_discharge_rate,
            last_user_activity: self.last_user_activity.elapsed(),
            is_system_idle: self.is_system_idle(),
            on_battery,
        };

        compute_new(&params)
    }
}

/// Run the daemon
pub fn run_daemon(mut config: AppConfig, verbose: bool) -> Result<(), AppError> {
    // Set effective log level based on config and verbose flag
    let effective_log_level = if verbose {
        LogLevel::Debug
    } else {
        config.daemon.log_level
    };

    // Get the appropriate level filter
    let level_filter = match effective_log_level {
        LogLevel::Error => LevelFilter::Error,
        LogLevel::Warning => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
    };

    // Update the log level filter if needed, without re-initializing the logger
    log::set_max_level(level_filter);

    info!("Starting superfreq daemon...");

    // Create a flag that will be set to true when a signal is received
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Set up signal handlers
    ctrlc::set_handler(move || {
        info!("Received shutdown signal, exiting...");
        r.store(false, Ordering::SeqCst);
    })
    .map_err(|e| AppError::Generic(format!("Error setting Ctrl-C handler: {e}")))?;

    info!(
        "Daemon initialized with poll interval: {}s",
        config.daemon.poll_interval_sec
    );

    // Set up stats file if configured
    if let Some(stats_path) = &config.daemon.stats_file_path {
        info!("Stats will be written to: {stats_path}");
    }

    // Initialize config file watcher if a path is available
    let config_file_path = if let Ok(path) = std::env::var("SUPERFREQ_CONFIG") {
        Some(path)
    } else {
        // Check standard config paths
        let default_paths = ["/etc/xdg/superfreq/config.toml", "/etc/superfreq.toml"];

        default_paths
            .iter()
            .find(|&path| std::path::Path::new(path).exists())
            .map(|path| (*path).to_string())
    };

    let mut config_watcher = if let Some(path) = config_file_path {
        match ConfigWatcher::new(&path) {
            Ok(watcher) => {
                info!("Watching config file: {path}");
                Some(watcher)
            }
            Err(e) => {
                warn!("Failed to initialize config file watcher: {e}");
                None
            }
        }
    } else {
        warn!("No config file found to watch for changes.");
        None
    };

    // Variables for adaptive polling
    // Make sure that the poll interval is *never* zero to prevent a busy loop
    let mut current_poll_interval = config.daemon.poll_interval_sec.max(1);
    if config.daemon.poll_interval_sec == 0 {
        warn!("Poll interval is set to zero in config, using 1s minimum to prevent a busy loop");
    }
    let mut system_history = SystemHistory::default();

    // Main loop
    while running.load(Ordering::SeqCst) {
        let start_time = Instant::now();

        // Check for configuration changes
        if let Some(watcher) = &mut config_watcher {
            if let Some(config_result) = watcher.check_for_changes() {
                match config_result {
                    Ok(new_config) => {
                        info!("Config file changed, updating configuration");
                        config = new_config;
                        // Reset polling interval after config change
                        current_poll_interval = config.daemon.poll_interval_sec.max(1);
                        if config.daemon.poll_interval_sec == 0 {
                            warn!(
                                "Poll interval is set to zero in updated config, using 1s minimum to prevent a busy loop"
                            );
                        }
                        // Mark this as a system event for adaptive polling
                        system_history.last_user_activity = Instant::now();
                    }
                    Err(e) => {
                        error!("Error loading new configuration: {e}");
                        // Continue with existing config
                    }
                }
            }
        }

        match monitor::collect_system_report(&config) {
            Ok(report) => {
                debug!("Collected system report, applying settings...");

                // Store the current state before updating history
                let previous_state = system_history.current_state.clone();

                // Update system history with new data
                system_history.update(&report);

                // Update the stats file if configured
                if let Some(stats_path) = &config.daemon.stats_file_path {
                    if let Err(e) = write_stats_file(stats_path, &report) {
                        error!("Failed to write stats file: {e}");
                    }
                }

                match engine::determine_and_apply_settings(&report, &config, None) {
                    Ok(()) => {
                        debug!("Successfully applied system settings");

                        // If system state changed, log the new state
                        if system_history.current_state != previous_state {
                            info!(
                                "System state changed to: {:?}",
                                system_history.current_state
                            );
                        }
                    }
                    Err(e) => {
                        error!("Error applying system settings: {e}");
                    }
                }

                // Check if we're on battery
                let on_battery = !report.batteries.is_empty()
                    && report.batteries.first().is_some_and(|b| !b.ac_connected);

                // Calculate optimal polling interval if adaptive polling is enabled
                if config.daemon.adaptive_interval {
                    let optimal_interval =
                        system_history.calculate_optimal_interval(&config, on_battery);

                    // Don't change the interval too dramatically at once
                    if optimal_interval > current_poll_interval {
                        current_poll_interval = (current_poll_interval + optimal_interval) / 2;
                    } else if optimal_interval < current_poll_interval {
                        current_poll_interval = current_poll_interval
                            - ((current_poll_interval - optimal_interval) / 2).max(1);
                    }

                    // Make sure that we respect the (user) configured min and max limits
                    current_poll_interval = current_poll_interval.clamp(
                        config.daemon.min_poll_interval_sec,
                        config.daemon.max_poll_interval_sec,
                    );

                    debug!("Adaptive polling: set interval to {current_poll_interval}s");
                } else {
                    // If adaptive polling is disabled, still apply battery-saving adjustment
                    if config.daemon.throttle_on_battery && on_battery {
                        let battery_multiplier = 2; // poll half as often on battery

                        // We need to make sure `poll_interval_sec` is *at least* 1
                        // before multiplying.
                        let safe_interval = config.daemon.poll_interval_sec.max(1);
                        current_poll_interval = (safe_interval * battery_multiplier)
                            .min(config.daemon.max_poll_interval_sec);

                        debug!(
                            "On battery power, increased poll interval to {current_poll_interval}s"
                        );
                    } else {
                        // Use the configured poll interval
                        current_poll_interval = config.daemon.poll_interval_sec.max(1);
                        if config.daemon.poll_interval_sec == 0 {
                            debug!("Using minimum poll interval of 1s instead of configured 0s");
                        }
                    }
                }
            }
            Err(e) => {
                error!("Error collecting system report: {e}");
            }
        }

        // Sleep for the remaining time in the poll interval
        let elapsed = start_time.elapsed();
        let poll_duration = Duration::from_secs(current_poll_interval);
        if elapsed < poll_duration {
            let sleep_time = poll_duration - elapsed;
            debug!("Sleeping for {}s until next cycle", sleep_time.as_secs());
            std::thread::sleep(sleep_time);
        }
    }

    info!("Daemon stopped");
    Ok(())
}

/// Write current system stats to a file for --stats to read
fn write_stats_file(path: &str, report: &SystemReport) -> Result<(), std::io::Error> {
    let mut file = File::create(path)?;

    writeln!(file, "timestamp={:?}", report.timestamp)?;

    // CPU info
    writeln!(file, "governor={:?}", report.cpu_global.current_governor)?;
    writeln!(file, "turbo={:?}", report.cpu_global.turbo_status)?;

    if let Some(temp) = report.cpu_global.average_temperature_celsius {
        writeln!(file, "cpu_temp={temp:.1}")?;
    }

    // Battery info
    if !report.batteries.is_empty() {
        let battery = &report.batteries[0];
        writeln!(file, "ac_power={}", battery.ac_connected)?;
        if let Some(cap) = battery.capacity_percent {
            writeln!(file, "battery_percent={cap}")?;
        }
    }

    // System load
    writeln!(file, "load_1m={:.2}", report.system_load.load_avg_1min)?;
    writeln!(file, "load_5m={:.2}", report.system_load.load_avg_5min)?;
    writeln!(file, "load_15m={:.2}", report.system_load.load_avg_15min)?;

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
