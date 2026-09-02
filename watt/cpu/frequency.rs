use std::fmt;

use anyhow::{
  Context,
  bail,
};

use super::Cpu;
use crate::fs;

const KHZ_PER_MHZ: f64 = 1000.0;

/// A positive CPU frequency stored exactly in kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frequency(u64);

impl Frequency {
  /// Constructs a frequency from a positive kHz value.
  pub fn from_khz(khz: u64) -> anyhow::Result<Self> {
    if khz == 0 {
      bail!("frequency must be greater than zero");
    }

    Ok(Self(khz))
  }

  /// Constructs a frequency from MHz, rounded to the nearest kHz.
  pub fn from_mhz(mhz: f64) -> anyhow::Result<Self> {
    let khz = (mhz * KHZ_PER_MHZ).round();
    if !khz.is_finite() || khz < 1.0 || khz >= u64::MAX as f64 {
      bail!("invalid frequency: {mhz} MHz");
    }

    Ok(Self(khz as u64))
  }

  /// Returns the exact frequency in kHz.
  pub const fn as_khz(self) -> u64 {
    self.0
  }

  /// Returns the frequency represented in MHz.
  pub fn as_mhz(self) -> f64 {
    self.0 as f64 / KHZ_PER_MHZ
  }
}

impl fmt::Display for Frequency {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{} kHz", self.0)
  }
}

#[derive(Debug, Clone, Copy)]
struct Range {
  minimum: Frequency,
  maximum: Frequency,
}

impl Range {
  fn new(minimum: Frequency, maximum: Frequency) -> anyhow::Result<Self> {
    if minimum > maximum {
      bail!(
        "minimum frequency ({minimum}) cannot be higher than maximum \
         frequency ({maximum})"
      );
    }

    Ok(Self { minimum, maximum })
  }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Transition {
  minimum: Option<Frequency>,
  maximum: Option<Frequency>,
  order:   WriteOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteOrder {
  MinimumFirst,
  MaximumFirst,
}

impl Transition {
  fn new(
    hardware: Range,
    current: Range,
    minimum: Option<Frequency>,
    maximum: Option<Frequency>,
  ) -> anyhow::Result<Self> {
    let target = Range::new(
      minimum.unwrap_or(current.minimum),
      maximum.unwrap_or(current.maximum),
    )?;

    if target.minimum < hardware.minimum {
      bail!(
        "new software minimum frequency ({minimum}) cannot be lower than the \
         hardware minimum frequency ({hardware_minimum})",
        minimum = target.minimum,
        hardware_minimum = hardware.minimum,
      );
    }
    if target.maximum > hardware.maximum {
      bail!(
        "new software maximum frequency ({maximum}) cannot be higher than the \
         hardware maximum frequency ({hardware_maximum})",
        maximum = target.maximum,
        hardware_maximum = hardware.maximum,
      );
    }

    Ok(Self {
      minimum,
      maximum,
      order: if target.minimum > current.maximum {
        WriteOrder::MaximumFirst
      } else {
        WriteOrder::MinimumFirst
      },
    })
  }
}

impl Cpu {
  pub(super) fn scan_frequency(&mut self) -> anyhow::Result<()> {
    let number = self.number;
    let path =
      |name| format!("/sys/devices/system/cpu/cpu{number}/cpufreq/{name}");

    self.frequency = read(path("cpuinfo_cur_freq"))
      .with_context(|| format!("failed to parse {self} frequency"))?;
    if self.frequency.is_none() {
      self.frequency = read(path("scaling_cur_freq"))
        .with_context(|| format!("failed to parse {self} frequency"))?;
    }
    self.frequency_minimum = read(path("cpuinfo_min_freq"))
      .with_context(|| format!("failed to parse {self} frequency minimum"))?;
    self.frequency_maximum = read(path("cpuinfo_max_freq"))
      .with_context(|| format!("failed to parse {self} frequency maximum"))?;

    Ok(())
  }

  pub fn frequency_available(&self) -> bool {
    self.frequency_control
      && self.frequency_minimum.is_some()
      && self.frequency_maximum.is_some()
  }

  pub(super) fn frequency_transition(
    &self,
    minimum: Option<Frequency>,
    maximum: Option<Frequency>,
  ) -> anyhow::Result<Option<Transition>> {
    if minimum.is_none() && maximum.is_none() {
      return Ok(None);
    }

    let hardware = Range::new(
      self
        .frequency_minimum
        .context("hardware minimum frequency is unavailable")?,
      self
        .frequency_maximum
        .context("hardware maximum frequency is unavailable")?,
    )
    .with_context(|| format!("invalid hardware frequency range for {self}"))?;
    let current = Range::new(
      self.read_frequency("scaling_min_freq")?,
      self.read_frequency("scaling_max_freq")?,
    )
    .with_context(|| format!("invalid current frequency range for {self}"))?;

    Transition::new(hardware, current, minimum, maximum)
      .with_context(|| format!("invalid requested frequency range for {self}"))
      .map(Some)
  }

  pub(super) fn apply_frequency_transition(
    &self,
    transition: Transition,
  ) -> anyhow::Result<()> {
    let Transition {
      minimum,
      maximum,
      order,
    } = transition;

    let writes = match order {
      WriteOrder::MinimumFirst => {
        [("scaling_min_freq", minimum), ("scaling_max_freq", maximum)]
      },
      WriteOrder::MaximumFirst => {
        [("scaling_max_freq", maximum), ("scaling_min_freq", minimum)]
      },
    };
    for (name, frequency) in writes {
      if let Some(frequency) = frequency {
        self.write_frequency(name, frequency)?;
      }
    }

    Ok(())
  }

  fn read_frequency(&self, name: &str) -> anyhow::Result<Frequency> {
    let path =
      format!("/sys/devices/system/cpu/cpu{}/cpufreq/{name}", self.number);

    read(path)?.with_context(|| format!("{name} is unavailable for {self}"))
  }

  fn write_frequency(
    &self,
    name: &str,
    frequency: Frequency,
  ) -> anyhow::Result<()> {
    let path =
      format!("/sys/devices/system/cpu/cpu{}/cpufreq/{name}", self.number);

    fs::write(&path, &frequency.as_khz().to_string())
      .with_context(|| format!("failed to set {name} for {self}"))?;
    let observed = self.read_frequency(name)?;
    fs::observe(path, observed.as_khz().to_string());
    if observed != frequency {
      log::warn!(
        "{self} {name} requested {frequency}, kernel applied {observed}",
      );
    } else {
      log::info!("{self} {name} set to {frequency}");
    }

    Ok(())
  }
}

fn read(
  path: impl AsRef<std::path::Path>,
) -> anyhow::Result<Option<Frequency>> {
  fs::read_n::<u64>(path)?
    .map(Frequency::from_khz)
    .transpose()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn frequency(khz: u64) -> Frequency {
    Frequency::from_khz(khz).expect("valid test frequency")
  }

  #[test]
  fn frequency_rejects_invalid_mhz() {
    for mhz in [-1.0, 0.0, f64::INFINITY, u64::MAX as f64] {
      assert!(Frequency::from_mhz(mhz).is_err(), "{mhz} must be rejected");
    }
  }

  #[test]
  fn frequency_preserves_fractional_mhz_as_exact_khz() {
    assert_eq!(
      Frequency::from_mhz(702.4)
        .expect("valid frequency")
        .as_khz(),
      702_400
    );
  }

  #[test]
  fn transition_raises_maximum_before_minimum() {
    let transition = Transition::new(
      Range::new(frequency(400_000), frequency(4_000_000)).unwrap(),
      Range::new(frequency(400_000), frequency(1_000_000)).unwrap(),
      Some(frequency(2_000_000)),
      Some(frequency(3_000_000)),
    )
    .expect("valid transition");

    assert_eq!(transition.order, WriteOrder::MaximumFirst);
  }

  #[test]
  fn transition_lowers_minimum_before_maximum() {
    let transition = Transition::new(
      Range::new(frequency(400_000), frequency(4_000_000)).unwrap(),
      Range::new(frequency(2_000_000), frequency(4_000_000)).unwrap(),
      Some(frequency(1_000_000)),
      Some(frequency(1_500_000)),
    )
    .expect("valid transition");

    assert_eq!(transition.order, WriteOrder::MinimumFirst);
  }

  #[test]
  fn transition_rejects_inverted_or_out_of_hardware_range() {
    let hardware =
      Range::new(frequency(702_000), frequency(3_504_000)).unwrap();
    let current = Range::new(frequency(702_000), frequency(3_504_000)).unwrap();

    assert!(
      Transition::new(
        hardware,
        current,
        Some(frequency(2_000_000)),
        Some(frequency(1_800_000)),
      )
      .is_err()
    );
    assert!(
      Transition::new(hardware, current, Some(frequency(200_000)), None,)
        .is_err()
    );
  }
}
