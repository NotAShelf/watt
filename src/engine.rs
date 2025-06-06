use crate::battery;
use crate::config::{AppConfig, ProfileConfig, TurboAutoSettings};
use crate::core::{OperationalMode, SystemReport, TurboSetting};
use crate::cpu::{self};
use crate::util::error::{ControlError, EngineError};
use log::{debug, info, warn};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Track turbo boost state for AC and battery power modes
struct TurboHysteresisStates {
    /// State for when on AC power
    charger: TurboHysteresis,
    /// State for when on battery power
    battery: TurboHysteresis,
}

impl TurboHysteresisStates {
    const fn new() -> Self {
        Self {
            charger: TurboHysteresis::new(),
            battery: TurboHysteresis::new(),
        }
    }

    const fn get_for_power_state(&self, is_on_ac: bool) -> &TurboHysteresis {
        if is_on_ac {
            &self.charger
        } else {
            &self.battery
        }
    }
}

static TURBO_STATES: OnceLock<TurboHysteresisStates> = OnceLock::new();

/// Get or initialize the global turbo states
fn get_turbo_states() -> &'static TurboHysteresisStates {
    TURBO_STATES.get_or_init(TurboHysteresisStates::new)
}

/// Manage turbo boost hysteresis state.
/// Contains the state needed to implement hysteresis
/// for the dynamic turbo management feature
struct TurboHysteresis {
    /// Whether turbo was enabled in the previous cycle
    previous_state: AtomicBool,
    /// Whether the hysteresis state has been initialized
    initialized: AtomicBool,
}

impl TurboHysteresis {
    const fn new() -> Self {
        Self {
            previous_state: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        }
    }

    /// Get the previous turbo state, if initialized
    fn get_previous_state(&self) -> Option<bool> {
        if self.initialized.load(Ordering::Acquire) {
            Some(self.previous_state.load(Ordering::Acquire))
        } else {
            None
        }
    }

    /// Initialize the state with a specific value if not already initialized
    /// Only one thread should be able to initialize the state
    fn initialize_with(&self, initial_state: bool) -> bool {
        // First, try to atomically change initialized from false to true
        // Only one thread can win the initialization race
        match self.initialized.compare_exchange(
            false,             // expected: not initialized
            true,              // desired: mark as initialized
            Ordering::Release, // success: release for memory visibility
            Ordering::Acquire, // failure: just need to acquire the current value
        ) {
            Ok(_) => {
                // We won the race to initialize
                // Now it's safe to set the initial state since we know we're the only
                // thread that has successfully marked this as initialized
                self.previous_state.store(initial_state, Ordering::Release);
                initial_state
            }
            Err(_) => {
                // Another thread already initialized it.
                // Just read the current state value that was set by the winning thread
                self.previous_state.load(Ordering::Acquire)
            }
        }
    }

    /// Update the turbo state for hysteresis
    fn update_state(&self, new_state: bool) {
        // First store the new state, then mark as initialized
        // With this, any thread seeing initialized=true will also see the correct state
        self.previous_state.store(new_state, Ordering::Release);

        // Already initialized, no need for compare_exchange
        if self.initialized.load(Ordering::Relaxed) {
            return;
        }

        // Otherwise, try to set initialized=true (but only if it was false)
        self.initialized
            .compare_exchange(
                false,             // expected: not initialized
                true,              // desired: mark as initialized
                Ordering::Release, // success: release for memory visibility
                Ordering::Relaxed, // failure: we don't care about the current value on failure
            )
            .ok(); // Ignore the result. If it fails, it means another thread already initialized it
    }
}

/// Try applying a CPU feature and handle common error cases. Centralizes the where we
/// previously did:
/// 1. Try to apply a feature setting
/// 2. If not supported, log a warning and continue
/// 3. If other error, propagate the error
fn try_apply_feature<F, T>(
    feature_name: &str,
    value_description: &str,
    apply_fn: F,
) -> Result<(), EngineError>
where
    F: FnOnce() -> Result<T, ControlError>,
{
    info!("Setting {feature_name} to '{value_description}'");

    match apply_fn() {
        Ok(_) => Ok(()),
        Err(e) => {
            if matches!(e, ControlError::NotSupported(_)) {
                warn!(
                    "{feature_name} setting is not supported on this system. Skipping {feature_name} configuration."
                );
                Ok(())
            } else {
                // Propagate all other errors, including InvalidValueError
                Err(EngineError::ControlError(e))
            }
        }
    }
}

/// Determines the appropriate CPU profile based on power status or forced mode,
/// and applies the settings (via helpers defined in the `cpu` module)
pub fn determine_and_apply_settings(
    report: &SystemReport,
    config: &AppConfig,
    force_mode: Option<OperationalMode>,
) -> Result<(), EngineError> {
    // First, check if there's a governor override set
    if let Some(override_governor) = cpu::get_governor_override() {
        info!(
            "Governor override is active: '{}'. Setting governor.",
            override_governor.trim()
        );

        // Apply the override governor setting
        try_apply_feature("override governor", override_governor.trim(), || {
            cpu::set_governor(override_governor.trim(), None)
        })?;
    }

    // Determine AC/Battery status once, early in the function
    // For desktops (no batteries), we should always use the AC power profile
    // For laptops, we check if all batteries report connected to AC
    let on_ac_power = if report.batteries.is_empty() {
        // No batteries means desktop/server, always on AC
        true
    } else {
        // Check if all batteries report AC connected
        report.batteries.iter().all(|b| b.ac_connected)
    };

    let selected_profile_config: &ProfileConfig;

    if let Some(mode) = force_mode {
        match mode {
            OperationalMode::Powersave => {
                info!("Forced Powersave mode selected. Applying 'battery' profile.");
                selected_profile_config = &config.battery;
            }
            OperationalMode::Performance => {
                info!("Forced Performance mode selected. Applying 'charger' profile.");
                selected_profile_config = &config.charger;
            }
        }
    } else {
        // Use the previously computed on_ac_power value
        if on_ac_power {
            info!("On AC power, selecting Charger profile.");
            selected_profile_config = &config.charger;
        } else {
            info!("On Battery power, selecting Battery profile.");
            selected_profile_config = &config.battery;
        }
    }

    // Apply settings from selected_profile_config
    if let Some(governor) = &selected_profile_config.governor {
        info!("Setting governor to '{governor}'");
        // Let set_governor handle the validation
        if let Err(e) = cpu::set_governor(governor, None) {
            // If the governor is not available, log a warning
            if matches!(e, ControlError::InvalidGovernor(_))
                || matches!(e, ControlError::NotSupported(_))
            {
                warn!(
                    "Configured governor '{governor}' is not available on this system. Skipping."
                );
            } else {
                return Err(e.into());
            }
        }
    }

    if let Some(turbo_setting) = selected_profile_config.turbo {
        info!("Setting turbo to '{turbo_setting:?}'");
        match turbo_setting {
            TurboSetting::Auto => {
                if selected_profile_config.enable_auto_turbo {
                    debug!("Managing turbo in auto mode based on system conditions");
                    manage_auto_turbo(report, selected_profile_config, on_ac_power)?;
                } else {
                    debug!(
                        "Superfreq's dynamic turbo management is disabled by configuration. Ensuring system uses its default behavior for automatic turbo control."
                    );
                    // Make sure the system is set to its default automatic turbo mode.
                    // This is important if turbo was previously forced off.
                    try_apply_feature("Turbo boost", "system default (Auto)", || {
                        cpu::set_turbo(TurboSetting::Auto)
                    })?;
                }
            }
            _ => {
                try_apply_feature("Turbo boost", &format!("{turbo_setting:?}"), || {
                    cpu::set_turbo(turbo_setting)
                })?;
            }
        }
    }

    if let Some(epp) = &selected_profile_config.epp {
        try_apply_feature("EPP", epp, || cpu::set_epp(epp, None))?;
    }

    if let Some(epb) = &selected_profile_config.epb {
        try_apply_feature("EPB", epb, || cpu::set_epb(epb, None))?;
    }

    if let Some(min_freq) = selected_profile_config.min_freq_mhz {
        try_apply_feature("min frequency", &format!("{min_freq} MHz"), || {
            cpu::set_min_frequency(min_freq, None)
        })?;
    }

    if let Some(max_freq) = selected_profile_config.max_freq_mhz {
        try_apply_feature("max frequency", &format!("{max_freq} MHz"), || {
            cpu::set_max_frequency(max_freq, None)
        })?;
    }

    if let Some(profile) = &selected_profile_config.platform_profile {
        try_apply_feature("platform profile", profile, || {
            cpu::set_platform_profile(profile)
        })?;
    }

    // Set battery charge thresholds if configured
    if let Some(thresholds) = &selected_profile_config.battery_charge_thresholds {
        let start_threshold = thresholds.start;
        let stop_threshold = thresholds.stop;

        if start_threshold < stop_threshold && stop_threshold <= 100 {
            info!("Setting battery charge thresholds: {start_threshold}-{stop_threshold}%");
            match battery::set_battery_charge_thresholds(start_threshold, stop_threshold) {
                Ok(()) => debug!("Battery charge thresholds set successfully"),
                Err(e) => warn!("Failed to set battery charge thresholds: {e}"),
            }
        } else {
            warn!(
                "Invalid battery threshold values: start={start_threshold}, stop={stop_threshold}"
            );
        }
    }

    debug!("Profile settings applied successfully.");

    Ok(())
}

fn manage_auto_turbo(
    report: &SystemReport,
    config: &ProfileConfig,
    on_ac_power: bool,
) -> Result<(), EngineError> {
    // Get the auto turbo settings from the config
    let turbo_settings = &config.turbo_auto_settings;

    // Validate the complete configuration to ensure it's usable
    validate_turbo_auto_settings(turbo_settings)?;

    // Get average CPU temperature and CPU load
    let cpu_temp = report.cpu_global.average_temperature_celsius;

    // Check if we have CPU usage data available
    let avg_cpu_usage = if report.cpu_cores.is_empty() {
        None
    } else {
        let sum: f32 = report
            .cpu_cores
            .iter()
            .filter_map(|core| core.usage_percent)
            .sum();
        let count = report
            .cpu_cores
            .iter()
            .filter(|core| core.usage_percent.is_some())
            .count();

        if count > 0 {
            Some(sum / count as f32)
        } else {
            None
        }
    };

    // Get the previous state or initialize with the configured initial state
    let previous_turbo_enabled = {
        let turbo_states = get_turbo_states();
        let hysteresis = turbo_states.get_for_power_state(on_ac_power);
        if let Some(state) = hysteresis.get_previous_state() {
            state
        } else {
            // Initialize with the configured initial state and return it
            hysteresis.initialize_with(turbo_settings.initial_turbo_state)
        }
    };

    // Decision logic for enabling/disabling turbo with hysteresis
    let enable_turbo = match (cpu_temp, avg_cpu_usage, previous_turbo_enabled) {
        // If temperature is too high, disable turbo regardless of load
        (Some(temp), _, _) if temp >= turbo_settings.temp_threshold_high => {
            info!(
                "Auto Turbo: Disabled due to high temperature ({:.1}°C >= {:.1}°C)",
                temp, turbo_settings.temp_threshold_high
            );
            false
        }

        // If load is high enough, enable turbo (unless temp already caused it to disable)
        (_, Some(usage), _) if usage >= turbo_settings.load_threshold_high => {
            info!(
                "Auto Turbo: Enabled due to high CPU load ({:.1}% >= {:.1}%)",
                usage, turbo_settings.load_threshold_high
            );
            true
        }

        // If load is low, disable turbo
        (_, Some(usage), _) if usage <= turbo_settings.load_threshold_low => {
            info!(
                "Auto Turbo: Disabled due to low CPU load ({:.1}% <= {:.1}%)",
                usage, turbo_settings.load_threshold_low
            );
            false
        }

        // In intermediate load range, maintain previous state (hysteresis)
        (_, Some(usage), prev_state)
            if usage > turbo_settings.load_threshold_low
                && usage < turbo_settings.load_threshold_high =>
        {
            info!(
                "Auto Turbo: Maintaining previous state ({}) due to intermediate load ({:.1}%)",
                if prev_state { "enabled" } else { "disabled" },
                usage
            );
            prev_state
        }

        // When CPU load data is present but temperature is missing, use the same hysteresis logic
        (None, Some(usage), prev_state) => {
            info!(
                "Auto Turbo: Maintaining previous state ({}) due to missing temperature data (load: {:.1}%)",
                if prev_state { "enabled" } else { "disabled" },
                usage
            );
            prev_state
        }

        // When all metrics are missing, maintain the previous state
        (None, None, prev_state) => {
            info!(
                "Auto Turbo: Maintaining previous state ({}) due to missing all CPU metrics",
                if prev_state { "enabled" } else { "disabled" }
            );
            prev_state
        }

        // Any other cases with partial metrics, maintain previous state for stability
        (_, _, prev_state) => {
            info!(
                "Auto Turbo: Maintaining previous state ({}) due to incomplete CPU metrics",
                if prev_state { "enabled" } else { "disabled" }
            );
            prev_state
        }
    };

    // Save the current state for next time
    {
        let turbo_states = get_turbo_states();
        let hysteresis = turbo_states.get_for_power_state(on_ac_power);
        hysteresis.update_state(enable_turbo);
    }

    // Only apply the setting if the state has changed
    let changed = previous_turbo_enabled != enable_turbo;
    if changed {
        let turbo_setting = if enable_turbo {
            TurboSetting::Always
        } else {
            TurboSetting::Never
        };

        info!(
            "Auto Turbo: Applying turbo change from {} to {}",
            if previous_turbo_enabled {
                "enabled"
            } else {
                "disabled"
            },
            if enable_turbo { "enabled" } else { "disabled" }
        );

        match cpu::set_turbo(turbo_setting) {
            Ok(()) => {
                debug!(
                    "Auto Turbo: Successfully set turbo to {}",
                    if enable_turbo { "enabled" } else { "disabled" }
                );
                Ok(())
            }
            Err(e) => Err(EngineError::ControlError(e)),
        }
    } else {
        debug!(
            "Auto Turbo: Maintaining turbo state ({}) - no change needed",
            if enable_turbo { "enabled" } else { "disabled" }
        );
        Ok(())
    }
}

fn validate_turbo_auto_settings(settings: &TurboAutoSettings) -> Result<(), EngineError> {
    if settings.load_threshold_high <= settings.load_threshold_low
        || settings.load_threshold_high > 100.0
        || settings.load_threshold_high < 0.0
        || settings.load_threshold_low < 0.0
        || settings.load_threshold_low > 100.0
    {
        return Err(EngineError::ConfigurationError(
            "Invalid turbo auto settings: load thresholds must be between 0 % and 100 % with high > low"
                .to_string(),
        ));
    }

    // Validate temperature threshold (realistic range for CPU temps in Celsius)
    // TODO: different CPUs have different temperature thresholds. While 110 is a good example
    // "extreme" case, the upper barrier might be *lower* for some devices. We'll want to fix
    // this eventually, or make it configurable.
    if settings.temp_threshold_high <= 0.0 || settings.temp_threshold_high > 110.0 {
        return Err(EngineError::ConfigurationError(
            "Invalid turbo auto settings: temperature threshold must be between 0°C and 110°C"
                .to_string(),
        ));
    }

    Ok(())
}
