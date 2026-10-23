use std::path::{
  Path,
  PathBuf,
};

use anyhow::Context as _;

use crate::fs;

/// A shared cpufreq control directory and its logical CPUs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
  /// Kernel policy name, or a CPU name for a legacy directory.
  pub name:         String,
  /// Canonical path of the shared controls.
  pub path:         PathBuf,
  /// Online and offline CPUs that belong to the policy.
  pub related_cpus: Vec<u32>,
  /// Scaling driver reported by the kernel, when available.
  pub driver:       Option<String>,
}

impl Policy {
  pub(super) fn scan(path: PathBuf, cpu: u32) -> anyhow::Result<Self> {
    let policy = path
      .file_name()
      .and_then(|name| name.to_str())
      .filter(|name| name.starts_with("policy"));
    let (name, related_cpus) = match policy {
      Some(name) => (name.to_owned(), Self::read_cpus(&path)?),
      None => (format!("cpu{cpu}"), vec![cpu]),
    };
    let driver = fs::read(path.join("scaling_driver"))?;
    Ok(Self {
      name,
      path,
      related_cpus,
      driver,
    })
  }

  fn read_cpus(path: &Path) -> anyhow::Result<Vec<u32>> {
    let list = match fs::read(path.join("related_cpus"))? {
      Some(list) => list,
      None => {
        fs::read(path.join("affected_cpus"))?.with_context(|| {
          format!("CPU list is unavailable for {}", path.display())
        })?
      },
    };
    let mut cpus = list
      .split_whitespace()
      .map(str::parse)
      .collect::<Result<Vec<u32>, _>>()
      .with_context(|| format!("invalid CPU list for {}", path.display()))?;
    cpus.sort_unstable();
    cpus.dedup();
    Ok(cpus)
  }
}
