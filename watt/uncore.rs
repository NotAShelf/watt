use std::{
  fmt,
  hash,
  path::PathBuf,
};

use anyhow::{
  Context,
  bail,
};
use yansi::Paint as _;

use crate::fs;

const UNCORE_PATH: &str = "/sys/devices/system/cpu/intel_uncore_frequency";

#[derive(Debug, Clone)]
pub struct Uncore {
  pub name:            String,
  pub path:            PathBuf,
  pub initial_min_khz: u64,
  pub initial_max_khz: u64,
  pub min_khz:         u64,
  pub max_khz:         u64,
}

impl PartialEq for Uncore {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name
  }
}

impl Eq for Uncore {}

impl hash::Hash for Uncore {
  fn hash<H: hash::Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl fmt::Display for Uncore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "uncore device '{name}'", name = self.name.cyan())
  }
}

impl Uncore {
  pub fn all() -> anyhow::Result<Vec<Self>> {
    log::info!("detecting Intel uncore frequency devices...");

    let Some(entries) = fs::read_dir(UNCORE_PATH)
      .context("failed to read Intel uncore frequency devices")?
    else {
      log::debug!("Intel uncore frequency control is not available");
      return Ok(Vec::new());
    };

    let mut uncores = Vec::new();

    for entry in entries {
      let entry = entry
        .with_context(|| format!("failed to read entry of '{UNCORE_PATH}'"))?;
      let path = entry.path();

      if !path.is_dir() || !path.join("min_freq_khz").exists() {
        continue;
      }

      let name = entry.file_name().to_string_lossy().to_string();
      let uncore = Self::scan(name, path)?;
      uncores.push(uncore);
    }

    log::info!("detected {len} uncore devices", len = uncores.len());
    Ok(uncores)
  }

  fn scan(name: String, path: PathBuf) -> anyhow::Result<Self> {
    let initial_min_khz = read_required(&path, "initial_min_freq_khz")?;
    let initial_max_khz = read_required(&path, "initial_max_freq_khz")?;
    let min_khz = read_required(&path, "min_freq_khz")?;
    let max_khz = read_required(&path, "max_freq_khz")?;

    Ok(Self {
      name,
      path,
      initial_min_khz,
      initial_max_khz,
      min_khz,
      max_khz,
    })
  }

  pub fn set_min_khz(&self, value: u64) -> anyhow::Result<()> {
    if value < self.initial_min_khz {
      bail!(
        "new uncore minimum frequency ({value} kHz) cannot be lower than the \
         initial minimum frequency ({min} kHz) for {self}",
        min = self.initial_min_khz,
      );
    }
    fs::write(self.path.join("min_freq_khz"), &value.to_string())
      .with_context(|| format!("failed to set minimum frequency for {self}"))?;

    log::info!("{self} minimum frequency set to {value} kHz");
    Ok(())
  }

  pub fn set_max_khz(&self, value: u64) -> anyhow::Result<()> {
    if value > self.initial_max_khz {
      bail!(
        "new uncore maximum frequency ({value} kHz) cannot be higher than the \
         initial maximum frequency ({max} kHz) for {self}",
        max = self.initial_max_khz,
      );
    }
    fs::write(self.path.join("max_freq_khz"), &value.to_string())
      .with_context(|| format!("failed to set maximum frequency for {self}"))?;

    log::info!("{self} maximum frequency set to {value} kHz");
    Ok(())
  }
}

fn read_required(path: &std::path::Path, file: &str) -> anyhow::Result<u64> {
  fs::read_n::<u64>(path.join(file))?
    .with_context(|| format!("missing uncore frequency file '{file}'"))
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub frequency_khz_minimum: Option<u64>,
  pub frequency_khz_maximum: Option<u64>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.frequency_khz_minimum.is_some() && self.frequency_khz_maximum.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      frequency_khz_minimum: self
        .frequency_khz_minimum
        .or(that.frequency_khz_minimum),
      frequency_khz_maximum: self
        .frequency_khz_maximum
        .or(that.frequency_khz_maximum),
    }
  }

  pub fn apply(&self, uncore: &Uncore) -> anyhow::Result<()> {
    let target_min = self.frequency_khz_minimum.unwrap_or(uncore.min_khz);
    let target_max = self.frequency_khz_maximum.unwrap_or(uncore.max_khz);

    if target_min > target_max {
      bail!(
        "new uncore minimum frequency ({target_min} kHz) cannot be higher \
         than the new maximum frequency ({target_max} kHz) for {uncore}"
      );
    }

    if target_min > uncore.max_khz {
      if let Some(value) = self.frequency_khz_maximum {
        uncore.set_max_khz(value)?;
      }
      if let Some(value) = self.frequency_khz_minimum {
        uncore.set_min_khz(value)?;
      }
    } else {
      if let Some(value) = self.frequency_khz_minimum {
        uncore.set_min_khz(value)?;
      }
      if let Some(value) = self.frequency_khz_maximum {
        uncore.set_max_khz(value)?;
      }
    }

    Ok(())
  }
}
