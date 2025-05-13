mod config;
mod core;
mod cpu;
mod daemon;
mod engine;
mod error;
mod monitor;

use crate::config::AppConfig;
use crate::core::TurboSetting;
use crate::error::ControlError;
use clap::Parser;

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
    /// Set turbo boost behavior
    SetTurbo {
        #[clap(value_enum)]
        setting: TurboSetting,
    },
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
}

fn main() {
    let cli = Cli::parse();

    // Load configuration first, as it might be needed by the monitor module
    // E.g., for ignored power supplies
    let config = match config::load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error loading configuration: {}. Using default values.", e);
            // Proceed with default config if loading fails, as per previous steps
            AppConfig::default()
        }
    };

    let command_result = match cli.command {
        Some(Commands::Info) => match monitor::collect_system_report(&config) {
            Ok(report) => {
                println!("--- System Information ---");
                println!("CPU Model: {}", report.system_info.cpu_model);
                println!("Architecture: {}", report.system_info.architecture);
                println!(
                    "Linux Distribution: {}",
                    report.system_info.linux_distribution
                );
                println!("Timestamp: {:?}", report.timestamp);

                println!("\n--- CPU Global Info ---");
                println!("Current Governor: {:?}", report.cpu_global.current_governor);
                println!(
                    "Available Governors: {:?}",
                    report.cpu_global.available_governors.join(", ")
                );
                println!("Turbo Status: {:?}", report.cpu_global.turbo_status);
                println!("EPP: {:?}", report.cpu_global.epp);
                println!("EPB: {:?}", report.cpu_global.epb);
                println!("Platform Profile: {:?}", report.cpu_global.platform_profile);
                println!(
                    "Average CPU Temperature: {}",
                    report.cpu_global.average_temperature_celsius.map_or_else(
                        || "N/A (CPU temperature sensor not detected)".to_string(),
                        |t| format!("{:.1}°C", t)
                    )
                );

                println!("\n--- CPU Core Info ---");
                for core_info in report.cpu_cores {
                    println!(
                        "  Core {}: Current Freq: {:?} MHz, Min Freq: {:?} MHz, Max Freq: {:?} MHz, Usage: {:?}%, Temp: {:?}°C",
                        core_info.core_id,
                        core_info
                            .current_frequency_mhz
                            .map_or_else(|| "N/A".to_string(), |f| f.to_string()),
                        core_info
                            .min_frequency_mhz
                            .map_or_else(|| "N/A".to_string(), |f| f.to_string()),
                        core_info
                            .max_frequency_mhz
                            .map_or_else(|| "N/A".to_string(), |f| f.to_string()),
                        core_info
                            .usage_percent
                            .map_or_else(|| "N/A".to_string(), |f| format!("{:.1}", f)),
                        core_info
                            .temperature_celsius
                            .map_or_else(|| "N/A".to_string(), |f| format!("{:.1}", f))
                    );
                }

                println!("\n--- Battery Info ---");
                if report.batteries.is_empty() {
                    println!("  No batteries found or all are ignored.");
                } else {
                    for battery_info in report.batteries {
                        println!(
                            "  Battery {}: AC Connected: {}, State: {:?}, Capacity: {:?}%, Power Rate: {:?} W, Charge Thresholds: {:?}-{:?}",
                            battery_info.name,
                            battery_info.ac_connected,
                            battery_info.charging_state.as_deref().unwrap_or("N/A"),
                            battery_info
                                .capacity_percent
                                .map_or_else(|| "N/A".to_string(), |c| c.to_string()),
                            battery_info
                                .power_rate_watts
                                .map_or_else(|| "N/A".to_string(), |p| format!("{:.2}", p)),
                            battery_info
                                .charge_start_threshold
                                .map_or_else(|| "N/A".to_string(), |t| t.to_string()),
                            battery_info
                                .charge_stop_threshold
                                .map_or_else(|| "N/A".to_string(), |t| t.to_string())
                        );
                    }
                }

                println!("\n--- System Load ---");
                println!(
                    "Load Average (1m, 5m, 15m): {:.2}, {:.2}, {:.2}",
                    report.system_load.load_avg_1min,
                    report.system_load.load_avg_5min,
                    report.system_load.load_avg_15min
                );
                Ok(())
            }
            Err(e) => Err(Box::new(e) as Box<dyn std::error::Error>),
        },
        Some(Commands::SetGovernor { governor, core_id }) => cpu::set_governor(&governor, core_id)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
        Some(Commands::SetTurbo { setting }) => {
            cpu::set_turbo(setting).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Some(Commands::SetEpp { epp, core_id }) => {
            cpu::set_epp(&epp, core_id).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Some(Commands::SetEpb { epb, core_id }) => {
            cpu::set_epb(&epb, core_id).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Some(Commands::SetMinFreq { freq_mhz, core_id }) => {
            cpu::set_min_frequency(freq_mhz, core_id)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Some(Commands::SetMaxFreq { freq_mhz, core_id }) => {
            cpu::set_max_frequency(freq_mhz, core_id)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Some(Commands::SetPlatformProfile { profile }) => cpu::set_platform_profile(&profile)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>),
        Some(Commands::Daemon { verbose }) => daemon::run_daemon(config, verbose),
        None => {
            println!("Welcome to superfreq! Use --help for commands.");
            println!("Current effective configuration: {:?}", config);
            Ok(())
        }
    };

    if let Err(e) = command_result {
        eprintln!("Error executing command: {}", e);
        if let Some(source) = e.source() {
            eprintln!("Caused by: {}", source);
        }
        // TODO: Consider specific error handling for PermissionDenied from cpu here
        // For example, check if e.downcast_ref::<cpu::ControlError>() matches PermissionDenied
        // and print a more specific message like "Try running with sudo."
        // We'll revisit this in the future once CPU logic is more stable.
        if let Some(control_error) = e.downcast_ref::<ControlError>() {
            if matches!(control_error, ControlError::PermissionDenied(_)) {
                eprintln!(
                    "Hint: This operation may require administrator privileges (e.g., run with sudo)."
                );
            }
        }

        std::process::exit(1);
    }
}
