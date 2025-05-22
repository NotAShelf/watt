use anyhow::{Context, bail};
use yansi::Paint as _;

use std::{fmt, string::ToString};

use crate::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpu {
    pub number: u32,

    pub has_cpufreq: bool,

    pub available_governors: Vec<String>,
    pub governor: Option<String>,

    pub frequency_mhz: Option<u64>,
    pub frequency_mhz_minimum: Option<u64>,
    pub frequency_mhz_maximum: Option<u64>,

    pub available_epps: Vec<String>,
    pub epp: Option<String>,

    pub available_epbs: Vec<String>,
    pub epb: Option<String>,

    pub time_user: u64,
    pub time_nice: u64,
    pub time_system: u64,
    pub time_idle: u64,
    pub time_iowait: u64,
    pub time_irq: u64,
    pub time_softirq: u64,
    pub time_steal: u64,
}

impl Cpu {
    pub fn time_total(&self) -> u64 {
        self.time_user
            + self.time_nice
            + self.time_system
            + self.time_idle
            + self.time_iowait
            + self.time_irq
            + self.time_softirq
            + self.time_steal
    }

    pub fn time_idle(&self) -> u64 {
        self.time_idle + self.time_iowait
    }
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let number = self.number.cyan();

        write!(f, "CPU {number}")
    }
}

impl Cpu {
    pub fn new(number: u32) -> anyhow::Result<Self> {
        let mut cpu = Self {
            number,
            has_cpufreq: false,

            available_governors: Vec::new(),
            governor: None,

            frequency_mhz: None,
            frequency_mhz_minimum: None,
            frequency_mhz_maximum: None,

            available_epps: Vec::new(),
            epp: None,

            available_epbs: Vec::new(),
            epb: None,

            time_user: 0,
            time_nice: 0,
            time_system: 0,
            time_idle: 0,
            time_iowait: 0,
            time_irq: 0,
            time_softirq: 0,
            time_steal: 0,
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
        let Self { number, .. } = self;

        if !fs::exists(format!("/sys/devices/system/cpu/cpu{number}")) {
            bail!("{self} does not exist");
        }

        self.has_cpufreq = fs::exists(format!("/sys/devices/system/cpu/cpu{number}/cpufreq"));

        self.rescan_times()?;

        if self.has_cpufreq {
            self.rescan_governor()?;
            self.rescan_frequency()?;
            self.rescan_epp()?;
            self.rescan_epb()?;
        }

        Ok(())
    }

    fn rescan_times(&mut self) -> anyhow::Result<()> {
        // TODO: Don't read this per CPU. Share the read or
        // find something in /sys/.../cpu{N} that does it.
        let content = fs::read("/proc/stat")
            .context("/proc/stat does not exist")?
            .context("failed to read CPU stat")?;

        let cpu_name = format!("cpu{number}", number = self.number);

        let mut stats = content
            .lines()
            .find_map(|line| {
                line.starts_with(&cpu_name)
                    .then(|| line.split_whitespace().skip(1))
            })
            .with_context(|| format!("failed to find {self} in CPU stats"))?;

        self.time_user = stats
            .next()
            .with_context(|| format!("failed to find {self} user time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} user time"))?;
        self.time_nice = stats
            .next()
            .with_context(|| format!("failed to find {self} nice time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} nice time"))?;
        self.time_system = stats
            .next()
            .with_context(|| format!("failed to find {self} system time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} system time"))?;
        self.time_idle = stats
            .next()
            .with_context(|| format!("failed to find {self} idle time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} idle time"))?;
        self.time_iowait = stats
            .next()
            .with_context(|| format!("failed to find {self} iowait time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} iowait time"))?;
        self.time_irq = stats
            .next()
            .with_context(|| format!("failed to find {self} irq time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} irq time"))?;
        self.time_softirq = stats
            .next()
            .with_context(|| format!("failed to find {self} softirq time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} softirq time"))?;
        self.time_steal = stats
            .next()
            .with_context(|| format!("failed to find {self} steal time"))?
            .parse()
            .with_context(|| format!("failed to parse {self} steal time"))?;

        Ok(())
    }

    fn rescan_governor(&mut self) -> anyhow::Result<()> {
        let Self { number, .. } = *self;

        self.available_governors = 'available_governors: {
            let Some(Ok(content)) = fs::read(format!(
                "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_available_governors"
            )) else {
                break 'available_governors Vec::new();
            };

            content
                .split_whitespace()
                .map(ToString::to_string)
                .collect()
        };

        self.governor = Some(
            fs::read(format!(
                "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_governor"
            ))
            .with_context(|| format!("failed to find {self} scaling governor"))?
            .with_context(|| format!("failed to read {self} scaling governor"))?,
        );

        Ok(())
    }

    fn rescan_frequency(&mut self) -> anyhow::Result<()> {
        let Self { number, .. } = *self;

        let frequency_khz = fs::read_u64(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_cur_freq"
        ))
        .with_context(|| format!("failed to find {self} frequency"))?
        .with_context(|| format!("failed to parse {self} frequency"))?;
        let frequency_khz_minimum = fs::read_u64(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"
        ))
        .with_context(|| format!("failed to find {self} frequency minimum"))?
        .with_context(|| format!("failed to parse {self} frequency"))?;
        let frequency_khz_maximum = fs::read_u64(format!(
            "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_max_freq"
        ))
        .with_context(|| format!("failed to find {self} frequency maximum"))?
        .with_context(|| format!("failed to parse {self} frequency"))?;

        self.frequency_mhz = Some(frequency_khz / 1000);
        self.frequency_mhz_minimum = Some(frequency_khz_minimum / 1000);
        self.frequency_mhz_maximum = Some(frequency_khz_maximum / 1000);

        Ok(())
    }

    fn rescan_epp(&mut self) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        self.available_epps = 'available_epps: {
            let Some(Ok(content)) = fs::read(format!(
                "/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_available_preferences"
            )) else {
                break 'available_epps Vec::new();
            };

            content
                .split_whitespace()
                .map(ToString::to_string)
                .collect()
        };

        self.epp = Some(
            fs::read(format!(
                "/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_preference"
            ))
            .with_context(|| format!("failed to find {self} EPP"))?
            .with_context(|| format!("failed to read {self} EPP"))?,
        );

        Ok(())
    }

    fn rescan_epb(&mut self) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        self.available_epbs = if self.has_cpufreq {
            vec![
                "1".to_owned(),
                "2".to_owned(),
                "3".to_owned(),
                "4".to_owned(),
                "5".to_owned(),
                "6".to_owned(),
                "7".to_owned(),
                "8".to_owned(),
                "9".to_owned(),
                "10".to_owned(),
                "11".to_owned(),
                "12".to_owned(),
                "13".to_owned(),
                "14".to_owned(),
                "15".to_owned(),
                "performance".to_owned(),
                "balance-performance".to_owned(),
                "balance_performance".to_owned(), // Alternative form with underscore.
                "balance-power".to_owned(),
                "balance_power".to_owned(), // Alternative form with underscore.
                "power".to_owned(),
            ]
        } else {
            Vec::new()
        };

        self.epb = Some(
            fs::read(format!(
                "/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_bias"
            ))
            .with_context(|| format!("failed to find {self} EPB"))?
            .with_context(|| format!("failed to read {self} EPB"))?,
        );

        Ok(())
    }

    pub fn set_governor(&mut self, governor: &str) -> anyhow::Result<()> {
        let Self {
            number,
            available_governors: ref governors,
            ..
        } = *self;

        if !governors
            .iter()
            .any(|avail_governor| avail_governor == governor)
        {
            bail!(
                "governor '{governor}' is not available for {self}. available governors: {governors}",
                governors = governors.join(", "),
            );
        }

        fs::write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_governor"),
            governor,
        )
        .with_context(|| {
            format!(
                "this probably means that {self} doesn't exist or doesn't support changing governors"
            )
        })?;

        self.governor = Some(governor.to_owned());

        Ok(())
    }

    pub fn set_epp(&mut self, epp: &str) -> anyhow::Result<()> {
        let Self {
            number,
            available_epps: ref epps,
            ..
        } = *self;

        if !epps.iter().any(|avail_epp| avail_epp == epp) {
            bail!(
                "EPP value '{epp}' is not availabile for {self}. available EPP values: {epps}",
                epps = epps.join(", "),
            );
        }

        fs::write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_preference"),
            epp,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing EPP")
        })?;

        self.epp = Some(epp.to_owned());

        Ok(())
    }

    pub fn set_epb(&mut self, epb: &str) -> anyhow::Result<()> {
        let Self {
            number,
            available_epbs: ref epbs,
            ..
        } = *self;

        if !epbs.iter().any(|avail_epb| avail_epb == epb) {
            bail!(
                "EPB value '{epb}' is not available for {self}. available EPB values: {valid}",
                valid = epbs.join(", "),
            );
        }

        fs::write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_bias"),
            epb,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing EPB")
        })?;

        self.epb = Some(epb.to_owned());

        Ok(())
    }

    pub fn set_frequency_mhz_minimum(&mut self, frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = *self;

        self.validate_frequency_mhz_minimum(frequency_mhz)?;

        // We use u64 for the intermediate calculation to prevent overflow
        let frequency_khz = frequency_mhz * 1000;
        let frequency_khz = frequency_khz.to_string();

        fs::write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"),
            &frequency_khz,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing minimum frequency")
        })?;

        self.frequency_mhz_minimum = Some(frequency_mhz);

        Ok(())
    }

    fn validate_frequency_mhz_minimum(&self, new_frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let Some(Ok(minimum_frequency_khz)) = fs::read_u64(format!(
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

    pub fn set_frequency_mhz_maximum(&mut self, frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = *self;

        self.validate_frequency_mhz_maximum(frequency_mhz)?;

        // We use u64 for the intermediate calculation to prevent overflow
        let frequency_khz = frequency_mhz * 1000;
        let frequency_khz = frequency_khz.to_string();

        fs::write(
            format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_max_freq"),
            &frequency_khz,
        )
        .with_context(|| {
            format!("this probably means that {self} doesn't exist or doesn't support changing maximum frequency")
        })?;

        self.frequency_mhz_maximum = Some(frequency_mhz);

        Ok(())
    }

    fn validate_frequency_mhz_maximum(&self, new_frequency_mhz: u64) -> anyhow::Result<()> {
        let Self { number, .. } = self;

        let Some(Ok(maximum_frequency_khz)) = fs::read_u64(format!(
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
        if fs::write(intel_boost_path_negated, value_boost_negated).is_ok() {
            return Ok(());
        }
        if fs::write(amd_boost_path, value_boost).is_ok() {
            return Ok(());
        }
        if fs::write(msr_boost_path, value_boost).is_ok() {
            return Ok(());
        }
        if fs::write(generic_boost_path, value_boost).is_ok() {
            return Ok(());
        }

        // Also try per-core cpufreq boost for some AMD systems.
        if Self::all()?.iter().any(|cpu| {
            let Cpu { number, .. } = cpu;

            fs::write(
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
