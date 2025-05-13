use crate::config::AppConfig;
use crate::core::{BatteryInfo, CpuCoreInfo, CpuGlobalInfo, SystemInfo, SystemLoad, SystemReport};
use crate::util::error::ControlError;
use crate::util::error::SysMonitorError;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    thread,
    time::Duration,
    time::SystemTime,
};

impl std::error::Error for SysMonitorError {}

pub type Result<T, E = SysMonitorError> = std::result::Result<T, E>;

// Read a sysfs file to a string, trimming whitespace
fn read_sysfs_file_trimmed(path: impl AsRef<Path>) -> Result<String> {
    fs::read_to_string(path.as_ref())
        .map(|s| s.trim().to_string())
        .map_err(|e| {
            SysMonitorError::ReadError(format!("Path: {:?}, Error: {}", path.as_ref().display(), e))
        })
}

// Read a sysfs file and parse it to a specific type
fn read_sysfs_value<T: FromStr>(path: impl AsRef<Path>) -> Result<T> {
    let content = read_sysfs_file_trimmed(path.as_ref())?;
    content.parse::<T>().map_err(|_| {
        SysMonitorError::ParseError(format!(
            "Could not parse '{}' from {:?}",
            content,
            path.as_ref().display()
        ))
    })
}

pub fn get_system_info() -> Result<SystemInfo> {
    let mut cpu_model = "Unknown".to_string();
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        for line in cpuinfo.lines() {
            if line.starts_with("model name") {
                if let Some(val) = line.split(':').nth(1) {
                    cpu_model = val.trim().to_string();
                    break;
                }
            }
        }
    }

    let architecture = std::env::consts::ARCH.to_string();

    let mut linux_distribution = "Unknown".to_string();
    if let Ok(os_release) = fs::read_to_string("/etc/os-release") {
        for line in os_release.lines() {
            if line.starts_with("PRETTY_NAME=") {
                if let Some(val) = line.split('=').nth(1) {
                    linux_distribution = val.trim_matches('"').to_string();
                    break;
                }
            }
        }
    } else if let Ok(lsb_release) = fs::read_to_string("/etc/lsb-release") {
        // fallback for some systems
        for line in lsb_release.lines() {
            if line.starts_with("DISTRIB_DESCRIPTION=") {
                if let Some(val) = line.split('=').nth(1) {
                    linux_distribution = val.trim_matches('"').to_string();
                    break;
                }
            }
        }
    }

    Ok(SystemInfo {
        cpu_model,
        architecture,
        linux_distribution,
    })
}

fn get_logical_core_count() -> Result<u32> {
    let mut count = 0;
    let path = Path::new("/sys/devices/system/cpu");
    if path.exists() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(name_str) = name.to_str() {
                if name_str.starts_with("cpu")
                    && name_str.len() > 3
                    && name_str[3..].chars().all(char::is_numeric)
                {
                    // Check if it's a directory representing a core that can have cpufreq
                    if entry.path().join("cpufreq").exists() {
                        count += 1;
                    } else if Path::new(&format!("/sys/devices/system/cpu/{}/online", name_str))
                        .exists()
                    {
                        // Fallback for cores that might not have cpufreq but are online (e.g. E-cores on some setups before driver loads)
                        // This is a simplification; true cpufreq capability is key.
                        // If cpufreq dir doesn't exist, it might not be controllable by this tool.
                        // For counting purposes, we count it if it's an online CPU.
                        count += 1;
                    }
                }
            }
        }
    }
    if count == 0 {
        // Fallback to num_cpus crate if sysfs parsing fails or yields 0
        Ok(num_cpus::get() as u32)
    } else {
        Ok(count)
    }
}

#[derive(Debug, Clone, Copy)]
struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
    guest: u64,
    guest_nice: u64,
}

impl CpuTimes {
    fn total_time(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    fn idle_time(&self) -> u64 {
        self.idle + self.iowait
    }
}

fn read_all_cpu_times() -> Result<HashMap<u32, CpuTimes>> {
    let content = fs::read_to_string("/proc/stat").map_err(SysMonitorError::Io)?;
    let mut cpu_times_map = HashMap::new();

    for line in content.lines() {
        if line.starts_with("cpu") && line.chars().nth(3).map_or(false, |c| c.is_digit(10)) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 11 {
                return Err(SysMonitorError::ProcStatParseError(format!(
                    "Line too short: {}",
                    line
                )));
            }

            let core_id_str = &parts[0][3..];
            let core_id = core_id_str.parse::<u32>().map_err(|_| {
                SysMonitorError::ProcStatParseError(format!(
                    "Failed to parse core_id: {}",
                    core_id_str
                ))
            })?;

            let times = CpuTimes {
                user: parts[1].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse user time: {}",
                        parts[1]
                    ))
                })?,
                nice: parts[2].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse nice time: {}",
                        parts[2]
                    ))
                })?,
                system: parts[3].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse system time: {}",
                        parts[3]
                    ))
                })?,
                idle: parts[4].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse idle time: {}",
                        parts[4]
                    ))
                })?,
                iowait: parts[5].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse iowait time: {}",
                        parts[5]
                    ))
                })?,
                irq: parts[6].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse irq time: {}",
                        parts[6]
                    ))
                })?,
                softirq: parts[7].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse softirq time: {}",
                        parts[7]
                    ))
                })?,
                steal: parts[8].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse steal time: {}",
                        parts[8]
                    ))
                })?,
                guest: parts[9].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse guest time: {}",
                        parts[9]
                    ))
                })?,
                guest_nice: parts[10].parse().map_err(|_| {
                    SysMonitorError::ProcStatParseError(format!(
                        "Failed to parse guest_nice time: {}",
                        parts[10]
                    ))
                })?,
            };
            cpu_times_map.insert(core_id, times);
        }
    }
    Ok(cpu_times_map)
}

pub fn get_cpu_core_info(
    core_id: u32,
    prev_times: &CpuTimes,
    current_times: &CpuTimes,
) -> Result<CpuCoreInfo> {
    let cpufreq_path = PathBuf::from(format!("/sys/devices/system/cpu/cpu{}/cpufreq/", core_id));

    let current_frequency_mhz = read_sysfs_value::<u32>(cpufreq_path.join("scaling_cur_freq"))
        .map(|khz| khz / 1000)
        .ok();
    let min_frequency_mhz = read_sysfs_value::<u32>(cpufreq_path.join("scaling_min_freq"))
        .map(|khz| khz / 1000)
        .ok();
    let max_frequency_mhz = read_sysfs_value::<u32>(cpufreq_path.join("scaling_max_freq"))
        .map(|khz| khz / 1000)
        .ok();

    // Temperature detection.
    // Should be generic enough to be able to support for multiple hardware sensors
    // with the possibility of extending later down the road.
    let mut temperature_celsius: Option<f32> = None;

    // Search for temperature in hwmon devices
    if let Ok(hwmon_dir) = fs::read_dir("/sys/class/hwmon") {
        for hw_entry in hwmon_dir.flatten() {
            let hw_path = hw_entry.path();

            // Check hwmon driver name
            if let Ok(name) = read_sysfs_file_trimmed(hw_path.join("name")) {
                // Intel CPU temperature driver
                if name == "coretemp" {
                    if let Some(temp) = get_temperature_for_core(&hw_path, core_id, "Core") {
                        temperature_celsius = Some(temp);
                        break;
                    }
                }
                // AMD CPU temperature driver
                // TODO: 'zenergy' can also report those stats, I think?
                else if name == "k10temp" || name == "zenpower" || name == "amdgpu" {
                    // AMD's k10temp doesn't always label cores individually
                    // First try to find core-specific temps
                    if let Some(temp) = get_temperature_for_core(&hw_path, core_id, "Tdie") {
                        temperature_celsius = Some(temp);
                        break;
                    }

                    // Try Tctl temperature (CPU control temp)
                    if let Some(temp) = get_generic_sensor_temperature(&hw_path, "Tctl") {
                        temperature_celsius = Some(temp);
                        break;
                    }

                    // Try CPU temperature
                    if let Some(temp) = get_generic_sensor_temperature(&hw_path, "CPU") {
                        temperature_celsius = Some(temp);
                        break;
                    }

                    // Fall back to any available temperature input without a specific label
                    temperature_celsius = get_fallback_temperature(&hw_path);
                    if temperature_celsius.is_some() {
                        break;
                    }
                }
                // Other CPU temperature drivers
                else if name.contains("cpu") || name.contains("temp") {
                    // Try to find a label that matches this core
                    if let Some(temp) = get_temperature_for_core(&hw_path, core_id, "Core") {
                        temperature_celsius = Some(temp);
                        break;
                    }

                    // Fall back to any temperature reading if specific core not found
                    temperature_celsius = get_fallback_temperature(&hw_path);
                    if temperature_celsius.is_some() {
                        break;
                    }
                }
            }
        }
    }

    // Try /sys/devices/platform paths for thermal zones as a last resort
    if temperature_celsius.is_none() {
        if let Ok(thermal_zones) = fs::read_dir("/sys/devices/virtual/thermal") {
            for entry in thermal_zones.flatten() {
                let zone_path = entry.path();
                let name = entry.file_name().into_string().unwrap_or_default();

                if name.starts_with("thermal_zone") {
                    // Try to match by type
                    if let Ok(zone_type) = read_sysfs_file_trimmed(zone_path.join("type")) {
                        if zone_type.contains("cpu")
                            || zone_type.contains("x86")
                            || zone_type.contains("core")
                        {
                            if let Ok(temp_mc) = read_sysfs_value::<i32>(zone_path.join("temp")) {
                                temperature_celsius = Some(temp_mc as f32 / 1000.0);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    let usage_percent: Option<f32> = {
        let prev_idle = prev_times.idle_time();
        let current_idle = current_times.idle_time();

        let prev_total = prev_times.total_time();
        let current_total = current_times.total_time();

        let total_diff = current_total.saturating_sub(prev_total);
        let idle_diff = current_idle.saturating_sub(prev_idle);

        // Avoid division by zero if no time has passed or counters haven't changed
        if total_diff == 0 {
            None
        } else {
            let usage = 100.0 * (1.0 - (idle_diff as f32 / total_diff as f32));
            Some(usage.max(0.0).min(100.0)) // clamp between 0 and 100
        }
    };

    Ok(CpuCoreInfo {
        core_id,
        current_frequency_mhz,
        min_frequency_mhz,
        max_frequency_mhz,
        usage_percent,
        temperature_celsius,
    })
}

/// Finds core-specific temperature
fn get_temperature_for_core(hw_path: &Path, core_id: u32, label_prefix: &str) -> Option<f32> {
    for i in 1..=32 {
        // Increased range to handle systems with many sensors
        let label_path = hw_path.join(format!("temp{}_label", i));
        let input_path = hw_path.join(format!("temp{}_input", i));

        if label_path.exists() && input_path.exists() {
            if let Ok(label) = read_sysfs_file_trimmed(&label_path) {
                // Match various common label formats:
                // "Core X", "core X", "Core-X", "CPU Core X", etc.
                let core_pattern = format!("{} {}", label_prefix, core_id);
                let alt_pattern = format!("{}-{}", label_prefix, core_id);

                if label.eq_ignore_ascii_case(&core_pattern)
                    || label.eq_ignore_ascii_case(&alt_pattern)
                    || label
                        .to_lowercase()
                        .contains(&format!("core {}", core_id).to_lowercase())
                {
                    if let Ok(temp_mc) = read_sysfs_value::<i32>(&input_path) {
                        return Some(temp_mc as f32 / 1000.0);
                    }
                }
            }
        }
    }
    None
}

// Finds generic sensor temperatures by label
fn get_generic_sensor_temperature(hw_path: &Path, label_name: &str) -> Option<f32> {
    for i in 1..=32 {
        let label_path = hw_path.join(format!("temp{}_label", i));
        let input_path = hw_path.join(format!("temp{}_input", i));

        if label_path.exists() && input_path.exists() {
            if let Ok(label) = read_sysfs_file_trimmed(&label_path) {
                if label.eq_ignore_ascii_case(label_name)
                    || label.to_lowercase().contains(&label_name.to_lowercase())
                {
                    if let Ok(temp_mc) = read_sysfs_value::<i32>(&input_path) {
                        return Some(temp_mc as f32 / 1000.0);
                    }
                }
            }
        } else if !label_path.exists() && input_path.exists() {
            // Some sensors might not have labels but still have valid temp inputs
            if let Ok(temp_mc) = read_sysfs_value::<i32>(&input_path) {
                return Some(temp_mc as f32 / 1000.0);
            }
        }
    }
    None
}

// Fallback to any temperature reading from a sensor
fn get_fallback_temperature(hw_path: &Path) -> Option<f32> {
    for i in 1..=32 {
        let input_path = hw_path.join(format!("temp{}_input", i));

        if input_path.exists() {
            if let Ok(temp_mc) = read_sysfs_value::<i32>(&input_path) {
                return Some(temp_mc as f32 / 1000.0);
            }
        }
    }
    None
}

pub fn get_all_cpu_core_info() -> Result<Vec<CpuCoreInfo>> {
    let initial_cpu_times = read_all_cpu_times()?;
    thread::sleep(Duration::from_millis(250)); // Interval for CPU usage calculation
    let final_cpu_times = read_all_cpu_times()?;

    let num_cores = get_logical_core_count()?; // Or derive from keys in cpu_times
    let mut core_infos = Vec::with_capacity(num_cores as usize);

    for core_id in 0..num_cores {
        if let (Some(prev), Some(curr)) = (
            initial_cpu_times.get(&core_id),
            final_cpu_times.get(&core_id),
        ) {
            match get_cpu_core_info(core_id, prev, curr) {
                Ok(info) => core_infos.push(info),
                Err(e) => {
                    // Log or handle error for a single core, maybe push a partial info or skip
                    eprintln!("Error getting info for core {}: {}", core_id, e);
                }
            }
        } else {
            // Log or handle missing times for a core
            eprintln!("Missing CPU time data for core {}", core_id);
        }
    }
    Ok(core_infos)
}

pub fn get_cpu_global_info(cpu_cores: &[CpuCoreInfo]) -> Result<CpuGlobalInfo> {
    // FIXME: Assume global settings can be read from cpu0 or are consistent.
    // This might not work properly for heterogeneous systems (e.g. big.LITTLE)
    let cpufreq_base = Path::new("/sys/devices/system/cpu/cpu0/cpufreq/");

    let current_governor = if cpufreq_base.join("scaling_governor").exists() {
        read_sysfs_file_trimmed(cpufreq_base.join("scaling_governor")).ok()
    } else {
        None
    };

    let available_governors = if cpufreq_base.join("scaling_available_governors").exists() {
        read_sysfs_file_trimmed(cpufreq_base.join("scaling_available_governors"))
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_else(|_| vec![])
    } else {
        vec![]
    };

    let turbo_status = if Path::new("/sys/devices/system/cpu/intel_pstate/no_turbo").exists() {
        // 0 means turbo enabled, 1 means disabled for intel_pstate
        read_sysfs_value::<u8>("/sys/devices/system/cpu/intel_pstate/no_turbo")
            .map(|val| val == 0)
            .ok()
    } else if Path::new("/sys/devices/system/cpu/cpufreq/boost").exists() {
        // 1 means turbo enabled, 0 means disabled for generic cpufreq boost
        read_sysfs_value::<u8>("/sys/devices/system/cpu/cpufreq/boost")
            .map(|val| val == 1)
            .ok()
    } else {
        None
    };

    let epp = read_sysfs_file_trimmed(cpufreq_base.join("energy_performance_preference")).ok();

    // EPB is often an integer 0-15. Reading as string for now.
    let epb = read_sysfs_file_trimmed(cpufreq_base.join("energy_performance_bias")).ok();

    let platform_profile = read_sysfs_file_trimmed("/sys/firmware/acpi/platform_profile").ok();
    let _platform_profile_choices =
        read_sysfs_file_trimmed("/sys/firmware/acpi/platform_profile_choices").ok();

    // Calculate average CPU temperature from the core temperatures
    let average_temperature_celsius = if !cpu_cores.is_empty() {
        // Filter cores with temperature readings, then calculate average
        let cores_with_temp: Vec<&CpuCoreInfo> = cpu_cores
            .iter()
            .filter(|core| core.temperature_celsius.is_some())
            .collect();

        if !cores_with_temp.is_empty() {
            // Sum up all temperatures and divide by count
            let sum: f32 = cores_with_temp
                .iter()
                .map(|core| core.temperature_celsius.unwrap())
                .sum();
            Some(sum / cores_with_temp.len() as f32)
        } else {
            None
        }
    } else {
        None
    };

    Ok(CpuGlobalInfo {
        current_governor,
        available_governors,
        turbo_status,
        epp,
        epb,
        platform_profile,
        average_temperature_celsius,
    })
}

pub fn get_battery_info(config: &AppConfig) -> Result<Vec<BatteryInfo>> {
    let mut batteries = Vec::new();
    let power_supply_path = Path::new("/sys/class/power_supply");

    if !power_supply_path.exists() {
        return Ok(batteries); // no power supply directory
    }

    let ignored_supplies = config
        .ignored_power_supplies
        .as_ref()
        .cloned()
        .unwrap_or_default();

    // Determine overall AC connection status
    let mut overall_ac_connected = false;
    for entry in fs::read_dir(power_supply_path)? {
        let entry = entry?;
        let ps_path = entry.path();
        let name = entry.file_name().into_string().unwrap_or_default();

        // Check for AC adapter type (common names: AC, ACAD, ADP)
        if let Ok(ps_type) = read_sysfs_file_trimmed(ps_path.join("type")) {
            if ps_type == "Mains"
                || ps_type == "USB_PD_DRP"
                || ps_type == "USB_PD"
                || ps_type == "USB_DCP"
                || ps_type == "USB_CDP"
                || ps_type == "USB_ACA"
            {
                // USB types can also provide power
                if let Ok(online) = read_sysfs_value::<u8>(ps_path.join("online")) {
                    if online == 1 {
                        overall_ac_connected = true;
                        break;
                    }
                }
            }
        } else if name.starts_with("AC") || name.contains("ACAD") || name.contains("ADP") {
            // fallback for type file missing
            if let Ok(online) = read_sysfs_value::<u8>(ps_path.join("online")) {
                if online == 1 {
                    overall_ac_connected = true;
                    break;
                }
            }
        }
    }

    for entry in fs::read_dir(power_supply_path)? {
        let entry = entry?;
        let ps_path = entry.path();
        let name = entry.file_name().into_string().unwrap_or_default();

        if ignored_supplies.contains(&name) {
            continue;
        }

        if let Ok(ps_type) = read_sysfs_file_trimmed(ps_path.join("type")) {
            if ps_type == "Battery" {
                let status_str = read_sysfs_file_trimmed(ps_path.join("status")).ok();
                let capacity_percent = read_sysfs_value::<u8>(ps_path.join("capacity")).ok();

                let power_rate_watts = if ps_path.join("power_now").exists() {
                    read_sysfs_value::<i32>(ps_path.join("power_now")) // uW
                        .map(|uw| uw as f32 / 1_000_000.0)
                        .ok()
                } else if ps_path.join("current_now").exists()
                    && ps_path.join("voltage_now").exists()
                {
                    let current_ua = read_sysfs_value::<i32>(ps_path.join("current_now")).ok(); // uA
                    let voltage_uv = read_sysfs_value::<i32>(ps_path.join("voltage_now")).ok(); // uV
                    if let (Some(c), Some(v)) = (current_ua, voltage_uv) {
                        // Power (W) = (Voltage (V) * Current (A))
                        // (v / 1e6 V) * (c / 1e6 A) = (v * c / 1e12) W
                        Some((c as f64 * v as f64 / 1_000_000_000_000.0) as f32)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let charge_start_threshold =
                    read_sysfs_value::<u8>(ps_path.join("charge_control_start_threshold")).ok();
                let charge_stop_threshold =
                    read_sysfs_value::<u8>(ps_path.join("charge_control_end_threshold")).ok();

                batteries.push(BatteryInfo {
                    name: name.clone(),
                    ac_connected: overall_ac_connected,
                    charging_state: status_str,
                    capacity_percent,
                    power_rate_watts,
                    charge_start_threshold,
                    charge_stop_threshold,
                });
            }
        }
    }
    Ok(batteries)
}

pub fn get_system_load() -> Result<SystemLoad> {
    let loadavg_str = read_sysfs_file_trimmed("/proc/loadavg")?;
    let parts: Vec<&str> = loadavg_str.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(SysMonitorError::ParseError(
            "Could not parse /proc/loadavg: expected at least 3 parts".to_string(),
        ));
    }
    let load_avg_1min = parts[0].parse().map_err(|_| {
        SysMonitorError::ParseError(format!("Failed to parse 1min load: {}", parts[0]))
    })?;
    let load_avg_5min = parts[1].parse().map_err(|_| {
        SysMonitorError::ParseError(format!("Failed to parse 5min load: {}", parts[1]))
    })?;
    let load_avg_15min = parts[2].parse().map_err(|_| {
        SysMonitorError::ParseError(format!("Failed to parse 15min load: {}", parts[2]))
    })?;

    Ok(SystemLoad {
        load_avg_1min,
        load_avg_5min,
        load_avg_15min,
    })
}

pub fn collect_system_report(config: &AppConfig) -> Result<SystemReport> {
    let system_info = get_system_info()?;
    let cpu_cores = get_all_cpu_core_info()?;
    let cpu_global = get_cpu_global_info(&cpu_cores)?;
    let batteries = get_battery_info(config)?;
    let system_load = get_system_load()?;

    Ok(SystemReport {
        system_info,
        cpu_cores,
        cpu_global,
        batteries,
        system_load,
        timestamp: SystemTime::now(),
    })
}
