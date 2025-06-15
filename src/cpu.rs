use crate::core::{GovernorOverrideMode, TurboSetting};
use crate::util::error::ControlError;
use core::str;
use log::debug;
use std::{fs, io, path::Path, string::ToString};

pub type Result<T, E = ControlError> = std::result::Result<T, E>;

// Valid EPB string values
const VALID_EPB_STRINGS: &[&str] = &[
    "performance",
    "balance-performance",
    "balance_performance", // alternative form
    "balance-power",
    "balance_power", // alternative form
    "power",
];

// EPP (Energy Performance Preference) string values
const EPP_FALLBACK_VALUES: &[&str] = &[
    "default",
    "performance",
    "balance-performance",
    "balance_performance", // alternative form with underscore
    "balance-power",
    "balance_power", // alternative form with underscore
    "power",
];

// Write a value to a sysfs file
fn write_sysfs_value(path: impl AsRef<Path>, value: &str) -> Result<()> {
    let p = path.as_ref();

    fs::write(p, value).map_err(|e| {
        let error_msg = format!("Path: {:?}, Value: '{}', Error: {}", p.display(), value, e);
        match e.kind() {
            io::ErrorKind::PermissionDenied => ControlError::PermissionDenied(error_msg),
            io::ErrorKind::NotFound => {
                ControlError::PathMissing(format!("Path '{}' does not exist", p.display()))
            }
            _ => ControlError::WriteError(error_msg),
        }
    })
}

pub fn get_logical_core_count() -> Result<u32> {
    // Using num_cpus::get() for a reliable count of logical cores accessible.
    // The monitor module's get_logical_core_count might be more specific to cpufreq-capable cores,
    // but for applying settings, we might want to iterate over all reported by OS.
    // However, settings usually apply to cores with cpufreq.
    // Let's use a similar discovery to monitor's get_logical_core_count
    let mut num_cores: u32 = 0;
    let path = Path::new("/sys/devices/system/cpu");
    if !path.exists() {
        return Err(ControlError::NotSupported(format!(
            "No logical cores found at {}.",
            path.display()
        )));
    }

    let entries = fs::read_dir(path)
        .map_err(|_| {
            ControlError::PermissionDenied(format!("Cannot read contents of {}.", path.display()))
        })?
        .flatten();

    for entry in entries {
        let entry_file_name = entry.file_name();
        let Some(name) = entry_file_name.to_str() else {
            continue;
        };

        // Skip non-CPU directories (e.g., cpuidle, cpufreq)
        if !name.starts_with("cpu") || name.len() <= 3 || !name[3..].chars().all(char::is_numeric) {
            continue;
        }

        if !entry.path().join("cpufreq").exists() {
            continue;
        }

        if name[3..].parse::<u32>().is_ok() {
            num_cores += 1;
        }
    }
    if num_cores == 0 {
        // Fallback if sysfs iteration above fails to find any cpufreq cores
        num_cores = num_cpus::get() as u32;
    }

    Ok(num_cores)
}

fn for_each_cpu_core<F>(mut action: F) -> Result<()>
where
    F: FnMut(u32) -> Result<()>,
{
    let num_cores: u32 = get_logical_core_count()?;

    for core_id in 0u32..num_cores {
        action(core_id)?;
    }
    Ok(())
}

pub fn set_governor(governor: &str, core_id: Option<u32>) -> Result<()> {
    // Validate the governor is available on this system
    // This returns both the validation result and the list of available governors
    let (is_valid, available_governors) = is_governor_valid(governor)?;

    if !is_valid {
        return Err(ControlError::InvalidGovernor(format!(
            "Governor '{}' is not available on this system. Valid governors: {}",
            governor,
            available_governors.join(", ")
        )));
    }

    let action = |id: u32| {
        let path = format!("/sys/devices/system/cpu/cpu{id}/cpufreq/scaling_governor");
        if Path::new(&path).exists() {
            write_sysfs_value(&path, governor)
        } else {
            // Silently ignore if the path doesn't exist for a specific core,
            // as not all cores might have cpufreq (e.g. offline cores)
            Ok(())
        }
    };

    core_id.map_or_else(|| for_each_cpu_core(action), action)
}

/// Check if the provided governor is available in the system
/// Returns a tuple of (`is_valid`, `available_governors`) to avoid redundant file reads
fn is_governor_valid(governor: &str) -> Result<(bool, Vec<String>)> {
    let governors = get_available_governors()?;

    // Convert input governor to lowercase for case-insensitive comparison
    let governor_lower = governor.to_lowercase();

    // Convert all available governors to lowercase for comparison
    let governors_lower: Vec<String> = governors.iter().map(|g| g.to_lowercase()).collect();

    // Check if the lowercase governor is in the lowercase list
    Ok((governors_lower.contains(&governor_lower), governors))
}

/// Get available CPU governors from the system
fn get_available_governors() -> Result<Vec<String>> {
    let cpu_base_path = Path::new("/sys/devices/system/cpu");

    // First try the traditional path with cpu0. This is the most common case
    // and will usually catch early, but we should try to keep the code to handle
    // "edge" cases lightweight, for the (albeit smaller) number of users that
    // run Superfreq on unusual systems.
    let cpu0_path = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors";
    if Path::new(cpu0_path).exists() {
        let content = fs::read_to_string(cpu0_path).map_err(|e| {
            ControlError::ReadError(format!("Failed to read available governors from cpu0: {e}"))
        })?;

        let governors: Vec<String> = content
            .split_whitespace()
            .map(ToString::to_string)
            .collect();

        if !governors.is_empty() {
            return Ok(governors);
        }
    }

    // If cpu0 doesn't have the file or it's empty, scan all CPUs
    // This handles heterogeneous systems where cpu0 might not have cpufreq
    if let Ok(entries) = fs::read_dir(cpu_base_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let name = match file_name.to_str() {
                Some(name) => name,
                None => continue,
            };

            // Skip non-CPU directories
            if !name.starts_with("cpu")
                || name.len() <= 3
                || !name[3..].chars().all(char::is_numeric)
            {
                continue;
            }

            let governor_path = path.join("cpufreq/scaling_available_governors");
            if governor_path.exists() {
                match fs::read_to_string(&governor_path) {
                    Ok(content) => {
                        let governors: Vec<String> = content
                            .split_whitespace()
                            .map(ToString::to_string)
                            .collect();

                        if !governors.is_empty() {
                            return Ok(governors);
                        }
                    }
                    Err(_) => continue, // try next CPU if this one fails
                }
            }
        }
    }

    // If we get here, we couldn't find any valid governors list
    Err(ControlError::NotSupported(
        "Could not determine available governors on any CPU".to_string(),
    ))
}

pub fn set_turbo(setting: TurboSetting) -> Result<()> {
    let value_pstate = match setting {
        TurboSetting::Always => "0", // no_turbo = 0 means turbo is enabled
        TurboSetting::Never => "1",  // no_turbo = 1 means turbo is disabled
        // Auto mode is handled at the engine level, not directly at the sysfs level
        TurboSetting::Auto => {
            debug!("Turbo Auto mode is managed by engine logic based on system conditions");
            return Ok(());
        }
    };
    let value_boost = match setting {
        TurboSetting::Always => "1", // boost = 1 means turbo is enabled
        TurboSetting::Never => "0",  // boost = 0 means turbo is disabled
        TurboSetting::Auto => {
            debug!("Turbo Auto mode is managed by engine logic based on system conditions");
            return Ok(());
        }
    };

    // AMD specific paths
    let amd_pstate_path = "/sys/devices/system/cpu/amd_pstate/cpufreq/boost";
    let msr_boost_path = "/sys/devices/system/cpu/cpufreq/amd_pstate_enable_boost";

    // Path priority (from most to least specific)
    let pstate_path = "/sys/devices/system/cpu/intel_pstate/no_turbo";
    let boost_path = "/sys/devices/system/cpu/cpufreq/boost";

    // Try each boost control path in order of specificity
    if Path::new(pstate_path).exists() {
        write_sysfs_value(pstate_path, value_pstate)
    } else if Path::new(amd_pstate_path).exists() {
        write_sysfs_value(amd_pstate_path, value_boost)
    } else if Path::new(msr_boost_path).exists() {
        write_sysfs_value(msr_boost_path, value_boost)
    } else if Path::new(boost_path).exists() {
        write_sysfs_value(boost_path, value_boost)
    } else {
        // Also try per-core cpufreq boost for some AMD systems
        let result = try_set_per_core_boost(value_boost)?;
        if result {
            Ok(())
        } else {
            Err(ControlError::NotSupported(
                "No supported CPU boost control mechanism found.".to_string(),
            ))
        }
    }
}

/// Try to set boost on a per-core basis for systems that support it
fn try_set_per_core_boost(value: &str) -> Result<bool> {
    let mut success = false;
    let num_cores = get_logical_core_count()?;

    for core_id in 0..num_cores {
        let boost_path = format!("/sys/devices/system/cpu/cpu{core_id}/cpufreq/boost");

        if Path::new(&boost_path).exists() {
            write_sysfs_value(&boost_path, value)?;
            success = true;
        }
    }

    Ok(success)
}

pub fn set_epp(epp: &str, core_id: Option<u32>) -> Result<()> {
    // Validate the EPP value against available options
    let available_epp = get_available_epp_values()?;
    if !available_epp.iter().any(|v| v.eq_ignore_ascii_case(epp)) {
        return Err(ControlError::InvalidValueError(format!(
            "Invalid EPP value: '{}'. Available values: {}",
            epp,
            available_epp.join(", ")
        )));
    }

    let action = |id: u32| {
        let path = format!("/sys/devices/system/cpu/cpu{id}/cpufreq/energy_performance_preference");
        if Path::new(&path).exists() {
            write_sysfs_value(&path, epp)
        } else {
            Ok(())
        }
    };
    core_id.map_or_else(|| for_each_cpu_core(action), action)
}

/// Get available EPP values from the system
fn get_available_epp_values() -> Result<Vec<String>> {
    let path = "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_available_preferences";

    if !Path::new(path).exists() {
        // If the file doesn't exist, fall back to a default set of common values
        // This is safer than failing outright, as some systems may allow these values     │
        // even without explicitly listing them
        return Ok(EPP_FALLBACK_VALUES.iter().map(|&s| s.to_string()).collect());
    }

    let content = fs::read_to_string(path).map_err(|e| {
        ControlError::ReadError(format!("Failed to read available EPP values: {e}"))
    })?;

    Ok(content
        .split_whitespace()
        .map(ToString::to_string)
        .collect())
}

pub fn set_epb(epb: &str, core_id: Option<u32>) -> Result<()> {
    // Validate EPB value - should be a number 0-15 or a recognized string value
    validate_epb_value(epb)?;

    let action = |id: u32| {
        let path = format!("/sys/devices/system/cpu/cpu{id}/cpufreq/energy_performance_bias");
        if Path::new(&path).exists() {
            write_sysfs_value(&path, epb)
        } else {
            Ok(())
        }
    };
    core_id.map_or_else(|| for_each_cpu_core(action), action)
}

fn validate_epb_value(epb: &str) -> Result<()> {
    // EPB can be a number from 0-15 or a recognized string
    // Try parsing as a number first
    if let Ok(value) = epb.parse::<u8>() {
        if value <= 15 {
            return Ok(());
        }
        return Err(ControlError::InvalidValueError(format!(
            "EPB numeric value must be between 0 and 15, got {value}"
        )));
    }

    // If not a number, check if it's a recognized string value.
    // This is using case-insensitive comparison
    if VALID_EPB_STRINGS
        .iter()
        .any(|valid| valid.eq_ignore_ascii_case(epb))
    {
        Ok(())
    } else {
        Err(ControlError::InvalidValueError(format!(
            "Invalid EPB value: '{}'. Must be a number 0-15 or one of: {}",
            epb,
            VALID_EPB_STRINGS.join(", ")
        )))
    }
}

pub fn set_min_frequency(freq_mhz: u32, core_id: Option<u32>) -> Result<()> {
    // Check if the new minimum frequency would be greater than current maximum
    if let Some(id) = core_id {
        validate_min_frequency(id, freq_mhz)?;
    } else {
        // Check for all cores
        let num_cores = get_logical_core_count()?;
        for id in 0..num_cores {
            validate_min_frequency(id, freq_mhz)?;
        }
    }

    // XXX: We use u64 for the intermediate calculation to prevent overflow
    let freq_khz = u64::from(freq_mhz) * 1000;
    let freq_khz_str = freq_khz.to_string();

    let action = |id: u32| {
        let path = format!("/sys/devices/system/cpu/cpu{id}/cpufreq/scaling_min_freq");
        if Path::new(&path).exists() {
            write_sysfs_value(&path, &freq_khz_str)
        } else {
            Ok(())
        }
    };
    core_id.map_or_else(|| for_each_cpu_core(action), action)
}

pub fn set_max_frequency(freq_mhz: u32, core_id: Option<u32>) -> Result<()> {
    // Check if the new maximum frequency would be less than current minimum
    if let Some(id) = core_id {
        validate_max_frequency(id, freq_mhz)?;
    } else {
        // Check for all cores
        let num_cores = get_logical_core_count()?;
        for id in 0..num_cores {
            validate_max_frequency(id, freq_mhz)?;
        }
    }

    // XXX: Use a u64 here as well.
    let freq_khz = u64::from(freq_mhz) * 1000;
    let freq_khz_str = freq_khz.to_string();

    let action = |id: u32| {
        let path = format!("/sys/devices/system/cpu/cpu{id}/cpufreq/scaling_max_freq");
        if Path::new(&path).exists() {
            write_sysfs_value(&path, &freq_khz_str)
        } else {
            Ok(())
        }
    };
    core_id.map_or_else(|| for_each_cpu_core(action), action)
}

fn read_sysfs_value_as_u32(path: &str) -> Result<u32> {
    if !Path::new(path).exists() {
        return Err(ControlError::NotSupported(format!(
            "File does not exist: {path}"
        )));
    }

    let content = fs::read_to_string(path)
        .map_err(|e| ControlError::ReadError(format!("Failed to read {path}: {e}")))?;

    content
        .trim()
        .parse::<u32>()
        .map_err(|e| ControlError::ParseError(format!("Failed to parse value from {path}: {e}")))
}

fn validate_min_frequency(core_id: u32, new_min_freq_mhz: u32) -> Result<()> {
    let max_freq_path = format!("/sys/devices/system/cpu/cpu{core_id}/cpufreq/scaling_max_freq");

    if !Path::new(&max_freq_path).exists() {
        return Ok(());
    }

    let max_freq_khz = read_sysfs_value_as_u32(&max_freq_path)?;
    let new_min_freq_khz = new_min_freq_mhz * 1000;

    if new_min_freq_khz > max_freq_khz {
        return Err(ControlError::InvalidValueError(format!(
            "Minimum frequency ({} MHz) cannot be higher than maximum frequency ({} MHz) for core {}",
            new_min_freq_mhz,
            max_freq_khz / 1000,
            core_id
        )));
    }

    Ok(())
}

fn validate_max_frequency(core_id: u32, new_max_freq_mhz: u32) -> Result<()> {
    let min_freq_path = format!("/sys/devices/system/cpu/cpu{core_id}/cpufreq/scaling_min_freq");

    if !Path::new(&min_freq_path).exists() {
        return Ok(());
    }

    let min_freq_khz = read_sysfs_value_as_u32(&min_freq_path)?;
    let new_max_freq_khz = new_max_freq_mhz * 1000;

    if new_max_freq_khz < min_freq_khz {
        return Err(ControlError::InvalidValueError(format!(
            "Maximum frequency ({} MHz) cannot be lower than minimum frequency ({} MHz) for core {}",
            new_max_freq_mhz,
            min_freq_khz / 1000,
            core_id
        )));
    }

    Ok(())
}

/// Sets the platform profile.
/// This changes the system performance, temperature, fan, and other hardware replated characteristics.
///
/// Also see [`The Kernel docs`] for this.
///
/// [`The Kernel docs`]: <https://docs.kernel.org/userspace-api/sysfs-platform_profile.html>
///
/// # Examples
///
/// ```
/// set_platform_profile("balanced");
/// ```
///
pub fn set_platform_profile(profile: &str) -> Result<()> {
    let path = "/sys/firmware/acpi/platform_profile";
    if !Path::new(path).exists() {
        return Err(ControlError::NotSupported(format!(
            "Platform profile control not found at {path}.",
        )));
    }

    let available_profiles = get_platform_profiles()?;

    if !available_profiles.contains(&profile.to_string()) {
        return Err(ControlError::InvalidProfile(format!(
            "Invalid platform control profile provided.\n\
             Provided profile: {} \n\
             Available profiles:\n\
             {}",
            profile,
            available_profiles.join(", ")
        )));
    }
    write_sysfs_value(path, profile)
}

/// Returns the list of available platform profiles.
///
/// # Errors
///
/// # Returns
///
/// - [`ControlError::NotSupported`] if:
///   - The file `/sys/firmware/acpi/platform_profile_choices` does not exist.
///   - The file `/sys/firmware/acpi/platform_profile_choices` is empty.
///
/// - [`ControlError::PermissionDenied`] if the file `/sys/firmware/acpi/platform_profile_choices` cannot be read.
///
pub fn get_platform_profiles() -> Result<Vec<String>> {
    let path = "/sys/firmware/acpi/platform_profile_choices";

    if !Path::new(path).exists() {
        return Err(ControlError::NotSupported(format!(
            "Platform profile choices not found at {path}."
        )));
    }

    let content = fs::read_to_string(path)
        .map_err(|_| ControlError::PermissionDenied(format!("Cannot read contents of {path}.")))?;

    Ok(content
        .split_whitespace()
        .map(ToString::to_string)
        .collect())
}

/// Path for storing the governor override state
const GOVERNOR_OVERRIDE_PATH: &str = "/etc/xdg/superfreq/governor_override";

/// Force a specific CPU governor or reset to automatic mode
pub fn force_governor(mode: GovernorOverrideMode) -> Result<()> {
    // Create directory if it doesn't exist
    let dir_path = Path::new("/etc/xdg/superfreq");
    if !dir_path.exists() {
        fs::create_dir_all(dir_path).map_err(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied {
                ControlError::PermissionDenied(format!(
                    "Permission denied creating directory: {}. Try running with sudo.",
                    dir_path.display()
                ))
            } else {
                ControlError::Io(e)
            }
        })?;
    }

    match mode {
        GovernorOverrideMode::Reset => {
            // Remove the override file if it exists
            if Path::new(GOVERNOR_OVERRIDE_PATH).exists() {
                fs::remove_file(GOVERNOR_OVERRIDE_PATH).map_err(|e| {
                    if e.kind() == io::ErrorKind::PermissionDenied {
                        ControlError::PermissionDenied(format!(
                            "Permission denied removing override file: {GOVERNOR_OVERRIDE_PATH}. Try running with sudo."
                        ))
                    } else {
                        ControlError::Io(e)
                    }
                })?;
                println!(
                    "Governor override has been reset. Normal profile-based settings will be used."
                );
            } else {
                println!("No governor override was set.");
            }
            Ok(())
        }
        GovernorOverrideMode::Performance | GovernorOverrideMode::Powersave => {
            // Create the override file with the selected governor
            let governor = mode.to_string().to_lowercase();
            fs::write(GOVERNOR_OVERRIDE_PATH, &governor).map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    ControlError::PermissionDenied(format!(
                        "Permission denied writing to override file: {GOVERNOR_OVERRIDE_PATH}. Try running with sudo."
                    ))
                } else {
                    ControlError::Io(e)
                }
            })?;

            // Also apply the governor immediately
            set_governor(&governor, None)?;

            println!(
                "Governor override set to '{governor}'. This setting will persist across reboots."
            );
            println!("To reset, use: superfreq force-governor reset");
            Ok(())
        }
    }
}

/// Get the current governor override if set
pub fn get_governor_override() -> Option<String> {
    if Path::new(GOVERNOR_OVERRIDE_PATH).exists() {
        fs::read_to_string(GOVERNOR_OVERRIDE_PATH).ok()
    } else {
        None
    }
}
