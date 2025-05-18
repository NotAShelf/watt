mod cli;
mod config;
mod core;
mod cpu;
mod daemon;
mod engine;
mod monitor;
mod power_supply;
mod util;

use anyhow::Context;
use clap::Parser as _;
use std::fmt::Write as _;
use std::io::Write as _;
use std::{io, process};
use yansi::Paint as _;

#[derive(clap::Parser, Debug)]
#[clap(author, version, about)]
struct Cli {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity,

    #[clap(subcommand)]
    command: Command,
}

#[derive(clap::Parser, Debug)]
enum Command {
    /// Display information.
    Info,

    /// Start the daemon.
    Start,

    /// Modify attributes.
    Set {
        /// The CPUs to apply the changes to. When unspecified, will be applied to all CPUs.
        #[arg(short = 'c', long = "for")]
        for_: Option<Vec<u32>>,

        /// Set the CPU governor.
        #[arg(long)]
        governor: Option<String>, // TODO: Validate with clap for available governors.

        /// Set CPU Energy Performance Preference (EPP). Short form: --epp.
        #[arg(long, alias = "epp")]
        energy_performance_preference: Option<String>,

        /// Set CPU Energy Performance Bias (EPB). Short form: --epb.
        #[arg(long, alias = "epb")]
        energy_performance_bias: Option<String>,

        /// Set minimum CPU frequency in MHz. Short form: --freq-min.
        #[arg(short = 'f', long, alias = "freq-min", value_parser = clap::value_parser!(u64).range(1..=10_000))]
        frequency_mhz_minimum: Option<u64>,

        /// Set maximum CPU frequency in MHz. Short form: --freq-max.
        #[arg(short = 'F', long, alias = "freq-max", value_parser = clap::value_parser!(u64).range(1..=10_000))]
        frequency_mhz_maximum: Option<u64>,

        /// Set turbo boost behaviour. Has to be for all CPUs.
        #[arg(long, conflicts_with = "for_")]
        turbo: Option<cpu::Turbo>,

        /// Set ACPI platform profile. Has to be for all CPUs.
        #[arg(long, alias = "profile", conflicts_with = "for_")]
        platform_profile: Option<String>,

        /// Set the percentage that the power supply has to drop under for charging to start. Short form: --charge-start.
        #[arg(short = 'p', long, alias = "charge-start", value_parser = clap::value_parser!(u8).range(0..=100), conflicts_with = "for_")]
        charge_threshold_start: Option<u8>,

        /// Set the percentage where charging will stop. Short form: --charge-end.
        #[arg(short = 'P', long, alias = "charge-end", value_parser = clap::value_parser!(u8).range(0..=100), conflicts_with = "for_")]
        charge_threshold_end: Option<u8>,
    },
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbosity.log_level_filter())
        .format_timestamp(None)
        .format_module_path(false)
        .init();

    let config = config::load_config().context("failed to load config")?;

    match cli.command {
        Command::Info => todo!(),

        Command::Start => {
            daemon::run_daemon(config)?;
            Ok(())
        }

        Command::Set {
            for_,
            governor,
            energy_performance_preference,
            energy_performance_bias,
            frequency_mhz_minimum,
            frequency_mhz_maximum,
            turbo,
            platform_profile,
            charge_threshold_start,
            charge_threshold_end,
        } => {
            let cpus = match for_ {
                Some(cpus) => cpus,
                None => cpu::get_real_cpus()?,
            };

            for cpu in cpus {
                if let Some(governor) = governor.as_ref() {
                    cpu::set_governor(governor, cpu)?;
                }

                if let Some(epp) = energy_performance_preference.as_ref() {
                    cpu::set_epp(epp, cpu)?;
                }

                if let Some(epb) = energy_performance_bias.as_ref() {
                    cpu::set_epb(epb, cpu)?;
                }

                if let Some(mhz_minimum) = frequency_mhz_minimum {
                    cpu::set_frequency_minimum(mhz_minimum, cpu)?;
                }

                if let Some(mhz_maximum) = frequency_mhz_maximum {
                    cpu::set_frequency_maximum(mhz_maximum, cpu)?;
                }
            }

            if let Some(turbo) = turbo {
                cpu::set_turbo(turbo)?;
            }

            if let Some(platform_profile) = platform_profile.as_ref() {
                cpu::set_platform_profile(platform_profile)?;
            }

            for power_supply in power_supply::get_power_supplies()? {
                if let Some(threshold_start) = charge_threshold_start {
                    power_supply::set_charge_threshold_start(&power_supply, threshold_start)?;
                }

                if let Some(threshold_end) = charge_threshold_end {
                    power_supply::set_charge_threshold_end(&power_supply, threshold_end)?;
                }
            }

            Ok(())
        }
    }

    // TODO: This will be moved to a different module in the future.
    // Some(Command::Info) => match monitor::collect_system_report(&config) {
    //     Ok(report) => {
    //         // Format section headers with proper centering
    //         let format_section = |title: &str| {
    //             let title_len = title.len();
    //             let total_width = title_len + 8; // 8 is for padding (4 on each side)
    //             let separator = "═".repeat(total_width);

    //             println!("\n╔{separator}╗");

    //             // Calculate centering
    //             println!("║  {title}  ║");

    //             println!("╚{separator}╝");
    //         };

    //         format_section("System Information");
    //         println!("CPU Model:      {}", report.system_info.cpu_model);
    //         println!("Architecture:     {}", report.system_info.architecture);
    //         println!(
    //             "Linux Distribution: {}",
    //             report.system_info.linux_distribution
    //         );

    //         // Format timestamp in a readable way
    //         println!("Current Time:     {}", jiff::Timestamp::now());

    //         format_section("CPU Global Info");
    //         println!(
    //             "Current Governor:  {}",
    //             report
    //                 .cpu_global
    //                 .current_governor
    //                 .as_deref()
    //                 .unwrap_or("N/A")
    //         );
    //         println!(
    //             "Available Governors: {}", // 21 length baseline
    //             report.cpu_global.available_governors.join(", ")
    //         );
    //         println!(
    //             "Turbo Status:    {}",
    //             match report.cpu_global.turbo_status {
    //                 Some(true) => "Enabled",
    //                 Some(false) => "Disabled",
    //                 None => "Unknown",
    //             }
    //         );

    //         println!(
    //             "EPP:         {}",
    //             report.cpu_global.epp.as_deref().unwrap_or("N/A")
    //         );
    //         println!(
    //             "EPB:         {}",
    //             report.cpu_global.epb.as_deref().unwrap_or("N/A")
    //         );
    //         println!(
    //             "Platform Profile:  {}",
    //             report
    //                 .cpu_global
    //                 .platform_profile
    //                 .as_deref()
    //                 .unwrap_or("N/A")
    //         );
    //         println!(
    //             "CPU Temperature:   {}",
    //             report.cpu_global.average_temperature_celsius.map_or_else(
    //                 || "N/A (No sensor detected)".to_string(),
    //                 |t| format!("{t:.1}°C")
    //             )
    //         );

    //         format_section("CPU Core Info");

    //         // Get max core ID length for padding
    //         let max_core_id_len = report
    //             .cpu_cores
    //             .last()
    //             .map_or(1, |core| core.core_id.to_string().len());

    //         // Table headers
    //         println!(
    //             "  {:>width$}  │ {:^10} │ {:^10} │ {:^10} │ {:^7} │ {:^9}",
    //             "Core",
    //             "Current",
    //             "Min",
    //             "Max",
    //             "Usage",
    //             "Temp",
    //             width = max_core_id_len + 4
    //         );
    //         println!(
    //             "  {:─>width$}──┼─{:─^10}─┼─{:─^10}─┼─{:─^10}─┼─{:─^7}─┼─{:─^9}",
    //             "",
    //             "",
    //             "",
    //             "",
    //             "",
    //             "",
    //             width = max_core_id_len + 4
    //         );

    //         for core_info in &report.cpu_cores {
    //             // Format frequencies: if current > max, show in a special way
    //             let current_freq = match core_info.current_frequency_mhz {
    //                 Some(freq) => {
    //                     let max_freq = core_info.max_frequency_mhz.unwrap_or(0);
    //                     if freq > max_freq && max_freq > 0 {
    //                         // Special format for boosted frequencies
    //                         format!("{freq}*")
    //                     } else {
    //                         format!("{freq}")
    //                     }
    //                 }
    //                 None => "N/A".to_string(),
    //             };

    //             // CPU core display
    //             println!(
    //                 "  Core {:<width$} │ {:>10} │ {:>10} │ {:>10} │ {:>7} │ {:>9}",
    //                 core_info.core_id,
    //                 format!("{} MHz", current_freq),
    //                 format!(
    //                     "{} MHz",
    //                     core_info
    //                         .min_frequency_mhz
    //                         .map_or_else(|| "N/A".to_string(), |f| f.to_string())
    //                 ),
    //                 format!(
    //                     "{} MHz",
    //                     core_info
    //                         .max_frequency_mhz
    //                         .map_or_else(|| "N/A".to_string(), |f| f.to_string())
    //                 ),
    //                 format!(
    //                     "{}%",
    //                     core_info
    //                         .usage_percent
    //                         .map_or_else(|| "N/A".to_string(), |f| format!("{f:.1}"))
    //                 ),
    //                 format!(
    //                     "{}°C",
    //                     core_info
    //                         .temperature_celsius
    //                         .map_or_else(|| "N/A".to_string(), |f| format!("{f:.1}"))
    //                 ),
    //                 width = max_core_id_len
    //             );
    //         }

    //         // Only display battery info for systems that have real batteries
    //         // Skip this section entirely on desktop systems
    //         if !report.batteries.is_empty() {
    //             let has_real_batteries = report.batteries.iter().any(|b| {
    //                 // Check if any battery has actual battery data
    //                 // (as opposed to peripherals like wireless mice)
    //                 b.capacity_percent.is_some() || b.power_rate_watts.is_some()
    //             });

    //             if has_real_batteries {
    //                 format_section("Battery Info");
    //                 for battery_info in &report.batteries {
    //                     // Check if this appears to be a real system battery
    //                     if battery_info.capacity_percent.is_some()
    //                         || battery_info.power_rate_watts.is_some()
    //                     {
    //                         let power_status = if battery_info.ac_connected {
    //                             "Connected to AC"
    //                         } else {
    //                             "Running on Battery"
    //                         };

    //                         println!("Battery {}:", battery_info.name);
    //                         println!("  Power Status:   {power_status}");
    //                         println!(
    //                             "  State:      {}",
    //                             battery_info.charging_state.as_deref().unwrap_or("Unknown")
    //                         );

    //                         if let Some(capacity) = battery_info.capacity_percent {
    //                             println!("  Capacity:     {capacity}%");
    //                         }

    //                         if let Some(power) = battery_info.power_rate_watts {
    //                             let direction = if power >= 0.0 {
    //                                 "charging"
    //                             } else {
    //                                 "discharging"
    //                             };
    //                             println!(
    //                                 "  Power Rate:     {:.2} W ({})",
    //                                 power.abs(),
    //                                 direction
    //                             );
    //                         }

    //                         // Display charge thresholds if available
    //                         if battery_info.charge_start_threshold.is_some()
    //                             || battery_info.charge_stop_threshold.is_some()
    //                         {
    //                             println!(
    //                                 "  Charge Thresholds: {}-{}",
    //                                 battery_info
    //                                     .charge_start_threshold
    //                                     .map_or_else(|| "N/A".to_string(), |t| t.to_string()),
    //                                 battery_info
    //                                     .charge_stop_threshold
    //                                     .map_or_else(|| "N/A".to_string(), |t| t.to_string())
    //                             );
    //                         }
    //                     }
    //                 }
    //             }
    //         }

    //         format_section("System Load");
    //         println!(
    //             "Load Average (1m):  {:.2}",
    //             report.system_load.load_avg_1min
    //         );
    //         println!(
    //             "Load Average (5m):  {:.2}",
    //             report.system_load.load_avg_5min
    //         );
    //         println!(
    //             "Load Average (15m): {:.2}",
    //             report.system_load.load_avg_15min
    //         );
    //         Ok(())
    //     }
    //     Err(e) => Err(AppError::Monitor(e)),
    // },
    // Some(CliCommand::SetPlatformProfile { profile }) => {
    //   // Get available platform profiles and validate early if possible
    //   match cpu::get_platform_profiles() {
    //     Ok(available_profiles) => {
    //       if available_profiles.contains(&profile) {
    //         log::info!("Setting platform profile to '{profile}'");
    //         cpu::set_platform_profile(&profile).map_err(AppError::Control)
    //       } else {
    //         log::error!(
    //           "Invalid platform profile: '{}'. Available profiles: {}",
    //           profile,
    //           available_profiles.join(", ")
    //         );
    //         Err(AppError::Generic(format!(
    //           "Invalid platform profile: '{}'. Available profiles: {}",
    //           profile,
    //           available_profiles.join(", ")
    //         )))
    //       }
    //     }
    //     Err(_e) => {
    //       // If we can't get profiles (e.g., feature not supported), pass through to the function
    //       cpu::set_platform_profile(&profile).map_err(AppError::Control)
    //     }
    //   }
    // }
}

fn main() {
    let Err(error) = real_main() else {
        return;
    };

    let mut err = io::stderr();

    let mut message = String::new();
    let mut chain = error.chain().rev().peekable();

    while let Some(error) = chain.next() {
        let _ = write!(
            err,
            "{header} ",
            header = if chain.peek().is_none() {
                "error:"
            } else {
                "cause:"
            }
            .red()
            .bold(),
        );

        String::clear(&mut message);
        let _ = write!(message, "{error}");

        let mut chars = message.char_indices();

        let _ = match (chars.next(), chars.next()) {
            (Some((_, first)), Some((second_start, second))) if second.is_lowercase() => {
                writeln!(
                    err,
                    "{first_lowercase}{rest}",
                    first_lowercase = first.to_lowercase(),
                    rest = &message[second_start..],
                )
            }

            _ => {
                writeln!(err, "{message}")
            }
        };
    }

    process::exit(1);
}
