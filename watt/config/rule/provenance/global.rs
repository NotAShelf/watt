use std::path::{
  Path,
  PathBuf,
};

use anyhow::Context as _;

use crate::{
  config::EvaluatedRule,
  cpu,
  fs,
};

pub(super) fn paths(
  field: &str,
  actions: &EvaluatedRule,
) -> anyhow::Result<Vec<PathBuf>> {
  let paths = match field {
    "turbo" => {
      cpu::Cpu::boost_capabilities(actions.cpu.keys().map(AsRef::as_ref))
        .map_or_else(Vec::new, |boost| boost.paths)
    },
    "min_perf_pct" | "max_perf_pct" => {
      vec![Path::new("/sys/devices/system/cpu/intel_pstate").join(field)]
    },
    "link_power_management_policy" => return alpm_paths(),
    "power_save" | "power_save_controller" => {
      ["snd_hda_intel", "snd_ac97_codec"]
        .into_iter()
        .map(|module| {
          PathBuf::from(format!("/sys/module/{module}/parameters/{field}"))
        })
        .collect()
    },
    "platform_profile" => {
      vec![PathBuf::from("/sys/firmware/acpi/platform_profile")]
    },
    "enabled" | "defrag" => {
      ["transparent_hugepage", "redhat_transparent_hugepage"]
        .into_iter()
        .map(|name| Path::new("/sys/kernel/mm").join(name).join(field))
        .collect()
    },
    _ => vec![Path::new("/proc/sys/vm").join(field)],
  };
  Ok(paths)
}

pub(crate) fn alpm_paths() -> anyhow::Result<Vec<PathBuf>> {
  let Some(entries) = fs::read_dir("/sys/class/scsi_host")? else {
    return Ok(Vec::new());
  };
  let mut paths = Vec::new();
  for entry in entries {
    let entry = entry.context("failed to read SCSI host entry")?;
    if let Some(root) = fs::canonicalize(entry.path())? {
      let path = root.join("link_power_management_policy");
      if fs::exists(&path) {
        paths.push(path);
      }
    }
  }
  paths.sort();
  paths.dedup();
  Ok(paths)
}
