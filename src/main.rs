mod battery;
mod cli;
mod config;
mod core;
mod cpu;
mod daemon;
mod engine;
mod monitor;
mod util;

use crate::config::AppConfig;
use crate::core::{GovernorOverrideMode, TurboSetting};
use crate::util::error::{AppError, ControlError};
use clap::{Parser, value_parser};
use env_logger::Builder;
use log::{debug, error, info};
use std::error::Error;
use std::sync::Once;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Display current system information
    Info,
    /// Run as a daemon in the background
    Daemon {
        #[clap(long)]
        verbose: bool,
    },
    /// Set CPU governor
    SetGovernor {
        governor: String,
        #[clap(long)]
        core_id: Option<u32>,
    },
    /// Force a specific governor mode persistently
    ForceGovernor {
        /// Mode to force: performance, powersave, or reset
        #[clap(value_enum)]
        mode: GovernorOverrideMode,
    },
    /// Set turbo boost behavior
    SetTurbo {
        #[clap(value_enum)]
        setting: TurboSetting,
    },
    /// Display comprehensive debug information
    Debug,
    /// Set Energy Performance Preference (EPP)
    SetEpp {
        epp: String,
        #[clap(long)]
        core_id: Option<u32>,
    },
    /// Set Energy Performance Bias (EPB)
    SetEpb {
        epb: String, // Typically 0-15
        #[clap(long)]
        core_id: Option<u32>,
    },
    /// Set minimum CPU frequency
    SetMinFreq {
        freq_mhz: u32,
        #[clap(long)]
        core_id: Option<u32>,
    },
    /// Set maximum CPU frequency
    SetMaxFreq {
        freq_mhz: u32,
        #[clap(long)]
        core_id: Option<u32>,
    },
    /// Set ACPI platform profile
    SetPlatformProfile { profile: String },
    /// Set battery charge thresholds to extend battery lifespan
    SetBatteryThresholds {
        /// Percentage at which charging starts (when below this value)
        #[clap(value_parser = value_parser!(u8).range(0..=99))]
        start_threshold: u8,
        /// Percentage at which charging stops (when it reaches this value)
        #[clap(value_parser = value_parser!(u8).range(1..=100))]
        stop_threshold: u8,
    },
}

fn main() -> Result<(), AppError> {
    // Initialize logger once for the entire application
    init_logger();

    let cli = Cli::parse();

    // Load configuration first, as it might be needed by the monitor module
    // E.g., for ignored power supplies
    let config = match config::load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("Error loading configuration: {e}. Using default values.");
            // Proceed with default config if loading fails
            AppConfig::default()
        }
    };

    let command_result: Result<(), AppError> = match cli.command {
        // TODO: This will be moved to a different module in the future.
        Some(Commands::Info) => match monitor::collect_system_report(&config) {
            Ok(report) => {
                // Format section headers with proper centering
                let format_section = |title: &str| {
                    let title_len = title.len();
                    let total_width = title_len + 8; // 8 is for padding (4 on each side)
                    let separator = "═".repeat(total_width);

                    println!("\n╔{separator}╗");

                    // Calculate centering
                    println!("║    {title}    ║");

                    println!("╚{separator}╝");
                };

                format_section("System Information");
                println!("CPU Model:          {}", report.system_info.cpu_model);
                println!("Architecture:       {}", report.system_info.architecture);
                println!(
                    "Linux Distribution: {}",
                    report.system_info.linux_distribution
                );

                // Format timestamp in a readable way
                println!(
                    "Current Time:       {}",
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                );

                format_section("CPU Global Info");
                println!(
                    "Current Governor:   {}",
                    report
                        .cpu_global
                        .current_governor
                        .as_deref()
                        .unwrap_or("N/A")
                );
                println!(
                    "Available Governors: {}",
                    report.cpu_global.available_governors.join(", ")
                );
                println!(
                    "Turbo Status:       {}",
                    match report.cpu_global.turbo_status {
                        Some(true) => "Enabled",
                        Some(false) => "Disabled",
                        None => "Unknown",
                    }
                );

                println!(
                    "EPP:                {}",
                    report.cpu_global.epp.as_deref().unwrap_or("N/A")
                );
                println!(
                    "EPB:                {}",
                    report.cpu_global.epb.as_deref().unwrap_or("N/A")
                );
                println!(
                    "Platform Profile:   {}",
                    report
                        .cpu_global
                        .platform_profile
                        .as_deref()
                        .unwrap_or("N/A")
                );
                println!(
                    "CPU Temperature:    {}",
                    report.cpu_global.average_temperature_celsius.map_or_else(
                        || "N/A (No sensor detected)".to_string(),
                        |t| format!("{t:.1}°C")
                    )
                );

                format_section("CPU Core Info");

                // Get max core ID length for padding
                let max_core_id_len = report
                    .cpu_cores
                    .last()
                    .map_or(1, |core| core.core_id.to_string().len());

                // Table headers
                println!(
                    "  {:>width$}  │ {:^10} │ {:^10} │ {:^10} │ {:^7} │ {:^9}",
                    "Core",
                    "Current",
                    "Min",
                    "Max",
                    "Usage",
                    "Temp",
                    width = max_core_id_len + 4
                );
                println!(
                    "  {:─>width$}──┼─{:─^10}─┼─{:─^10}─┼─{:─^10}─┼─{:─^7}─┼─{:─^9}",
                    "",
                    "",
                    "",
                    "",
                    "",
                    "",
                    width = max_core_id_len + 4
                );

                for core_info in &report.cpu_cores {
                    // Format frequencies: if current > max, show in a special way
                    let current_freq = match core_info.current_frequency_mhz {
                        Some(freq) => {
                            let max_freq = core_info.max_frequency_mhz.unwrap_or(0);
                            if freq > max_freq && max_freq > 0 {
                                // Special format for boosted frequencies
                                format!("{freq}*")
                            } else {
                                format!("{freq}")
                            }
                        }
                        None => "N/A".to_string(),
                    };

                    // CPU core display
                    println!(
                        "  Core {:<width$} │ {:>10} │ {:>10} │ {:>10} │ {:>7} │ {:>9}",
                        core_info.core_id,
                        format!("{} MHz", current_freq),
                        format!(
                            "{} MHz",
                            core_info
                                .min_frequency_mhz
                                .map_or_else(|| "N/A".to_string(), |f| f.to_string())
                        ),
                        format!(
                            "{} MHz",
                            core_info
                                .max_frequency_mhz
                                .map_or_else(|| "N/A".to_string(), |f| f.to_string())
                        ),
                        format!(
                            "{}%",
                            core_info
                                .usage_percent
                                .map_or_else(|| "N/A".to_string(), |f| format!("{f:.1}"))
                        ),
                        format!(
                            "{}°C",
                            core_info
                                .temperature_celsius
                                .map_or_else(|| "N/A".to_string(), |f| format!("{f:.1}"))
                        ),
                        width = max_core_id_len
                    );
                }

                // Only display battery info for systems that have real batteries
                // Skip this section entirely on desktop systems
                if !report.batteries.is_empty() {
                    let has_real_batteries = report.batteries.iter().any(|b| {
                        // Check if any battery has actual battery data
                        // (as opposed to peripherals like wireless mice)
                        b.capacity_percent.is_some() || b.power_rate_watts.is_some()
                    });

                    if has_real_batteries {
                        format_section("Battery Info");
                        for battery_info in &report.batteries {
                            // Check if this appears to be a real system battery
                            if battery_info.capacity_percent.is_some()
                                || battery_info.power_rate_watts.is_some()
                            {
                                let power_status = if battery_info.ac_connected {
                                    "Connected to AC"
                                } else {
                                    "Running on Battery"
                                };

                                println!("Battery {}:", battery_info.name);
                                println!("  Power Status:     {power_status}");
                                println!(
                                    "  State:            {}",
                                    battery_info.charging_state.as_deref().unwrap_or("Unknown")
                                );

                                if let Some(capacity) = battery_info.capacity_percent {
                                    println!("  Capacity:         {capacity}%");
                                }

                                if let Some(power) = battery_info.power_rate_watts {
                                    let direction = if power >= 0.0 {
                                        "charging"
                                    } else {
                                        "discharging"
                                    };
                                    println!(
                                        "  Power Rate:       {:.2} W ({})",
                                        power.abs(),
                                        direction
                                    );
                                }

                                // Display charge thresholds if available
                                if battery_info.charge_start_threshold.is_some()
                                    || battery_info.charge_stop_threshold.is_some()
                                {
                                    println!(
                                        "  Charge Thresholds: {}-{}",
                                        battery_info
                                            .charge_start_threshold
                                            .map_or_else(|| "N/A".to_string(), |t| t.to_string()),
                                        battery_info
                                            .charge_stop_threshold
                                            .map_or_else(|| "N/A".to_string(), |t| t.to_string())
                                    );
                                }
                            }
                        }
                    }
                }

                format_section("System Load");
                println!(
                    "Load Average (1m):  {:.2}",
                    report.system_load.load_avg_1min
                );
                println!(
                    "Load Average (5m):  {:.2}",
                    report.system_load.load_avg_5min
                );
                println!(
                    "Load Average (15m): {:.2}",
                    report.system_load.load_avg_15min
                );
                Ok(())
            }
            Err(e) => Err(AppError::Monitor(e)),
        },
        Some(Commands::SetGovernor { governor, core_id }) => {
            cpu::set_governor(&governor, core_id).map_err(AppError::Control)
        }
        Some(Commands::ForceGovernor { mode }) => {
            cpu::force_governor(mode).map_err(AppError::Control)
        }
        Some(Commands::SetTurbo { setting }) => cpu::set_turbo(setting).map_err(AppError::Control),
        Some(Commands::SetEpp { epp, core_id }) => {
            cpu::set_epp(&epp, core_id).map_err(AppError::Control)
        }
        Some(Commands::SetEpb { epb, core_id }) => {
            cpu::set_epb(&epb, core_id).map_err(AppError::Control)
        }
        Some(Commands::SetMinFreq { freq_mhz, core_id }) => {
            // Basic validation for reasonable CPU frequency values
            validate_freq(freq_mhz, "Minimum")?;
            cpu::set_min_frequency(freq_mhz, core_id).map_err(AppError::Control)
        }
        Some(Commands::SetMaxFreq { freq_mhz, core_id }) => {
            // Basic validation for reasonable CPU frequency values
            validate_freq(freq_mhz, "Maximum")?;
            cpu::set_max_frequency(freq_mhz, core_id).map_err(AppError::Control)
        }
        Some(Commands::SetPlatformProfile { profile }) => {
            // Get available platform profiles and validate early if possible
            match cpu::get_platform_profiles() {
                Ok(available_profiles) => {
                    if available_profiles.contains(&profile) {
                        info!("Setting platform profile to '{profile}'");
                        cpu::set_platform_profile(&profile).map_err(AppError::Control)
                    } else {
                        error!(
                            "Invalid platform profile: '{}'. Available profiles: {}",
                            profile,
                            available_profiles.join(", ")
                        );
                        Err(ControlError::InvalidProfile(format!(
                            "Invalid platform profile: '{}'. Available profiles: {}",
                            profile,
                            available_profiles.join(", ")
                        ))
                        .into())
                    }
                }
                Err(_e) => {
                    // If we can't get profiles (e.g., feature not supported), pass through to the function
                    cpu::set_platform_profile(&profile).map_err(AppError::Control)
                }
            }
        }
        Some(Commands::SetBatteryThresholds {
            start_threshold,
            stop_threshold,
        }) => {
            // We only need to check if start < stop since the range validation is handled by Clap
            if start_threshold >= stop_threshold {
                error!(
                    "Start threshold ({start_threshold}) must be less than stop threshold ({stop_threshold})"
                );
                Err(ControlError::InvalidValueError(format!(
                    "Start threshold ({start_threshold}) must be less than stop threshold ({stop_threshold})"
                )).into())
            } else {
                info!(
                    "Setting battery thresholds: start at {start_threshold}%, stop at {stop_threshold}%"
                );
                battery::set_battery_charge_thresholds(start_threshold, stop_threshold)
                    .map_err(AppError::Control)
            }
        }
        Some(Commands::Daemon { verbose }) => daemon::run_daemon(config, verbose),
        Some(Commands::Debug) => cli::debug::run_debug(&config),
        None => {
            info!("Welcome to superfreq! Use --help for commands.");
            debug!("Current effective configuration: {config:?}");
            Ok(())
        }
    };

    if let Err(e) = command_result {
        error!("Error executing command: {e}");
        if let Some(source) = e.source() {
            error!("Caused by: {source}");
        }

        // Check for permission denied errors
        if let AppError::Control(control_error) = &e {
            if matches!(control_error, ControlError::PermissionDenied(_)) {
                error!(
                    "Hint: This operation may require administrator privileges (e.g., run with sudo)."
                );
            }
        }

        std::process::exit(1);
    }

    Ok(())
}

/// Initialize the logger for the entire application
static LOGGER_INIT: Once = Once::new();
fn init_logger() {
    LOGGER_INIT.call_once(|| {
        // Set default log level based on environment or default to Info
        let env_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());

        Builder::new()
            .parse_filters(&env_log)
            .format_timestamp(None)
            .format_module_path(false)
            .init();

        debug!("Logger initialized with RUST_LOG={env_log}");
    });
}

/// Validate CPU frequency input values
fn validate_freq(freq_mhz: u32, label: &str) -> Result<(), AppError> {
    if freq_mhz == 0 {
        error!("{label} frequency cannot be zero");
        Err(ControlError::InvalidValueError(format!("{label} frequency cannot be zero")).into())
    } else if freq_mhz > 10000 {
        // Extremely high value unlikely to be valid
        error!("{label} frequency ({freq_mhz} MHz) is unreasonably high");
        Err(ControlError::InvalidValueError(format!(
            "{label} frequency ({freq_mhz} MHz) is unreasonably high"
        ))
        .into())
    } else {
        Ok(())
    }
}
