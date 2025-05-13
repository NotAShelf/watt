use crate::config::{AppConfig, ProfileConfig};
use crate::core::{OperationalMode, SystemReport, TurboSetting};
use crate::cpu::{self};
use crate::util::error::ControlError;

#[derive(Debug)]
pub enum EngineError {
    ControlError(ControlError),
    ConfigurationError(String),
}

impl From<ControlError> for EngineError {
    fn from(err: ControlError) -> Self {
        EngineError::ControlError(err)
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::ControlError(e) => write!(f, "CPU control error: {}", e),
            EngineError::ConfigurationError(s) => write!(f, "Configuration error: {}", s),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::ControlError(e) => Some(e),
            EngineError::ConfigurationError(_) => None,
        }
    }
}

/// Determines the appropriate CPU profile based on power status or forced mode,
/// and applies the settings using functions from the `cpu` module.
pub fn determine_and_apply_settings(
    report: &SystemReport,
    config: &AppConfig,
    force_mode: Option<OperationalMode>,
) -> Result<(), EngineError> {
    let selected_profile_config: &ProfileConfig;

    if let Some(mode) = force_mode {
        match mode {
            OperationalMode::Powersave => {
                println!("Engine: Forced Powersave mode selected. Applying 'battery' profile.");
                selected_profile_config = &config.battery;
            }
            OperationalMode::Performance => {
                println!("Engine: Forced Performance mode selected. Applying 'charger' profile.");
                selected_profile_config = &config.charger;
            }
        }
    } else {
        // Determine AC/Battery status
        // If no batteries, assume AC power (desktop).
        // Otherwise, check the ac_connected status from the (first) battery.
        // XXX: This relies on the setting ac_connected in BatteryInfo being set correctly.
        let on_ac_power = report.batteries.is_empty() ||
                          report.batteries.first().map_or(false, |b| b.ac_connected);

        if on_ac_power {
            println!("Engine: On AC power, selecting Charger profile.");
            selected_profile_config = &config.charger;
        } else {
            println!("Engine: On Battery power, selecting Battery profile.");
            selected_profile_config = &config.battery;
        }
    }

    // Apply settings from selected_profile_config
    // TODO: The println! statements are for temporary debugging/logging
    // and we'd like to replace them with proper logging in the future.

    if let Some(governor) = &selected_profile_config.governor {
        println!("Engine: Setting governor to '{}'", governor);
        cpu::set_governor(governor, None)?;
    }

    if let Some(turbo_setting) = selected_profile_config.turbo {
        println!("Engine: Setting turbo to '{:?}'", turbo_setting);
        cpu::set_turbo(turbo_setting)?;
    }

    if let Some(epp) = &selected_profile_config.epp {
        println!("Engine: Setting EPP to '{}'", epp);
        cpu::set_epp(epp, None)?;
    }

    if let Some(epb) = &selected_profile_config.epb {
         println!("Engine: Setting EPB to '{}'", epb);
        cpu::set_epb(epb, None)?;
    }

    if let Some(min_freq) = selected_profile_config.min_freq_mhz {
        println!("Engine: Setting min frequency to '{} MHz'", min_freq);
        cpu::set_min_frequency(min_freq, None)?;
    }

    if let Some(max_freq) = selected_profile_config.max_freq_mhz {
        println!("Engine: Setting max frequency to '{} MHz'", max_freq);
        cpu::set_max_frequency(max_freq, None)?;
    }

    if let Some(profile) = &selected_profile_config.platform_profile {
        println!("Engine: Setting platform profile to '{}'", profile);
        cpu::set_platform_profile(profile)?;
    }

    println!("Engine: Profile settings applied successfully.");

    Ok(())
}
