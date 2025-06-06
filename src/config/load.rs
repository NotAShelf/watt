// Configuration loading functionality
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::types::{AppConfig, AppConfigToml, ConfigError, DaemonConfig, ProfileConfig};

/// The primary function to load application configuration from a specific path or from default locations.
///
/// # Arguments
///
/// * `specific_path` - If provided, only attempts to load from this path and errors if not found
///
/// # Returns
///
/// * `Ok(AppConfig)` - Successfully loaded configuration
/// * `Err(ConfigError)` - Error loading or parsing configuration
pub fn load_config() -> Result<AppConfig, ConfigError> {
    load_config_from_path(None)
}

/// Load configuration from a specific path or try default paths
pub fn load_config_from_path(specific_path: Option<&str>) -> Result<AppConfig, ConfigError> {
    // If a specific path is provided, only try that one
    if let Some(path_str) = specific_path {
        let path = Path::new(path_str);
        if path.exists() {
            return load_and_parse_config(path);
        }
        return Err(ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Specified config file not found: {}", path.display()),
        )));
    }

    // Check for SUPERFREQ_CONFIG environment variable
    if let Ok(env_path) = std::env::var("SUPERFREQ_CONFIG") {
        let env_path = Path::new(&env_path);
        if env_path.exists() {
            println!(
                "Loading config from SUPERFREQ_CONFIG: {}",
                env_path.display()
            );
            return load_and_parse_config(env_path);
        }
        eprintln!(
            "Warning: Config file specified by SUPERFREQ_CONFIG not found: {}",
            env_path.display()
        );
    }

    // System-wide paths
    let config_paths = vec![
        PathBuf::from("/etc/xdg/superfreq/config.toml"),
        PathBuf::from("/etc/superfreq.toml"),
    ];

    for path in config_paths {
        if path.exists() {
            println!("Loading config from: {}", path.display());
            match load_and_parse_config(&path) {
                Ok(config) => return Ok(config),
                Err(e) => {
                    eprintln!("Error with config file {}: {}", path.display(), e);
                    // Continue trying other files
                }
            }
        }
    }

    println!("No configuration file found or all failed to parse. Using default configuration.");
    // Construct default AppConfig by converting default AppConfigToml
    let default_toml_config = AppConfigToml::default();
    Ok(AppConfig {
        charger: ProfileConfig::from(default_toml_config.charger),
        battery: ProfileConfig::from(default_toml_config.battery),
        ignored_power_supplies: default_toml_config.ignored_power_supplies,
        daemon: DaemonConfig::default(),
    })
}

/// Load and parse a configuration file
fn load_and_parse_config(path: &Path) -> Result<AppConfig, ConfigError> {
    let contents = fs::read_to_string(path).map_err(ConfigError::Io)?;

    let toml_app_config = toml::from_str::<AppConfigToml>(&contents).map_err(ConfigError::Toml)?;

    // Handle inheritance of values from global to profile configs
    let mut charger_profile = toml_app_config.charger.clone();
    let mut battery_profile = toml_app_config.battery.clone();

    // Clone global battery_charge_thresholds once if it exists
    if let Some(global_thresholds) = toml_app_config.battery_charge_thresholds {
        // Apply to charger profile if not already set
        if charger_profile.battery_charge_thresholds.is_none() {
            charger_profile.battery_charge_thresholds = Some(global_thresholds.clone());
        }

        // Apply to battery profile if not already set
        if battery_profile.battery_charge_thresholds.is_none() {
            battery_profile.battery_charge_thresholds = Some(global_thresholds);
        }
    }

    // Convert AppConfigToml to AppConfig
    Ok(AppConfig {
        charger: ProfileConfig::from(charger_profile),
        battery: ProfileConfig::from(battery_profile),
        ignored_power_supplies: toml_app_config.ignored_power_supplies,
        daemon: DaemonConfig {
            poll_interval_sec: toml_app_config.daemon.poll_interval_sec,
            adaptive_interval: toml_app_config.daemon.adaptive_interval,
            min_poll_interval_sec: toml_app_config.daemon.min_poll_interval_sec,
            max_poll_interval_sec: toml_app_config.daemon.max_poll_interval_sec,
            throttle_on_battery: toml_app_config.daemon.throttle_on_battery,
            log_level: toml_app_config.daemon.log_level,
            stats_file_path: toml_app_config.daemon.stats_file_path,
        },
    })
}
