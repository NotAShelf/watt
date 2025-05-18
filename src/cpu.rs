use anyhow::{Context, bail};
use derive_more::Display;
use serde::{Deserialize, Serialize};

use std::{fs, io, path::Path, string::ToString};

// // Valid EPP (Energy Performance Preference) string values.
// const EPP_FALLBACK_VALUES: &[&str] = &[
//     "default",
//     "performance",
//     "balance-performance",
//     "balance_performance", // Alternative form with underscore.
//     "balance-power",
//     "balance_power", // Alternative form with underscore.
//     "power",
// ];

fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();

    path.exists()
}

// Not doing any anyhow stuff here as all the calls of this ignore errors.
fn read_u64(path: impl AsRef<Path>) -> anyhow::Result<u64> {
    let path = path.as_ref();

    let content = fs::read_to_string(path)?;

    Ok(content.trim().parse::<u64>()?)
}

fn write(path: impl AsRef<Path>, value: &str) -> anyhow::Result<()> {
    let path = path.as_ref();

    fs::write(path, value).with_context(|| {
        format!(
            "failed to write '{value}' to '{path}'",
            path = path.display(),
        )
    })
}

/// Get real, tunable CPUs.
pub fn get_real_cpus() -> anyhow::Result<Vec<u32>> {
    const PATH: &str = "/sys/devices/system/cpu";

    let mut cpus = vec![];

    for entry in fs::read_dir(PATH)
        .with_context(|| format!("failed to read contents of '{PATH}'"))?
        .flatten()
    {
        let entry_file_name = entry.file_name();

        let Some(name) = entry_file_name.to_str() else {
            continue;
        };

        let Some(cpu_prefix_removed) = name.strip_prefix("cpu") else {
            continue;
        };

        // Has to match "cpu{N}".
        let Ok(cpu) = cpu_prefix_removed.parse::<u32>() else {
            continue;
        };

        // Has to match "cpu{N}/cpufreq".
        if !entry.path().join("cpufreq").exists() {
            continue;
        }

        cpus.push(cpu);
    }

    // Fall back if sysfs iteration above fails to find any cpufreq CPUs.
    if cpus.is_empty() {
        cpus = (0..num_cpus::get() as u32).collect();
    }

    Ok(cpus)
}

/// Set the governor for a CPU.
pub fn set_governor(governor: &str, cpu: u32) -> anyhow::Result<()> {
    let governors = get_available_governors_for(cpu);

    if !governors
        .iter()
        .any(|avail_governor| avail_governor == governor)
    {
        bail!(
            "governor '{governor}' is not available for CPU {cpu}. valid governors: {governors}",
            governors = governors.join(", "),
        );
    }

    write(
        format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor"),
        governor,
    )
    .with_context(|| {
        format!(
            "this probably means that CPU {cpu} doesn't exist or doesn't support changing governors"
        )
    })
}

/// Get available CPU governors for a CPU.
fn get_available_governors_for(cpu: u32) -> Vec<String> {
    let Ok(content) = fs::read_to_string(format!(
        "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_available_governors"
    )) else {
        return Vec::new();
    };

    content
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, clap::ValueEnum)]
pub enum Turbo {
    Always,
    Never,
}

pub fn set_turbo(setting: Turbo) -> anyhow::Result<()> {
    let value_boost = match setting {
        Turbo::Always => "1", // boost = 1 means turbo is enabled.
        Turbo::Never => "0",  // boost = 0 means turbo is disabled.
    };

    let value_boost_negated = match setting {
        Turbo::Always => "0", // no_turbo = 0 means turbo is enabled.
        Turbo::Never => "1",  // no_turbo = 1 means turbo is disabled.
    };

    // AMD specific paths
    let amd_boost_path = "/sys/devices/system/cpu/amd_pstate/cpufreq/boost";
    let msr_boost_path = "/sys/devices/system/cpu/cpufreq/amd_pstate_enable_boost";

    // Path priority (from most to least specific)
    let intel_boost_path_negated = "/sys/devices/system/cpu/intel_pstate/no_turbo";
    let generic_boost_path = "/sys/devices/system/cpu/cpufreq/boost";

    // Try each boost control path in order of specificity
    if write(intel_boost_path_negated, value_boost_negated).is_ok() {
        return Ok(());
    }
    if write(amd_boost_path, value_boost).is_ok() {
        return Ok(());
    }
    if write(msr_boost_path, value_boost).is_ok() {
        return Ok(());
    }
    if write(generic_boost_path, value_boost).is_ok() {
        return Ok(());
    }

    // Also try per-core cpufreq boost for some AMD systems.
    if get_real_cpus()?.iter().any(|cpu| {
        write(
            &format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/boost"),
            value_boost,
        )
        .is_ok()
    }) {
        return Ok(());
    }

    bail!("no supported CPU boost control mechanism found");
}

pub fn set_epp(epp: &str, cpu: u32) -> anyhow::Result<()> {
    // Validate the EPP value against available options
    let epps = get_available_epps(cpu);

    if !epps.iter().any(|avail_epp| avail_epp == epp) {
        bail!(
            "epp value '{epp}' is not availabile for CPU {cpu}. valid epp values: {epps}",
            epps = epps.join(", "),
        );
    }

    write(
        format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/energy_performance_preference"),
        epp,
    )
    .with_context(|| {
        format!("this probably means that CPU {cpu} doesn't exist or doesn't support changing EPP")
    })
}

/// Get available EPP values for a CPU.
fn get_available_epps(cpu: u32) -> Vec<String> {
    let Ok(content) = fs::read_to_string(format!(
        "/sys/devices/system/cpu/cpu{cpu}/cpufreq/energy_performance_available_preferences"
    )) else {
        return Vec::new();
    };

    content
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

pub fn set_epb(epb: &str, cpu: u32) -> anyhow::Result<()> {
    // Validate EPB value - should be a number 0-15 or a recognized string value.
    validate_epb_value(epb)?;

    write(
        format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/energy_performance_bias"),
        epb,
    )
    .with_context(|| {
        format!("this probably means that CPU {cpu} doesn't exist or doesn't support changing EPB")
    })
}

fn validate_epb_value(epb: &str) -> anyhow::Result<()> {
    // EPB can be a number from 0-15 or a recognized string.

    const VALID_EPB_STRINGS: &[&str] = &[
        "performance",
        "balance-performance",
        "balance_performance", // Alternative form with underscore.
        "balance-power",
        "balance_power", // Alternative form with underscore.
        "power",
    ];

    // Try parsing as a number first.
    if let Ok(value) = epb.parse::<u8>() {
        if value <= 15 {
            return Ok(());
        }

        bail!("EPB numeric value must be between 0 and 15, got {value}");
    }

    // If not a number, check if it's a recognized string value.
    if VALID_EPB_STRINGS.contains(&epb) {
        return Ok(());
    }

    bail!(
        "invalid EPB value: '{epb}'. must be a number between 0-15 inclusive or one of: {valid}",
        valid = VALID_EPB_STRINGS.join(", "),
    );
}

pub fn set_frequency_minimum(frequency_mhz: u64, cpu: u32) -> anyhow::Result<()> {
    validate_frequency_minimum(frequency_mhz, cpu)?;

    // We use u64 for the intermediate calculation to prevent overflow
    let frequency_khz = u64::from(frequency_mhz) * 1000;
    let frequency_khz = frequency_khz.to_string();

    write(
        format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_min_freq"),
        &frequency_khz,
    )
    .with_context(|| {
        format!("this probably means that CPU {cpu} doesn't exist or doesn't support changing minimum frequency")
    })
}

pub fn set_frequency_maximum(frequency_mhz: u64, cpu: u32) -> anyhow::Result<()> {
    validate_max_frequency(frequency_mhz, cpu)?;

    // We use u64 for the intermediate calculation to prevent overflow
    let frequency_khz = u64::from(frequency_mhz) * 1000;
    let frequency_khz = frequency_khz.to_string();

    write(
        format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_max_freq"),
        &frequency_khz,
    )
    .with_context(|| {
        format!("this probably means that CPU {cpu} doesn't exist or doesn't support changing maximum frequency")
    })
}

fn validate_frequency_minimum(new_frequency_mhz: u64, cpu: u32) -> anyhow::Result<()> {
    let Ok(minimum_frequency_khz) = read_u64(format!(
        "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_min_freq"
    )) else {
        // Just let it pass if we can't find anything.
        return Ok(());
    };

    if new_frequency_mhz as u64 * 1000 < minimum_frequency_khz {
        bail!(
            "new minimum frequency ({new_frequency_mhz} MHz) cannot be lower than the minimum frequency ({} MHz) for CPU {cpu}",
            minimum_frequency_khz / 1000,
        );
    }

    Ok(())
}

fn validate_max_frequency(new_frequency_mhz: u64, cpu: u32) -> anyhow::Result<()> {
    let Ok(maximum_frequency_khz) = read_u64(format!(
        "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_min_freq"
    )) else {
        // Just let it pass if we can't find anything.
        return Ok(());
    };

    if new_frequency_mhz * 1000 > maximum_frequency_khz {
        bail!(
            "new maximum frequency ({new_frequency_mhz} MHz) cannot be higher than the maximum frequency ({} MHz) for CPU {cpu}",
            maximum_frequency_khz / 1000,
        );
    }

    Ok(())
}

/// Sets the platform profile.
/// This changes the system performance, temperature, fan, and other hardware replated characteristics.
///
/// Also see [`The Kernel docs`] for this.
///
/// [`The Kernel docs`]: <https://docs.kernel.org/userspace-api/sysfs-platform_profile.html>
pub fn set_platform_profile(profile: &str) -> anyhow::Result<()> {
    let profiles = get_platform_profiles();

    if !profiles
        .iter()
        .any(|avail_profile| avail_profile == profile)
    {
        bail!(
            "profile '{profile}' is not available for system. valid profiles: {profiles}",
            profiles = profiles.join(", "),
        );
    }

    write("/sys/firmware/acpi/platform_profile", profile)
        .context("this probably means that your system does not support changing ACPI profiles")
}

/// Get the list of available platform profiles.
pub fn get_platform_profiles() -> Vec<String> {
    let path = "/sys/firmware/acpi/platform_profile_choices";

    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };

    content
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

/// Path for storing the governor override state.
const GOVERNOR_OVERRIDE_PATH: &str = "/etc/xdg/superfreq/governor_override";

#[derive(Display, Debug, Clone, Copy, clap::ValueEnum)]
pub enum GovernorOverride {
    #[display("performance")]
    Performance,
    #[display("powersave")]
    Powersave,
    #[display("reset")]
    Reset,
}

pub fn set_governor_override(mode: GovernorOverride) -> anyhow::Result<()> {
    let parent = Path::new(GOVERNOR_OVERRIDE_PATH).parent().unwrap();
    if !parent.exists() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create directory '{path}'",
                path = parent.display(),
            )
        })?;
    }

    match mode {
        GovernorOverride::Reset => {
            // Remove the override file if it exists
            let result = fs::remove_file(GOVERNOR_OVERRIDE_PATH);

            if let Err(error) = result {
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to delete governor override file '{GOVERNOR_OVERRIDE_PATH}'"
                        )
                    });
                }
            }

            log::info!(
                "governor override has been deleted. normal profile-based settings will be used"
            );
        }

        GovernorOverride::Performance | GovernorOverride::Powersave => {
            let governor = mode.to_string();

            write(GOVERNOR_OVERRIDE_PATH, &governor)
                .context("failed to write governor override")?;

            // TODO: Apply the setting too.

            log::info!(
                "governor override set to '{governor}'. this setting will persist across reboots"
            );
            log::info!("to reset, run: superfreq set --governor-persist reset");
        }
    }

    Ok(())
}

/// Get the current governor override if set.
pub fn get_governor_override() -> anyhow::Result<Option<String>> {
    match fs::read_to_string(GOVERNOR_OVERRIDE_PATH) {
        Ok(governor_override) => Ok(Some(governor_override)),

        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),

        Err(error) => Err(error).with_context(|| {
            format!("failed to read governor override at '{GOVERNOR_OVERRIDE_PATH}'")
        }),
    }
}
