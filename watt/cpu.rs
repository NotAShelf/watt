use std::{
  cell::OnceCell,
  collections::HashMap,
  fmt,
  mem,
  rc::Rc,
  string::ToString,
};

use anyhow::{
  Context,
  bail,
};
use yansi::Paint as _;

use crate::fs;

#[derive(Default, Debug, Clone, PartialEq)]
pub struct CpuRescanCache {
  stat: OnceCell<HashMap<u32, CpuStat>>,
  info: OnceCell<HashMap<u32, Rc<HashMap<String, String>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuStat {
  pub user:    u64,
  pub nice:    u64,
  pub system:  u64,
  pub idle:    u64,
  pub iowait:  u64,
  pub irq:     u64,
  pub softirq: u64,
  pub steal:   u64,
}

impl CpuStat {
  pub fn total(&self) -> u64 {
    self.user
      + self.nice
      + self.system
      + self.idle
      + self.iowait
      + self.irq
      + self.softirq
      + self.steal
  }

  pub fn idle(&self) -> u64 {
    self.idle + self.iowait
  }

  pub fn usage(&self) -> f64 {
    1.0 - self.idle() as f64 / self.total() as f64
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cpu {
  pub number: u32,

  pub has_cpufreq: bool,

  pub available_governors: Vec<String>,
  pub governor:            Option<String>,

  pub frequency_mhz:         Option<u64>,
  pub frequency_mhz_minimum: Option<u64>,
  pub frequency_mhz_maximum: Option<u64>,

  pub available_epps: Vec<String>,
  pub epp:            Option<String>,

  pub available_epbs: Vec<String>,
  pub epb:            Option<String>,

  pub stat: CpuStat,
  pub info: Option<Rc<HashMap<String, String>>>,

  pub temperature: Option<f64>,
}

impl fmt::Display for Cpu {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let number = self.number.cyan();

    write!(f, "CPU {number}")
  }
}

impl Cpu {
  pub fn new(number: u32, cache: &CpuRescanCache) -> anyhow::Result<Self> {
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

      stat: CpuStat {
        user:    0,
        nice:    0,
        system:  0,
        idle:    0,
        iowait:  0,
        irq:     0,
        softirq: 0,
        steal:   0,
      },
      info: None,

      temperature: None,
    };
    cpu.rescan(cache)?;

    Ok(cpu)
  }

  /// Get all CPUs.
  pub fn all() -> anyhow::Result<Vec<Cpu>> {
    const PATH: &str = "/sys/devices/system/cpu";

    let mut cpus = vec![];
    let cache = CpuRescanCache::default();

    for entry in fs::read_dir(PATH)
      .context("failed to read CPU entries")?
      .with_context(|| format!("'{PATH}' doesn't exist, are you on linux?"))?
    {
      let entry =
        entry.with_context(|| format!("failed to read entry of '{PATH}'"))?;

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

      cpus.push(Self::new(number, &cache)?);
    }

    // Fall back if sysfs iteration above fails to find any cpufreq CPUs.
    if cpus.is_empty() {
      for number in 0..num_cpus::get() as u32 {
        cpus.push(Self::new(number, &cache)?);
      }
    }

    Ok(cpus)
  }

  /// Rescan CPU, tuning local copy of settings.
  pub fn rescan(&mut self, cache: &CpuRescanCache) -> anyhow::Result<()> {
    let Self { number, .. } = self;

    if !fs::exists(format!("/sys/devices/system/cpu/cpu{number}")) {
      bail!("{self} does not exist");
    }

    self.has_cpufreq =
      fs::exists(format!("/sys/devices/system/cpu/cpu{number}/cpufreq"));

    if self.has_cpufreq {
      self.rescan_governor()?;
      self.rescan_frequency()?;
      self.rescan_epp()?;
      self.rescan_epb()?;
    }

    self.rescan_stat(cache)?;
    self.rescan_info(cache)?;

    Ok(())
  }

  fn rescan_governor(&mut self) -> anyhow::Result<()> {
    let Self { number, .. } = *self;

    self.available_governors = 'available_governors: {
      let Some(content) = fs::read(format!(
        "/sys/devices/system/cpu/cpu{number}/cpufreq/\
         scaling_available_governors"
      ))
      .with_context(|| format!("failed to read {self} available governors"))?
      else {
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
      .with_context(|| format!("failed to read {self} scaling governor"))?
      .with_context(|| format!("failed to find {self} scaling governor"))?,
    );

    Ok(())
  }

  fn rescan_frequency(&mut self) -> anyhow::Result<()> {
    let Self { number, .. } = *self;

    let frequency_khz = fs::read_n::<u64>(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_cur_freq"
    ))
    .with_context(|| format!("failed to parse {self} frequency"))?
    .with_context(|| format!("failed to find {self} frequency"))?;
    let frequency_khz_minimum = fs::read_n::<u64>(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"
    ))
    .with_context(|| format!("failed to parse {self} frequency minimum"))?
    .with_context(|| format!("failed to find {self} frequency"))?;
    let frequency_khz_maximum = fs::read_n::<u64>(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_max_freq"
    ))
    .with_context(|| format!("failed to parse {self} frequency maximum"))?
    .with_context(|| format!("failed to find {self} frequency"))?;

    self.frequency_mhz = Some(frequency_khz / 1000);
    self.frequency_mhz_minimum = Some(frequency_khz_minimum / 1000);
    self.frequency_mhz_maximum = Some(frequency_khz_maximum / 1000);

    Ok(())
  }

  fn rescan_epp(&mut self) -> anyhow::Result<()> {
    let Self { number, .. } = *self;

    self.available_epps = 'available_epps: {
      let Some(content) = fs::read(format!(
        "/sys/devices/system/cpu/cpu{number}/cpufreq/\
         energy_performance_available_preferences"
      ))
      .with_context(|| format!("failed to read {self} available EPPs"))?
      else {
        break 'available_epps Vec::new();
      };

      content
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
    };

    self.epp = Some(
      fs::read(format!(
        "/sys/devices/system/cpu/cpu{number}/cpufreq/\
         energy_performance_preference"
      ))
      .with_context(|| format!("failed to read {self} EPP"))?
      .with_context(|| format!("failed to find {self} EPP"))?,
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
      .with_context(|| format!("failed to read {self} EPB"))?
      .with_context(|| format!("failed to find {self} EPB"))?,
    );

    Ok(())
  }

  fn rescan_stat(&mut self, cache: &CpuRescanCache) -> anyhow::Result<()> {
    // OnceCell::get_or_try_init is unstable. Cope:
    let stat = match cache.stat.get() {
      Some(stat) => stat,

      None => {
        let content = fs::read("/proc/stat")
          .context("failed to read CPU stat")?
          .context("/proc/stat does not exist")?;

        cache
          .stat
          .set(HashMap::from_iter(content.lines().skip(1).filter_map(
            |line| {
              let mut parts = line.strip_prefix("cpu")?.split_whitespace();

              let number = parts.next()?.parse().ok()?;

              let stat = CpuStat {
                user:    parts.next()?.parse().ok()?,
                nice:    parts.next()?.parse().ok()?,
                system:  parts.next()?.parse().ok()?,
                idle:    parts.next()?.parse().ok()?,
                iowait:  parts.next()?.parse().ok()?,
                irq:     parts.next()?.parse().ok()?,
                softirq: parts.next()?.parse().ok()?,
                steal:   parts.next()?.parse().ok()?,
              };

              Some((number, stat))
            },
          )))
          .unwrap();

        cache.stat.get().unwrap()
      },
    };

    self.stat = stat
      .get(&self.number)
      .with_context(|| format!("failed to get stat of {self}"))?
      .clone();

    Ok(())
  }

  fn rescan_info(&mut self, cache: &CpuRescanCache) -> anyhow::Result<()> {
    // OnceCell::get_or_try_init is unstable. Cope:
    let info = match cache.info.get() {
      Some(stat) => stat,

      None => {
        let content = fs::read("/proc/cpuinfo")
          .context("failed to read CPU info")?
          .context("/proc/cpuinfo does not exist")?;

        let mut info = HashMap::new();
        let mut current_number = None;
        let mut current_data = HashMap::new();

        macro_rules! try_save_data {
          () => {
            if let Some(number) = current_number.take() {
              info.insert(number, Rc::new(mem::take(&mut current_data)));
            }
          };
        }

        for line in content.lines() {
          let parts = line.splitn(2, ':').collect::<Vec<_>>();

          if parts.len() == 2 {
            let key = parts[0].trim();
            let value = parts[1].trim();

            if key == "processor" {
              try_save_data!();

              current_number = value.parse::<u32>().ok();
            } else {
              current_data.insert(key.to_owned(), value.to_owned());
            }
          }
        }

        try_save_data!();

        cache.info.set(info).unwrap();
        cache.info.get().unwrap()
      },
    };

    self.info = info.get(&self.number).cloned();

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
        "governor '{governor}' is not available for {self}. available \
         governors: {governors}",
        governors = governors.join(", "),
      );
    }

    fs::write(
      format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_governor"),
      governor,
    )
    .with_context(|| {
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing governors"
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
        "EPP value '{epp}' is not available for {self}. available EPP values: \
         {epps}",
        epps = epps.join(", "),
      );
    }

    fs::write(
      format!(
        "/sys/devices/system/cpu/cpu{number}/cpufreq/\
         energy_performance_preference"
      ),
      epp,
    )
    .with_context(|| {
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing EPP"
      )
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
        "EPB value '{epb}' is not available for {self}. available EPB values: \
         {valid}",
        valid = epbs.join(", "),
      );
    }

    fs::write(
      format!(
        "/sys/devices/system/cpu/cpu{number}/cpufreq/energy_performance_bias"
      ),
      epb,
    )
    .with_context(|| {
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing EPB"
      )
    })?;

    self.epb = Some(epb.to_owned());

    Ok(())
  }

  pub fn set_frequency_mhz_minimum(
    &mut self,
    frequency_mhz: u64,
  ) -> anyhow::Result<()> {
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
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing minimum frequency"
      )
    })?;

    self.frequency_mhz_minimum = Some(frequency_mhz);

    Ok(())
  }

  fn validate_frequency_mhz_minimum(
    &self,
    new_frequency_mhz: u64,
  ) -> anyhow::Result<()> {
    let Self { number, .. } = self;

    let Some(minimum_frequency_khz) = fs::read_n::<u64>(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_min_freq"
    ))
    .with_context(|| format!("failed to read {self} minimum frequency"))?
    else {
      // Just let it pass if we can't find anything.
      return Ok(());
    };

    if new_frequency_mhz * 1000 < minimum_frequency_khz {
      bail!(
        "new minimum frequency ({new_frequency_mhz} MHz) cannot be lower than \
         the minimum frequency ({} MHz) for {self}",
        minimum_frequency_khz / 1000,
      );
    }

    Ok(())
  }

  pub fn set_frequency_mhz_maximum(
    &mut self,
    frequency_mhz: u64,
  ) -> anyhow::Result<()> {
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
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing maximum frequency"
      )
    })?;

    self.frequency_mhz_maximum = Some(frequency_mhz);

    Ok(())
  }

  fn validate_frequency_mhz_maximum(
    &self,
    new_frequency_mhz: u64,
  ) -> anyhow::Result<()> {
    let Self { number, .. } = self;

    let Some(maximum_frequency_khz) = fs::read_n::<u64>(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_max_freq"
    ))
    .with_context(|| format!("failed to read {self} maximum frequency"))?
    else {
      // Just let it pass if we can't find anything.
      return Ok(());
    };

    if new_frequency_mhz * 1000 > maximum_frequency_khz {
      bail!(
        "new maximum frequency ({new_frequency_mhz} MHz) cannot be higher \
         than the maximum frequency ({} MHz) for {self}",
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
    let msr_boost_path =
      "/sys/devices/system/cpu/cpufreq/amd_pstate_enable_boost";

    // Path priority (from most to least specific)
    let intel_boost_path_negated =
      "/sys/devices/system/cpu/intel_pstate/no_turbo";
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

  pub fn turbo() -> anyhow::Result<Option<bool>> {
    if let Some(content) =
      fs::read_n::<u64>("/sys/devices/system/cpu/intel_pstate/no_turbo")
        .context("failed to read CPU turbo boost status")?
    {
      return Ok(Some(content == 0));
    }

    if let Some(content) =
      fs::read_n::<u64>("/sys/devices/system/cpu/cpufreq/boost")
        .context("failed to read CPU turbo boost status")?
    {
      return Ok(Some(content == 1));
    }

    Ok(None)
  }
}
