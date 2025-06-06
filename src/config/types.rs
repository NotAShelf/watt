// Configuration types and structures for superfreq
use crate::core::TurboSetting;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;

/// Defines constant-returning functions used for default values.
/// This hopefully reduces repetition since we have way too many default functions
/// that just return constants.
macro_rules! default_const {
    ($name:ident, $type:ty, $value:expr) => {
        const fn $name() -> $type {
            $value
        }
    };
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BatteryChargeThresholds {
    pub start: u8,
    pub stop: u8,
}

impl BatteryChargeThresholds {
    pub fn new(start: u8, stop: u8) -> Result<Self, ConfigError> {
        if stop == 0 {
            return Err(ConfigError::Validation(
                "Stop threshold must be greater than 0%".to_string(),
            ));
        }
        if start >= stop {
            return Err(ConfigError::Validation(format!(
                "Start threshold ({start}) must be less than stop threshold ({stop})"
            )));
        }
        if stop > 100 {
            return Err(ConfigError::Validation(format!(
                "Stop threshold ({stop}) cannot exceed 100%"
            )));
        }

        Ok(Self { start, stop })
    }
}

impl TryFrom<(u8, u8)> for BatteryChargeThresholds {
    type Error = ConfigError;

    fn try_from(values: (u8, u8)) -> Result<Self, Self::Error> {
        let (start, stop) = values;
        Self::new(start, stop)
    }
}

// Structs for configuration using serde::Deserialize
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ProfileConfig {
    pub governor: Option<String>,
    pub turbo: Option<TurboSetting>,
    pub epp: Option<String>, // Energy Performance Preference (EPP)
    pub epb: Option<String>, // Energy Performance Bias (EPB) - usually an integer, but string for flexibility from sysfs
    pub min_freq_mhz: Option<u32>,
    pub max_freq_mhz: Option<u32>,
    pub platform_profile: Option<String>,
    #[serde(default)]
    pub turbo_auto_settings: TurboAutoSettings,
    #[serde(default)]
    pub enable_auto_turbo: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_charge_thresholds: Option<BatteryChargeThresholds>,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            governor: Some("schedutil".to_string()), // common sensible default (?)
            turbo: Some(TurboSetting::Auto),
            epp: None,              // defaults depend on governor and system
            epb: None,              // defaults depend on governor and system
            min_freq_mhz: None,     // no override
            max_freq_mhz: None,     // no override
            platform_profile: None, // no override
            turbo_auto_settings: TurboAutoSettings::default(),
            enable_auto_turbo: default_enable_auto_turbo(),
            battery_charge_thresholds: None,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub charger: ProfileConfig,
    #[serde(default)]
    pub battery: ProfileConfig,
    pub ignored_power_supplies: Option<Vec<String>>,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

// Error type for config loading
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parsing error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Configuration validation error: {0}")]
    Validation(String),
}

// Intermediate structs for TOML parsing
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ProfileConfigToml {
    pub governor: Option<String>,
    pub turbo: Option<String>, // "always", "auto", "never"
    pub epp: Option<String>,
    pub epb: Option<String>,
    pub min_freq_mhz: Option<u32>,
    pub max_freq_mhz: Option<u32>,
    pub platform_profile: Option<String>,
    pub turbo_auto_settings: Option<TurboAutoSettings>,
    #[serde(default = "default_enable_auto_turbo")]
    pub enable_auto_turbo: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_charge_thresholds: Option<BatteryChargeThresholds>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct AppConfigToml {
    #[serde(default)]
    pub charger: ProfileConfigToml,
    #[serde(default)]
    pub battery: ProfileConfigToml,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_charge_thresholds: Option<BatteryChargeThresholds>,
    pub ignored_power_supplies: Option<Vec<String>>,
    #[serde(default)]
    pub daemon: DaemonConfigToml,
}

impl Default for ProfileConfigToml {
    fn default() -> Self {
        Self {
            governor: Some("schedutil".to_string()),
            turbo: Some("auto".to_string()),
            epp: None,
            epb: None,
            min_freq_mhz: None,
            max_freq_mhz: None,
            platform_profile: None,
            turbo_auto_settings: None,
            enable_auto_turbo: default_enable_auto_turbo(),
            battery_charge_thresholds: None,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TurboAutoSettings {
    #[serde(default = "default_load_threshold_high")]
    pub load_threshold_high: f32,
    #[serde(default = "default_load_threshold_low")]
    pub load_threshold_low: f32,
    #[serde(default = "default_temp_threshold_high")]
    pub temp_threshold_high: f32,
    /// Initial turbo boost state when no previous state exists.
    /// Set to `true` to start with turbo enabled, `false` to start with turbo disabled.
    /// This is only used at first launch or after a reset.
    #[serde(default = "default_initial_turbo_state")]
    pub initial_turbo_state: bool,
}

// Default thresholds for Auto turbo mode
pub const DEFAULT_LOAD_THRESHOLD_HIGH: f32 = 70.0; // enable turbo if load is above this
pub const DEFAULT_LOAD_THRESHOLD_LOW: f32 = 30.0; // disable turbo if load is below this
pub const DEFAULT_TEMP_THRESHOLD_HIGH: f32 = 75.0; // disable turbo if temperature is above this
pub const DEFAULT_INITIAL_TURBO_STATE: bool = false; // by default, start with turbo disabled

default_const!(
    default_load_threshold_high,
    f32,
    DEFAULT_LOAD_THRESHOLD_HIGH
);
default_const!(default_load_threshold_low, f32, DEFAULT_LOAD_THRESHOLD_LOW);
default_const!(
    default_temp_threshold_high,
    f32,
    DEFAULT_TEMP_THRESHOLD_HIGH
);
default_const!(
    default_initial_turbo_state,
    bool,
    DEFAULT_INITIAL_TURBO_STATE
);

impl Default for TurboAutoSettings {
    fn default() -> Self {
        Self {
            load_threshold_high: DEFAULT_LOAD_THRESHOLD_HIGH,
            load_threshold_low: DEFAULT_LOAD_THRESHOLD_LOW,
            temp_threshold_high: DEFAULT_TEMP_THRESHOLD_HIGH,
            initial_turbo_state: DEFAULT_INITIAL_TURBO_STATE,
        }
    }
}

impl From<ProfileConfigToml> for ProfileConfig {
    fn from(toml_config: ProfileConfigToml) -> Self {
        Self {
            governor: toml_config.governor,
            turbo: toml_config
                .turbo
                .and_then(|s| match s.to_lowercase().as_str() {
                    "always" => Some(TurboSetting::Always),
                    "auto" => Some(TurboSetting::Auto),
                    "never" => Some(TurboSetting::Never),
                    _ => None,
                }),
            epp: toml_config.epp,
            epb: toml_config.epb,
            min_freq_mhz: toml_config.min_freq_mhz,
            max_freq_mhz: toml_config.max_freq_mhz,
            platform_profile: toml_config.platform_profile,
            turbo_auto_settings: toml_config.turbo_auto_settings.unwrap_or_default(),
            enable_auto_turbo: toml_config.enable_auto_turbo,
            battery_charge_thresholds: toml_config.battery_charge_thresholds,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DaemonConfig {
    #[serde(default = "default_poll_interval_sec")]
    pub poll_interval_sec: u64,
    #[serde(default = "default_adaptive_interval")]
    pub adaptive_interval: bool,
    #[serde(default = "default_min_poll_interval_sec")]
    pub min_poll_interval_sec: u64,
    #[serde(default = "default_max_poll_interval_sec")]
    pub max_poll_interval_sec: u64,
    #[serde(default = "default_throttle_on_battery")]
    pub throttle_on_battery: bool,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default = "default_stats_file_path")]
    pub stats_file_path: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warning,
    Info,
    Debug,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_sec: default_poll_interval_sec(),
            adaptive_interval: default_adaptive_interval(),
            min_poll_interval_sec: default_min_poll_interval_sec(),
            max_poll_interval_sec: default_max_poll_interval_sec(),
            throttle_on_battery: default_throttle_on_battery(),
            log_level: default_log_level(),
            stats_file_path: default_stats_file_path(),
        }
    }
}

default_const!(default_poll_interval_sec, u64, 5);
default_const!(default_adaptive_interval, bool, false);
default_const!(default_min_poll_interval_sec, u64, 1);
default_const!(default_max_poll_interval_sec, u64, 30);
default_const!(default_throttle_on_battery, bool, true);
default_const!(default_log_level, LogLevel, LogLevel::Info);
default_const!(default_stats_file_path, Option<String>, None);
default_const!(default_enable_auto_turbo, bool, true);

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DaemonConfigToml {
    #[serde(default = "default_poll_interval_sec")]
    pub poll_interval_sec: u64,
    #[serde(default = "default_adaptive_interval")]
    pub adaptive_interval: bool,
    #[serde(default = "default_min_poll_interval_sec")]
    pub min_poll_interval_sec: u64,
    #[serde(default = "default_max_poll_interval_sec")]
    pub max_poll_interval_sec: u64,
    #[serde(default = "default_throttle_on_battery")]
    pub throttle_on_battery: bool,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default = "default_stats_file_path")]
    pub stats_file_path: Option<String>,
}

impl Default for DaemonConfigToml {
    fn default() -> Self {
        Self {
            poll_interval_sec: default_poll_interval_sec(),
            adaptive_interval: default_adaptive_interval(),
            min_poll_interval_sec: default_min_poll_interval_sec(),
            max_poll_interval_sec: default_max_poll_interval_sec(),
            throttle_on_battery: default_throttle_on_battery(),
            log_level: default_log_level(),
            stats_file_path: default_stats_file_path(),
        }
    }
}
