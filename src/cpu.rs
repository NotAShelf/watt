use crate::core::TurboSetting;
use core::str;
use std::{fs, io, path::Path, string::ToString};

#[derive(Debug)]
pub enum ControlError {
    Io(io::Error),
    WriteError(String),
    InvalidValueError(String),
    NotSupported(String),
    PermissionDenied(String),
    InvalidProfile(String),
}

impl From<io::Error> for ControlError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied(err.to_string()),
            _ => Self::Io(err),
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::WriteError(s) => write!(f, "Failed to write to sysfs path: {s}"),
            Self::InvalidValueError(s) => write!(f, "Invalid value for setting: {s}"),
            Self::NotSupported(s) => write!(f, "Control action not supported: {s}"),
            Self::PermissionDenied(s) => {
                write!(f, "Permission denied: {s}. Try running with sudo.")
            }
            Self::InvalidProfile(s) => {
                write!(
                    f,
                    "Invalid platform control profile {s} supplied, please provide a valid one."
                )
            }
        }
    }
}

impl std::error::Error for ControlError {}

pub type Result<T, E = ControlError> = std::result::Result<T, E>;

// Write a value to a sysfs file
fn write_sysfs_value(path: impl AsRef<Path>, value: &str) -> Result<()> {
    let p = path.as_ref();
    fs::write(p, value).map_err(|e| {
        let error_msg = format!("Path: {:?}, Value: '{}', Error: {}", p.display(), value, e);
        if e.kind() == io::ErrorKind::PermissionDenied {
            ControlError::PermissionDenied(error_msg)
        } else {
            ControlError::WriteError(error_msg)
        }
    })
}

fn for_each_cpu_core<F>(mut action: F) -> Result<()>
where
    F: FnMut(u32) -> Result<()>,
{
    // Using num_cpus::get() for a reliable count of logical cores accessible.
    // The monitor module's get_logical_core_count might be more specific to cpufreq-capable cores,
    // but for applying settings, we might want to iterate over all reported by OS.
    // However, settings usually apply to cores with cpufreq.
    // Let's use a similar discovery to monitor's get_logical_core_count
    let mut cores_to_act_on = Vec::new();
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

        if let Ok(core_id) = name[3..].parse::<u32>() {
            cores_to_act_on.push(core_id);
        }
    }
    if cores_to_act_on.is_empty() {
        // Fallback if sysfs iteration above fails to find any cpufreq cores
        #[allow(clippy::cast_possible_truncation)]
        let num_cores = num_cpus::get() as u32;
        for core_id in 0..num_cores {
            cores_to_act_on.push(core_id);
        }
    }

    for core_id in cores_to_act_on {
        action(core_id)?;
    }
    Ok(())
}

pub fn set_governor(governor: &str, core_id: Option<u32>) -> Result<()> {
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

pub fn set_turbo(setting: TurboSetting) -> Result<()> {
    let value_pstate = match setting {
        TurboSetting::Always => "0", // no_turbo = 0 means turbo is enabled
        TurboSetting::Never => "1",  // no_turbo = 1 means turbo is disabled
        TurboSetting::Auto => return Err(ControlError::InvalidValueError("Turbo Auto cannot be directly set via intel_pstate/no_turbo or cpufreq/boost. System default.".to_string())),
    };
    let value_boost = match setting {
        TurboSetting::Always => "1", // boost = 1 means turbo is enabled
        TurboSetting::Never => "0",  // boost = 0 means turbo is disabled
         TurboSetting::Auto => return Err(ControlError::InvalidValueError("Turbo Auto cannot be directly set via intel_pstate/no_turbo or cpufreq/boost. System default.".to_string())),
    };

    let pstate_path = "/sys/devices/system/cpu/intel_pstate/no_turbo";
    let boost_path = "/sys/devices/system/cpu/cpufreq/boost";

    if Path::new(pstate_path).exists() {
        write_sysfs_value(pstate_path, value_pstate)
    } else if Path::new(boost_path).exists() {
        write_sysfs_value(boost_path, value_boost)
    } else {
        Err(ControlError::NotSupported(
            "Neither intel_pstate/no_turbo nor cpufreq/boost found.".to_string(),
        ))
    }
}

pub fn set_epp(epp: &str, core_id: Option<u32>) -> Result<()> {
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

pub fn set_epb(epb: &str, core_id: Option<u32>) -> Result<()> {
    // EPB is often an integer 0-15. Ensure `epb` string is valid if parsing.
    // For now, writing it directly as a string.
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

pub fn set_min_frequency(freq_mhz: u32, core_id: Option<u32>) -> Result<()> {
    let freq_khz_str = (freq_mhz * 1000).to_string();
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
    let freq_khz_str = (freq_mhz * 1000).to_string();
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
/// Returns [`ControlError::NotSupported`] if:
/// - The file `/sys/firmware/acpi/platform_profile_choices` does not exist.
/// - The file `/sys/firmware/acpi/platform_profile_choices` is empty.
///
/// Returns [`ControlError::PermissionDenied`] if the file `/sys/firmware/acpi/platform_profile_choices` cannot be read.
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
