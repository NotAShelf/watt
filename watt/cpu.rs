use std::{
  cell::OnceCell,
  collections::HashMap,
  fmt,
  fs::OpenOptions,
  hash,
  io::Write,
  mem,
  string::ToString,
  sync::Arc,
};

use anyhow::{
  Context,
  anyhow,
  bail,
};
use yansi::Paint as _;

use crate::fs;

pub mod frequency;

use frequency::Frequency;

#[derive(Default, Debug, Clone, PartialEq)]
struct CpuScanCache {
  stat: OnceCell<HashMap<u32, CpuStat>>,
  info: OnceCell<HashMap<u32, Arc<HashMap<String, String>>>>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
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

  /// Calculates usage based on delta between this stat and a previous stat.
  /// This gives the current CPU usage percentage over the time interval.
  pub fn usage_delta(&self, previous: &CpuStat) -> f64 {
    let total_delta = self.total().saturating_sub(previous.total()) as f64;
    let idle_delta = self.idle().saturating_sub(previous.idle()) as f64;

    if total_delta == 0.0 {
      return 0.0;
    }

    1.0 - idle_delta / total_delta
  }
}

#[derive(Default, Debug, Clone)]
pub struct Cpu {
  pub number: u32,

  pub has_cpufreq:       bool,
  pub frequency_control: bool,

  pub available_governors: Vec<String>,
  pub governor:            Option<String>,

  pub frequency:         Option<Frequency>,
  pub frequency_minimum: Option<Frequency>,
  pub frequency_maximum: Option<Frequency>,

  pub available_epps: Vec<String>,
  pub epp:            Option<String>,

  pub available_epbs: Vec<String>,
  pub epb:            Option<String>,

  pub stat:          CpuStat,
  /// Previous stat reading for calculating current usage.
  pub previous_stat: Option<CpuStat>,
  pub info:          Option<Arc<HashMap<String, String>>>,
}

impl PartialEq for Cpu {
  fn eq(&self, other: &Self) -> bool {
    self.number == other.number
  }
}

impl Eq for Cpu {}

impl hash::Hash for Cpu {
  fn hash<H: hash::Hasher>(&self, state: &mut H) {
    self.number.hash(state);
  }
}

impl fmt::Display for Cpu {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let number = self.number.cyan();

    write!(f, "CPU {number}")
  }
}

fn write_verified(
  path: impl AsRef<std::path::Path>,
  value: &str,
  setting: &str,
) -> anyhow::Result<()> {
  let path = path.as_ref();
  if fs::read(path)?.is_some_and(|observed| observed == value) {
    fs::keep_setting(path, value);
    return Ok(());
  }

  fs::write(path, value)?;
  let observed = fs::read(path)?
    .with_context(|| format!("{setting} disappeared after it was written"))?;
  fs::observe(path, &observed);

  if observed != value {
    bail!("{setting} did not retain '{value}'; observed '{observed}'");
  }

  Ok(())
}

impl Cpu {
  /// Returns current CPU usage based on delta from previous reading.
  /// Returns 0.0 on first reading when no previous stat is available.
  pub fn current_usage(&self) -> f64 {
    match &self.previous_stat {
      Some(prev) => self.stat.usage_delta(prev),
      None => 0.0,
    }
  }
  /// Get all CPUs.
  pub fn all() -> anyhow::Result<Vec<Cpu>> {
    fn from_number(number: u32, cache: &CpuScanCache) -> anyhow::Result<Cpu> {
      let mut cpu = Cpu {
        number,
        ..Cpu::default()
      };
      cpu.scan(cache)?;

      Ok(cpu)
    }

    const PATH: &str = "/sys/devices/system/cpu";

    log::info!("detecting CPUs...");

    let mut cpus = vec![];
    let cache = CpuScanCache::default();

    log::debug!("scanning CPU entries in {PATH}");

    let entries = fs::read_dir(PATH).context("failed to read CPU entries")?;
    if let Some(entries) = entries {
      for entry in entries {
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

        cpus.push(from_number(number, &cache)?);
      }
    } else {
      // Fall back only when sysfs is unavailable. An empty CPU directory can
      // legitimately occur while every CPU is offline during hotplug.
      log::warn!("no CPUs found in sysfs, using logical CPU count fallback");
      for number in 0..num_cpus::get() as u32 {
        cpus.push(from_number(number, &cache)?);
      }
    }

    log::info!("detected {len} CPUs", len = cpus.len());

    Ok(cpus)
  }

  /// Scan CPU, tuning local copy of settings.
  fn scan(&mut self, cache: &CpuScanCache) -> anyhow::Result<()> {
    log::debug!("scanning CPU {number}", number = self.number);

    let Self { number, .. } = self;

    if !fs::exists(format!("/sys/devices/system/cpu/cpu{number}")) {
      bail!("{self} does not exist");
    }

    self.has_cpufreq =
      fs::exists(format!("/sys/devices/system/cpu/cpu{number}/cpufreq"));
    self.frequency_control = [
      "scaling_min_freq",
      "scaling_max_freq",
      "cpuinfo_min_freq",
      "cpuinfo_max_freq",
    ]
    .iter()
    .all(|name| {
      fs::exists(format!(
        "/sys/devices/system/cpu/cpu{number}/cpufreq/{name}"
      ))
    });

    log::trace!(
      "CPU {number} has cpufreq: {has_cpufreq}",
      number = self.number,
      has_cpufreq = self.has_cpufreq
    );

    if self.has_cpufreq {
      self.scan_governor()?;
      self.scan_frequency()?;
      self.scan_epp()?;
      self.scan_epb()?;
    }

    self.scan_stat(cache)?;
    self.scan_info(cache)?;

    Ok(())
  }

  fn scan_governor(&mut self) -> anyhow::Result<()> {
    log::trace!("scanning governor for CPU {number}", number = self.number);

    let Self { number, .. } = *self;

    self.governor = fs::read(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_governor"
    ))
    .with_context(|| format!("failed to read {self} scaling governor"))?;

    if self.governor.is_some() {
      self.available_governors = 'available_governors: {
        let Some(content) = fs::read(format!(
          "/sys/devices/system/cpu/cpu{number}/cpufreq/\
           scaling_available_governors"
        ))
        .with_context(|| {
          format!("failed to read {self} available governors")
        })?
        else {
          break 'available_governors Vec::new();
        };

        content
          .split_whitespace()
          .map(ToString::to_string)
          .collect()
      };
    }

    Ok(())
  }

  fn scan_epp(&mut self) -> anyhow::Result<()> {
    log::trace!("scanning EPP for CPU {number}", number = self.number);

    let Self { number, .. } = *self;

    self.epp = fs::read(format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/\
       energy_performance_preference"
    ))
    .with_context(|| format!("failed to read {self} EPP"))?;

    if self.epp.is_some() {
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
    }

    Ok(())
  }

  fn scan_epb(&mut self) -> anyhow::Result<()> {
    log::trace!("scanning EPB for CPU {number}", number = self.number);

    let Self { number, .. } = self;

    self.epb = fs::read(format!(
      "/sys/devices/system/cpu/cpu{number}/power/energy_perf_bias"
    ))
    .with_context(|| format!("failed to read {self} EPB"))?;

    if self.epb.is_some() {
      self.available_epbs = vec![
        "0".to_owned(),
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
        "normal".to_owned(),
        "balance-power".to_owned(),
        "power".to_owned(),
      ];
    }

    Ok(())
  }

  fn scan_stat(&mut self, cache: &CpuScanCache) -> anyhow::Result<()> {
    log::trace!("scanning stat for CPU {number}", number = self.number);

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
          .map_err(|_| anyhow!("failed to initialize CPU stat cache"))?;

        cache
          .stat
          .get()
          .context("CPU stat cache was not initialized")?
      },
    };

    // Store current stat as previous before updating to enable delta
    // calculation
    self.previous_stat = Some(self.stat.clone());

    self.stat = stat
      .get(&self.number)
      .with_context(|| format!("failed to get stat of {self}"))?
      .clone();

    Ok(())
  }

  fn scan_info(&mut self, cache: &CpuScanCache) -> anyhow::Result<()> {
    log::trace!("scanning info for CPU {number}", number = self.number);

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
              info.insert(number, Arc::new(mem::take(&mut current_data)));
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

        cache
          .info
          .set(info)
          .map_err(|_| anyhow!("failed to initialize CPU info cache"))?;
        cache
          .info
          .get()
          .context("CPU info cache was not initialized")?
      },
    };

    self.info = info.get(&self.number).cloned();

    Ok(())
  }

  pub fn set_governor(&mut self, governor: &str) -> anyhow::Result<()> {
    self.validate_governor(governor)?;
    let number = self.number;
    let path =
      format!("/sys/devices/system/cpu/cpu{number}/cpufreq/scaling_governor");

    write_verified(&path, governor, "CPU governor").with_context(|| {
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing governors"
      )
    })?;

    self.governor = Some(governor.to_owned());

    log::info!(
      "CPU {number} governor set to {governor}",
      number = self.number
    );

    Ok(())
  }

  fn validate_governor(&self, governor: &str) -> anyhow::Result<()> {
    if !self
      .available_governors
      .iter()
      .any(|value| value == governor)
    {
      bail!(
        "governor '{governor}' is not available for {self}. available \
         governors: {governors}",
        governors = self.available_governors.join(", "),
      );
    }

    Ok(())
  }

  pub fn set_epp(&mut self, epp: &str) -> anyhow::Result<()> {
    self.validate_epp(epp)?;
    let number = self.number;
    let path = format!(
      "/sys/devices/system/cpu/cpu{number}/cpufreq/\
       energy_performance_preference"
    );

    write_verified(&path, epp, "CPU EPP").with_context(|| {
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing EPP"
      )
    })?;

    self.epp = Some(epp.to_owned());

    log::info!("CPU {number} EPP set to {epp}", number = self.number);

    Ok(())
  }

  fn validate_epp(&self, epp: &str) -> anyhow::Result<()> {
    if !self.available_epps.iter().any(|value| value == epp) {
      bail!(
        "EPP value '{epp}' is not available for {self}. available EPP values: \
         {epps}",
        epps = self.available_epps.join(", "),
      );
    }

    Ok(())
  }

  pub fn set_epb(&mut self, epb: &str) -> anyhow::Result<()> {
    self.validate_epb(epb)?;
    let number = self.number;
    let path =
      format!("/sys/devices/system/cpu/cpu{number}/power/energy_perf_bias");

    write_verified(&path, epb, "CPU EPB").with_context(|| {
      format!(
        "this probably means that {self} doesn't exist or doesn't support \
         changing EPB"
      )
    })?;

    self.epb = Some(epb.to_owned());

    log::info!("CPU {number} EPB set to {epb}", number = self.number);

    Ok(())
  }

  fn validate_epb(&self, epb: &str) -> anyhow::Result<()> {
    if !self.available_epbs.iter().any(|value| value == epb) {
      bail!(
        "EPB value '{epb}' is not available for {self}. available EPB values: \
         {valid}",
        valid = self.available_epbs.join(", "),
      );
    }

    Ok(())
  }

  pub fn set_pm_qos_resume_latency_us(
    &self,
    latency: &str,
  ) -> anyhow::Result<()> {
    self.validate_pm_qos_resume_latency()?;
    let Self { number, .. } = *self;
    let path = format!(
      "/sys/devices/system/cpu/cpu{number}/power/pm_qos_resume_latency_us"
    );

    write_verified(&path, latency, "CPU PM QoS resume latency").with_context(
      || {
        format!(
          "this probably means that {self} doesn't exist or doesn't support \
           changing PM QoS resume latency"
        )
      },
    )?;

    log::info!(
      "CPU {number} PM QoS resume latency set to {latency} us",
      number = self.number,
    );

    Ok(())
  }

  fn validate_pm_qos_resume_latency(&self) -> anyhow::Result<()> {
    let number = self.number;
    if !fs::exists(format!(
      "/sys/devices/system/cpu/cpu{number}/power/pm_qos_resume_latency_us"
    )) {
      bail!("PM QoS resume latency is not available for {self}");
    }

    Ok(())
  }

  pub fn set_pstate_min_performance_percent(percent: u8) -> anyhow::Result<()> {
    write_verified(
      "/sys/devices/system/cpu/intel_pstate/min_perf_pct",
      &percent.to_string(),
      "Intel P-State minimum performance",
    )
    .context("failed to set Intel P-State minimum performance percent")?;

    log::info!("Intel P-State minimum performance set to {percent}%");

    Ok(())
  }

  pub fn set_pstate_max_performance_percent(percent: u8) -> anyhow::Result<()> {
    write_verified(
      "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
      &percent.to_string(),
      "Intel P-State maximum performance",
    )
    .context("failed to set Intel P-State maximum performance percent")?;

    log::info!("Intel P-State maximum performance set to {percent}%");

    Ok(())
  }

  pub fn set_turbo<'a>(
    on: bool,
    mut cpus: impl Iterator<Item = &'a Self>,
  ) -> anyhow::Result<()> {
    log::info!("setting CPU turbo boost to {on}");

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
    if write_verified(
      intel_boost_path_negated,
      value_boost_negated,
      "Intel P-State turbo",
    )
    .is_ok()
    {
      return Ok(());
    }
    if write_verified(amd_boost_path, value_boost, "AMD P-State turbo").is_ok()
    {
      return Ok(());
    }
    if write_verified(msr_boost_path, value_boost, "AMD turbo").is_ok() {
      return Ok(());
    }
    if write_verified(generic_boost_path, value_boost, "generic turbo").is_ok()
    {
      return Ok(());
    }

    // Also try per-core cpufreq boost for some AMD systems.
    if cpus.any(|cpu| {
      let Cpu { number, .. } = cpu;

      write_verified(
        format!("/sys/devices/system/cpu/cpu{number}/cpufreq/boost"),
        value_boost,
        "CPU turbo",
      )
      .is_ok()
    }) {
      return Ok(());
    }

    bail!("no supported CPU boost control mechanism found");
  }

  pub fn is_intel_pstate() -> bool {
    fs::exists("/sys/devices/system/cpu/intel_pstate")
  }

  pub fn turbo() -> anyhow::Result<Option<bool>> {
    log::trace!("reading turbo boost status");

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

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub governor:                      Option<String>,
  pub energy_performance_preference: Option<String>,
  pub energy_perf_bias:              Option<String>,
  pub frequency_minimum:             Option<Frequency>,
  pub frequency_maximum:             Option<Frequency>,
  pub pm_qos_resume_latency_us:      Option<String>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.governor.is_some()
      && self.energy_performance_preference.is_some()
      && self.energy_perf_bias.is_some()
      && self.frequency_minimum.is_some()
      && self.frequency_maximum.is_some()
      && self.pm_qos_resume_latency_us.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      governor:                      self
        .governor
        .or_else(|| that.governor.clone()),
      energy_performance_preference: self
        .energy_performance_preference
        .or_else(|| that.energy_performance_preference.clone()),
      energy_perf_bias:              self
        .energy_perf_bias
        .or_else(|| that.energy_perf_bias.clone()),
      frequency_minimum:             self
        .frequency_minimum
        .or(that.frequency_minimum),
      frequency_maximum:             self
        .frequency_maximum
        .or(that.frequency_maximum),
      pm_qos_resume_latency_us:      self
        .pm_qos_resume_latency_us
        .or_else(|| that.pm_qos_resume_latency_us.clone()),
    }
  }

  pub fn validate(&self, cpu: &Cpu) -> anyhow::Result<()> {
    if let Some(governor) = &self.governor {
      cpu.validate_governor(governor)?;
    }
    if let Some(epp) = &self.energy_performance_preference {
      cpu.validate_epp(epp)?;
    }
    if let Some(epb) = &self.energy_perf_bias {
      cpu.validate_epb(epb)?;
    }
    if self.pm_qos_resume_latency_us.is_some() {
      cpu.validate_pm_qos_resume_latency()?;
    }

    cpu
      .frequency_transition(self.frequency_minimum, self.frequency_maximum)
      .map(|_| ())
  }

  pub fn apply(&self, cpu: &mut Cpu) -> anyhow::Result<()> {
    let frequency = cpu
      .frequency_transition(self.frequency_minimum, self.frequency_maximum)?;

    if let Some(governor) = &self.governor {
      cpu.set_governor(governor)?;
    }

    if let Some(epp) = &self.energy_performance_preference {
      cpu.set_epp(epp)?;
    }

    if let Some(epb) = &self.energy_perf_bias {
      cpu.set_epb(epb)?;
    }

    if let Some(frequency) = frequency {
      cpu.apply_frequency_transition(frequency)?;
    }

    if let Some(latency) = &self.pm_qos_resume_latency_us {
      cpu.set_pm_qos_resume_latency_us(latency)?;
    }

    Ok(())
  }
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct GlobalDelta {
  pub turbo:                          Option<bool>,
  pub pstate_min_performance_percent: Option<u8>,
  pub pstate_max_performance_percent: Option<u8>,
  pub dma_latency_us:                 Option<i32>,
}

impl GlobalDelta {
  pub fn is_some(&self) -> bool {
    self.turbo.is_some()
      && self.pstate_min_performance_percent.is_some()
      && self.pstate_max_performance_percent.is_some()
      && self.dma_latency_us.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      turbo:                          self.turbo.or(that.turbo),
      pstate_min_performance_percent: self
        .pstate_min_performance_percent
        .or(that.pstate_min_performance_percent),
      pstate_max_performance_percent: self
        .pstate_max_performance_percent
        .or(that.pstate_max_performance_percent),
      dma_latency_us:                 self
        .dma_latency_us
        .or(that.dma_latency_us),
    }
  }

  pub fn apply<'a>(
    &self,
    cpus: impl Iterator<Item = &'a Cpu>,
    dma_latency: &mut DmaLatency,
  ) -> anyhow::Result<()> {
    if let Some(percent) = self.pstate_min_performance_percent {
      Cpu::set_pstate_min_performance_percent(percent)?;
    }

    if let Some(percent) = self.pstate_max_performance_percent {
      Cpu::set_pstate_max_performance_percent(percent)?;
    }

    if let Some(turbo) = self.turbo {
      Cpu::set_turbo(turbo, cpus)?;
    }

    dma_latency.apply(self.dma_latency_us)?;

    Ok(())
  }
}

#[derive(Default, Debug)]
pub struct DmaLatency {
  current: Option<i32>,
  file:    Option<std::fs::File>,
}

impl DmaLatency {
  pub fn apply(&mut self, latency_us: Option<i32>) -> anyhow::Result<()> {
    if self.current == latency_us {
      return Ok(());
    }

    if let Some(latency_us) = latency_us {
      let mut file = OpenOptions::new()
        .write(true)
        .open(fs::path("/dev/cpu_dma_latency"))
        .context("failed to open /dev/cpu_dma_latency")?;

      file
        .write_all(&latency_us.to_ne_bytes())
        .context("failed to write CPU DMA latency request")?;

      self.file = Some(file);
      self.current = Some(latency_us);
      log::info!("CPU DMA latency request set to {latency_us} us");
    } else {
      self.file = None;
      self.current = None;
      log::info!("CPU DMA latency request released");
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::{
    env,
    fs,
    process,
  };

  use super::*;

  #[test]
  fn scans_and_applies_cpu_controls_in_a_virtual_system_tree() {
    let root =
      env::temp_dir().join(format!("watt-cpu-fixture-{}", process::id(),));
    let cpufreq = root.join("sys/devices/system/cpu/cpu0/cpufreq");
    fs::create_dir_all(&cpufreq).unwrap();
    fs::create_dir_all(root.join("proc")).unwrap();

    for (name, value) in [
      ("scaling_governor", "schedutil"),
      ("scaling_available_governors", "schedutil powersave"),
      ("cpuinfo_cur_freq", "1500000"),
      ("cpuinfo_min_freq", "800000"),
      ("cpuinfo_max_freq", "3000000"),
      ("scaling_min_freq", "800000"),
      ("scaling_max_freq", "3000000"),
    ] {
      fs::write(cpufreq.join(name), value).unwrap();
    }
    fs::write(
      root.join("proc/stat"),
      "cpu 1 0 1 8 0 0 0 0\ncpu0 1 0 1 8 0 0 0 0\n",
    )
    .unwrap();
    fs::write(root.join("proc/cpuinfo"), "processor : 0\n\n").unwrap();

    let _root = crate::fs::set_system_root_for_tests(&root);
    let mut cpu = Cpu::all().unwrap().pop().unwrap();
    Delta {
      governor: Some("powersave".to_owned()),
      frequency_minimum: Some(Frequency::from_khz(1_000_000).unwrap()),
      ..Delta::default()
    }
    .apply(&mut cpu)
    .unwrap();

    assert_eq!(
      fs::read_to_string(cpufreq.join("scaling_governor")).unwrap(),
      "powersave"
    );
    assert_eq!(
      fs::read_to_string(cpufreq.join("scaling_min_freq")).unwrap(),
      "1000000"
    );

    drop(_root);
    fs::remove_dir_all(root).unwrap();
  }
}
