use crate::config::AppConfig;
use crate::cpu;
use crate::monitor;
use crate::util::error::AppError;
use std::fs;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Prints comprehensive debug information about the system
pub fn run_debug(config: &AppConfig) -> Result<(), AppError> {
    println!("=== SUPERFREQ DEBUG INFORMATION ===");
    println!("Version: {}", env!("CARGO_PKG_VERSION"));

    // Current date and time
    let formatted_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    println!("Timestamp: {formatted_time}");

    // Kernel information
    if let Ok(kernel_info) = get_kernel_info() {
        println!("Kernel Version: {kernel_info}");
    } else {
        println!("Kernel Version: Unable to determine");
    }

    // System uptime
    if let Ok(uptime) = get_system_uptime() {
        println!(
            "System Uptime: {} hours, {} minutes",
            uptime.as_secs() / 3600,
            (uptime.as_secs() % 3600) / 60
        );
    } else {
        println!("System Uptime: Unable to determine");
    }

    // Get system information
    match monitor::collect_system_report(config) {
        Ok(report) => {
            println!("\n--- SYSTEM INFORMATION ---");
            println!("CPU Model: {}", report.system_info.cpu_model);
            println!("Architecture: {}", report.system_info.architecture);
            println!(
                "Linux Distribution: {}",
                report.system_info.linux_distribution
            );

            println!("\n--- CONFIGURATION ---");
            println!("Current Configuration: {config:#?}");

            // Print important sysfs paths and whether they exist
            println!("\n--- SYSFS PATHS ---");
            check_and_print_sysfs_path(
                "/sys/devices/system/cpu/intel_pstate/no_turbo",
                "Intel P-State Turbo Control",
            );
            check_and_print_sysfs_path(
                "/sys/devices/system/cpu/cpufreq/boost",
                "Generic CPU Boost Control",
            );
            check_and_print_sysfs_path(
                "/sys/devices/system/cpu/amd_pstate/cpufreq/boost",
                "AMD P-State Boost Control",
            );
            check_and_print_sysfs_path(
                "/sys/firmware/acpi/platform_profile",
                "ACPI Platform Profile Control",
            );
            check_and_print_sysfs_path("/sys/class/power_supply", "Power Supply Information");

            println!("\n--- CPU INFORMATION ---");
            println!("Current Governor: {:?}", report.cpu_global.current_governor);
            println!(
                "Available Governors: {}",
                report.cpu_global.available_governors.join(", ")
            );
            println!("Turbo Status: {:?}", report.cpu_global.turbo_status);
            println!(
                "Energy Performance Preference (EPP): {:?}",
                report.cpu_global.epp
            );
            println!("Energy Performance Bias (EPB): {:?}", report.cpu_global.epb);

            // Add governor override information
            if let Some(override_governor) = cpu::get_governor_override() {
                println!("Governor Override: {}", override_governor.trim());
            } else {
                println!("Governor Override: None");
            }

            println!("\n--- PLATFORM PROFILE ---");
            println!(
                "Current Platform Profile: {:?}",
                report.cpu_global.platform_profile
            );
            match cpu::get_platform_profiles() {
                Ok(profiles) => println!("Available Platform Profiles: {}", profiles.join(", ")),
                Err(_) => println!("Available Platform Profiles: Not supported on this system"),
            }

            println!("\n--- CPU CORES DETAIL ---");
            println!("Total CPU Cores: {}", report.cpu_cores.len());
            for core in &report.cpu_cores {
                println!("Core {}:", core.core_id);
                println!(
                    "  Current Frequency: {} MHz",
                    core.current_frequency_mhz
                        .map_or_else(|| "N/A".to_string(), |f| f.to_string())
                );
                println!(
                    "  Min Frequency: {} MHz",
                    core.min_frequency_mhz
                        .map_or_else(|| "N/A".to_string(), |f| f.to_string())
                );
                println!(
                    "  Max Frequency: {} MHz",
                    core.max_frequency_mhz
                        .map_or_else(|| "N/A".to_string(), |f| f.to_string())
                );
                println!(
                    "  Usage: {}%",
                    core.usage_percent
                        .map_or_else(|| "N/A".to_string(), |u| format!("{u:.1}"))
                );
                println!(
                    "  Temperature: {}°C",
                    core.temperature_celsius
                        .map_or_else(|| "N/A".to_string(), |t| format!("{t:.1}"))
                );
            }

            println!("\n--- TEMPERATURE INFORMATION ---");
            println!(
                "Average CPU Temperature: {}",
                report.cpu_global.average_temperature_celsius.map_or_else(
                    || "N/A (CPU temperature sensor not detected)".to_string(),
                    |t| format!("{t:.1}°C")
                )
            );

            println!("\n--- BATTERY INFORMATION ---");
            if report.batteries.is_empty() {
                println!("No batteries found or all are ignored.");
            } else {
                for battery in &report.batteries {
                    println!("Battery: {}", battery.name);
                    println!("  AC Connected: {}", battery.ac_connected);
                    println!(
                        "  Charging State: {}",
                        battery.charging_state.as_deref().unwrap_or("N/A")
                    );
                    println!(
                        "  Capacity: {}%",
                        battery
                            .capacity_percent
                            .map_or_else(|| "N/A".to_string(), |c| c.to_string())
                    );
                    println!(
                        "  Power Rate: {} W",
                        battery
                            .power_rate_watts
                            .map_or_else(|| "N/A".to_string(), |p| format!("{p:.2}"))
                    );
                    println!(
                        "  Charge Start Threshold: {}",
                        battery
                            .charge_start_threshold
                            .map_or_else(|| "N/A".to_string(), |t| t.to_string())
                    );
                    println!(
                        "  Charge Stop Threshold: {}",
                        battery
                            .charge_stop_threshold
                            .map_or_else(|| "N/A".to_string(), |t| t.to_string())
                    );
                }
            }

            println!("\n--- SYSTEM LOAD ---");
            println!(
                "Load Average (1 min): {:.2}",
                report.system_load.load_avg_1min
            );
            println!(
                "Load Average (5 min): {:.2}",
                report.system_load.load_avg_5min
            );
            println!(
                "Load Average (15 min): {:.2}",
                report.system_load.load_avg_15min
            );

            println!("\n--- DAEMON STATUS ---");
            // Simple check for daemon status - can be expanded later
            let daemon_status = fs::metadata("/var/run/superfreq.pid").is_ok();
            println!("Daemon Running: {daemon_status}");

            // Check for systemd service status
            if let Ok(systemd_status) = is_systemd_service_active("superfreq") {
                println!("Systemd Service Active: {systemd_status}");
            }

            Ok(())
        }
        Err(e) => Err(AppError::Monitor(e)),
    }
}

/// Get kernel version information
fn get_kernel_info() -> Result<String, AppError> {
    let output = Command::new("uname")
        .arg("-r")
        .output()
        .map_err(AppError::Io)?;

    let kernel_version = String::from_utf8(output.stdout)
        .map_err(|e| AppError::Generic(format!("Failed to parse kernel version: {e}")))?;
    Ok(kernel_version.trim().to_string())
}

/// Get system uptime
fn get_system_uptime() -> Result<Duration, AppError> {
    let uptime_str = fs::read_to_string("/proc/uptime").map_err(|e| AppError::Io(e))?;
    let uptime_secs = uptime_str
        .split_whitespace()
        .next()
        .ok_or_else(|| AppError::Generic("Invalid format in /proc/uptime file".to_string()))?
        .parse::<f64>()
        .map_err(|e| {
            AppError::Generic(format!("Failed to parse uptime from /proc/uptime: {}", e))
        })?;

    Ok(Duration::from_secs_f64(uptime_secs))
}

/// Check if a sysfs path exists and print its status
fn check_and_print_sysfs_path(path: &str, description: &str) {
    let exists = std::path::Path::new(path).exists();
    println!(
        "{}: {} ({})",
        description,
        path,
        if exists { "Exists" } else { "Not Found" }
    );
}

/// Check if a systemd service is active
fn is_systemd_service_active(service_name: &str) -> Result<bool, AppError> {
    let output = Command::new("systemctl")
        .arg("is-active")
        .arg(format!("{service_name}.service"))
        .stdout(Stdio::piped()) // capture stdout instead of letting it print
        .stderr(Stdio::null()) // redirect stderr to null
        .output()
        .map_err(AppError::Io)?;

    let status = String::from_utf8(output.stdout)
        .map_err(|e| AppError::Generic(format!("Failed to parse systemctl output: {e}")))?;
    Ok(status.trim() == "active")
}
