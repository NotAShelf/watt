use anyhow::{Context, bail};

use std::{fmt, fs, path::Path, string::ToString};

fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();

    path.exists()
}

// Not doing any anyhow stuff here as all the calls of this ignore errors.
fn read_u64(path: impl AsRef<Path>) -> anyhow::Result<u64> {
    let path = path.as_ref();

    let content = fs::read_to_string(path)?;

    Ok(content.trim().parse()?)
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

#[derive(Debug, Clone, Copy)]
pub struct Cpu {
    pub number: u32,
    pub has_cpufreq: bool,
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { number, .. } = self;
        write!(f, "CPU {number}")
    }
}

impl Cpu {
    pub fn new(number: u32) -> anyhow::Result<Self> {
        let mut cpu = Self {
            number,
            has_cpufreq: false,
        };
        cpu.rescan()?;

        Ok(cpu)
    }

    /// Get all CPUs.
    pub fn all() -> anyhow::Result<Vec<Cpu>> {
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
            let Ok(number) = cpu_prefix_removed.parse() else {
                continue;
            };

            cpus.push(Self::new(number)?);
        }

        // Fall back if sysfs iteration above fails to find any cpufreq CPUs.
        if cpus.is_empty() {
            for number in 0..num_cpus::get() as u32 {
                cpus.push(Self::new(number)?);
            }
        }

        Ok(cpus)
    }

    /// Rescan CPU, tuning local copy of settings.
    pub fn rescan(&mut self) -> anyhow::Result<()> {
        let has_cpufreq = exists(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq",
            number = self.number,
        ));

        self.has_cpufreq = has_cpufreq;

        Ok(())
    }

    pub fn get_available_governors(&self) -> Vec<String> {
        let Self { number, .. } = self;

        let Ok(content) = fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_available_governors"
        )) else {
            return Vec::new();
        };

        content
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    }

    pub fn set_governor(&self, governor: &str) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let governors = self.get_available_governors();

        if !governors
            .iter()
            .any(|avail_governor| avail_governor == governor)
        {
            bail!(
                "governor '{governor}' is not available for {self}. available governors: {governors}",
                governors = governors.join(", "),
            );
        }

        write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_governor"),
            governor,
        )
        .with_context(|| {
            format!(
                "this probably means that {self} doesn't exist or doesn't support changing governors"
            )
        })
    }

    pub fn get_available_epps(&self) -> Vec<String> {
        let Self { number, .. } = self;

        let Ok(content) = fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_available_preferences"
        )) else {
            return Vec::new();
        };

        content
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    }

    pub fn set_epp(&self, epp: &str) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let epps = self.get_available_epps();

        if !epps.iter().any(|avail_epp| avail_epp == epp) {
            bail!(
                "EPP value '{epp}' is not availabile for {self}. available EPP values: {epps}",
                epps = epps.join(", "),
            );
        }

        write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_preference"),
            epp,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing EPP")
        })
    }

    pub fn get_available_epbs(&self) -> &'static [&'static str] {
        if !self.has_cpufreq {
            return &[];
        }

        &[
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "10",
            "11",
            "12",
            "13",
            "14",
            "15",
            "performance",
            "balance-performance",
            "balance_performance", // Alternative form with underscore.
            "balance-power",
            "balance_power", // Alternative form with underscore.
            "power",
        ]
    }

    pub fn set_epb(&self, epb: &str) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let epbs = self.get_available_epbs();

        if !epbs.contains(&epb) {
            bail!(
                "EPB value '{epb}' is not available for {self}. available EPB values: {valid}",
                valid = epbs.join(", "),
            );
        }

        write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_bias"),
            epb,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing EPB")
        })
    }

    pub fn set_frequency_minimum(&self, frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        self.validate_frequency_minimum(frequency_mhz)?;

        // We use u64 for the intermediate calculation to prevent overflow
        let frequency_khz = frequency_mhz * 1000;
        let frequency_khz = frequency_khz.to_string();

        write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"),
            &frequency_khz,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing minimum frequency")
        })
    }

    fn validate_frequency_minimum(&self, new_frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let Ok(minimum_frequency_khz) = read_u64(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"
        )) else {
            // Just let it pass if we can't find anything.
            return Ok(());
        };

        if new_frequency_mhz * 1000 < minimum_frequency_khz {
            bail!(
                "new minimum frequency ({new_frequency_mhz} MHz) cannot be lower than the minimum frequency ({} MHz) for {self}",
                minimum_frequency_khz / 1000,
            );
        }

        Ok(())
    }

    pub fn set_frequency_maximum(&self, frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        self.validate_frequency_maximum(frequency_mhz)?;

        // We use u64 for the intermediate calculation to prevent overflow
        let frequency_khz = frequency_mhz * 1000;
        let frequency_khz = frequency_khz.to_string();

        write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_max_freq"),
            &frequency_khz,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing maximum frequency")
        })
    }

    fn validate_frequency_maximum(&self, new_frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let Ok(maximum_frequency_khz) = read_u64(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"
        )) else {
            // Just let it pass if we can't find anything.
            return Ok(());
        };

        if new_frequency_mhz * 1000 > maximum_frequency_khz {
            bail!(
                "new maximum frequency ({new_frequency_mhz} MHz) cannot be higher than the maximum frequency ({} MHz) for {self}",
                maximum_frequency_khz / 1000,
            );
        }

        Ok(())
    }

    pub fn set_turbo(on: bool) -> anyhow::Result<()> {
        let value_boost = match on {
            true => "1",  // boost = 1 means turbo is enabled.
            false => "0", // boost = 0 means turbo is disabled.
        };

        let value_boost_negated = match on {
            true => "0",  // no_turbo = 0 means turbo is enabled.
            false => "1", // no_turbo = 1 means turbo is disabled.
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
        if Self::all()?.iter().any(|cpu| {
            let Cpu { number, .. } = cpu;

            write(
                format!("/sys/devices/system/cpu/cpu{number}/cpufreq/boost"),
                value_boost,
            )
            .is_ok()
        }) {
            return Ok(());
        }

        bail!("no supported CPU boost control mechanism found");
    }
}
