use crate::config::AppConfig;
use crate::core::{BatteryInfo, CpuCoreInfo, CpuGlobalInfo, SystemInfo, SystemLoad, SystemReport};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    thread,
    time::Duration,
    time::SystemTime,
};

pub fn get_system_info() -> SystemInfo {
    let cpu_model = get_cpu_model().unwrap_or_else(|_| "Unknown".to_string());

    SystemInfo { cpu_model }
}

pub fn get_cpu_core_info(
    core_id: u32,
    prev_times: &CpuTimes,
    current_times: &CpuTimes,
) -> anyhow::Result<CpuCoreInfo> {
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

    Ok(CpuCoreInfo {
        core_id,
        temperature_celsius,
    })
}

/// Finds core-specific temperature
fn get_temperature_for_core(hw_path: &Path, core_id: u32, label_prefix: &str) -> Option<f32> {
    for i in 1..=32 {
        // Increased range to handle systems with many sensors
        let label_path = hw_path.join(format!("temp{i}_label"));
        let input_path = hw_path.join(format!("temp{i}_input"));

        if label_path.exists() && input_path.exists() {
            if let Ok(label) = read_sysfs_file_trimmed(&label_path) {
                // Match various common label formats:
                // "Core X", "core X", "Core-X", "CPU Core X", etc.
                let core_pattern = format!("{label_prefix} {core_id}");
                let alt_pattern = format!("{label_prefix}-{core_id}");

                if label.eq_ignore_ascii_case(&core_pattern)
                    || label.eq_ignore_ascii_case(&alt_pattern)
                    || label
                        .to_lowercase()
                        .contains(&format!("core {core_id}").to_lowercase())
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
        let label_path = hw_path.join(format!("temp{i}_label"));
        let input_path = hw_path.join(format!("temp{i}_input"));

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
        let input_path = hw_path.join(format!("temp{i}_input"));

        if input_path.exists() {
            if let Ok(temp_mc) = read_sysfs_value::<i32>(&input_path) {
                return Some(temp_mc as f32 / 1000.0);
            }
        }
    }
    None
}

pub fn get_all_cpu_core_info() -> anyhow::Result<Vec<CpuCoreInfo>> {
    let initial_cpu_times = read_all_cpu_times()?;
    thread::sleep(Duration::from_millis(250)); // interval for CPU usage calculation
    let final_cpu_times = read_all_cpu_times()?;

    let num_cores = get_real_cpus()
        .map_err(|_| SysMonitorError::ReadError("Could not get the number of cores".to_string()))?;

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
                    eprintln!("Error getting info for core {core_id}: {e}");
                }
            }
        } else {
            // Log or handle missing times for a core
            eprintln!("Missing CPU time data for core {core_id}");
        }
    }
    Ok(core_infos)
}

pub fn get_cpu_global_info(cpu_cores: &[CpuCoreInfo]) -> CpuGlobalInfo {
    // Calculate average CPU temperature from the core temperatures
    let average_temperature_celsius = if cpu_cores.is_empty() {
        None
    } else {
        // Filter cores with temperature readings, then calculate average
        let cores_with_temp: Vec<&CpuCoreInfo> = cpu_cores
            .iter()
            .filter(|core| core.temperature_celsius.is_some())
            .collect();

        if cores_with_temp.is_empty() {
            None
        } else {
            // Sum up all temperatures and divide by count
            let sum: f32 = cores_with_temp
                .iter()
                .map(|core| core.temperature_celsius.unwrap())
                .sum();
            Some(sum / cores_with_temp.len() as f32)
        }
    };

    // Return the constructed CpuGlobalInfo
    CpuGlobalInfo {
        average_temperature_celsius,
    }
}

pub fn get_cpu_model() -> anyhow::Result<String> {
    let path = Path::new("/proc/cpuinfo");
    let content = fs::read_to_string(path).map_err(|_| {
        SysMonitorError::ReadError(format!("Cannot read contents of {}.", path.display()))
    })?;

    for line in content.lines() {
        if line.starts_with("model name") {
            if let Some(val) = line.split(':').nth(1) {
                let cpu_model = val.trim().to_string();
                return Ok(cpu_model);
            }
        }
    }
    Err(SysMonitorError::ParseError(
        "Could not find CPU model name in /proc/cpuinfo.".to_string(),
    ))
}
