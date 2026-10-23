use std::{
  fmt,
  fs::DirEntry,
  hash,
  iter,
  path::{
    Path,
    PathBuf,
  },
};

use anyhow::{
  Context,
  bail,
};
use yansi::Paint as _;

use crate::fs;

#[derive(Debug, Clone)]
pub struct Gpu {
  pub name:    String,
  pub path:    PathBuf,
  radeon_path: Option<PathBuf>,
}

impl PartialEq for Gpu {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name
  }
}

impl Eq for Gpu {}

impl hash::Hash for Gpu {
  fn hash<H: hash::Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl fmt::Display for Gpu {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "GPU '{name}'", name = self.name.cyan())
  }
}

impl Gpu {
  pub fn all() -> anyhow::Result<Vec<Self>> {
    let Some(entries) = fs::read_dir("/sys/class/drm")? else {
      return Ok(Vec::new());
    };

    let mut gpus = Vec::new();

    for entry in entries {
      let entry = entry.context("failed to read DRM device entry")?;
      if let Some(gpu) = Self::scan(entry)? {
        gpus.push(gpu);
      }
    }

    Ok(gpus)
  }

  fn scan(entry: DirEntry) -> anyhow::Result<Option<Self>> {
    let name = entry.file_name().to_string_lossy().into_owned();
    if !name.starts_with("card") {
      return Ok(None);
    }
    let Some(path) = fs::canonicalize(entry.path())? else {
      return Ok(None);
    };
    let radeon_path = fs::canonicalize(path.join("device"))?
      .filter(|device| device.join("power_method").exists());
    if !Self::panel_power_savings_path(&path).exists() && radeon_path.is_none()
    {
      return Ok(None);
    }
    Ok(Some(Self {
      name,
      path,
      radeon_path,
    }))
  }

  pub(crate) fn control_roots(&self) -> impl Iterator<Item = &Path> {
    iter::once(self.path.as_path()).chain(self.radeon_path.as_deref())
  }

  fn panel_power_savings_path(path: &Path) -> PathBuf {
    path.join("amdgpu/panel_power_savings")
  }

  pub fn set_panel_power_savings(&self, value: u8) -> anyhow::Result<()> {
    if value > 4 {
      bail!("GPU panel power savings must be between 0 and 4, got {value}");
    }

    let path = Self::panel_power_savings_path(&self.path);
    if !path.exists() {
      log::debug!("{self} does not support AMDGPU panel power savings");
      return Ok(());
    }

    fs::write(path, &value.to_string())
      .with_context(|| format!("failed to set panel power savings for {self}"))
  }

  pub fn set_radeon_powersave(&self, value: &str) -> anyhow::Result<()> {
    let Some(path) = &self.radeon_path else {
      log::debug!("{self} does not support Radeon power profiles");
      return Ok(());
    };
    let method = path.join("power_method");

    match value {
      "default" | "auto" | "low" | "mid" | "high" => {
        fs::write(&method, "profile")
          .with_context(|| format!("failed to set power method for {self}"))?;
        fs::write(path.join("power_profile"), value)
          .with_context(|| format!("failed to set power profile for {self}"))?;
      },
      "dynpm" => {
        fs::write(&method, "dynpm")
          .with_context(|| format!("failed to set power method for {self}"))?
      },
      "dpm-battery" | "dpm-balanced" | "dpm-performance" => {
        fs::write(&method, "dpm")
          .with_context(|| format!("failed to set power method for {self}"))?;
        fs::write(
          path.join("power_dpm_state"),
          value.trim_start_matches("dpm-"),
        )
        .with_context(|| format!("failed to set DPM state for {self}"))?;
      },
      _ => bail!("invalid Radeon powersave value: {value}"),
    }

    Ok(())
  }
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub panel_power_savings: Option<u8>,
  pub radeon_powersave:    Option<String>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.panel_power_savings.is_some() && self.radeon_powersave.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      panel_power_savings: self
        .panel_power_savings
        .or(that.panel_power_savings),
      radeon_powersave:    self
        .radeon_powersave
        .or_else(|| that.radeon_powersave.clone()),
    }
  }

  pub fn apply(&self, gpu: &Gpu) -> anyhow::Result<()> {
    if let Some(value) = self.panel_power_savings {
      gpu.set_panel_power_savings(value)?;
    }
    if let Some(value) = &self.radeon_powersave {
      gpu.set_radeon_powersave(value)?;
    }

    Ok(())
  }
}
