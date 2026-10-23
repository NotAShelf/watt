use std::{
  collections::BTreeSet,
  path::PathBuf,
};

use anyhow::{
  Context,
  bail,
};

use super::{
  Cpu,
  write_verified,
};
use crate::fs;

const GLOBAL_CONTROLS: [(&str, bool); 4] = [
  ("/sys/devices/system/cpu/intel_pstate/no_turbo", true),
  ("/sys/devices/system/cpu/amd_pstate/cpufreq/boost", false),
  (
    "/sys/devices/system/cpu/cpufreq/amd_pstate_enable_boost",
    false,
  ),
  ("/sys/devices/system/cpu/cpufreq/boost", false),
];

#[derive(Debug)]
pub struct BoostCapabilities {
  pub paths:    Vec<PathBuf>,
  pub global:   bool,
  pub inverted: bool,
}

impl Cpu {
  pub fn boost_capabilities<'a>(
    cpus: impl Iterator<Item = &'a Cpu>,
  ) -> Option<BoostCapabilities> {
    BoostCapabilities::discover(cpus)
  }
}

impl BoostCapabilities {
  pub(super) fn discover<'a>(
    cpus: impl Iterator<Item = &'a Cpu>,
  ) -> Option<Self> {
    if let Some((path, inverted)) =
      GLOBAL_CONTROLS.iter().find(|(path, _)| fs::exists(path))
    {
      return Some(Self {
        paths:    vec![PathBuf::from(path)],
        inverted: *inverted,
        global:   true,
      });
    }
    let paths: BTreeSet<_> = cpus
      .map(|cpu| cpu.cpufreq_path("boost"))
      .filter(|path| fs::exists(path))
      .collect();
    (!paths.is_empty()).then(|| {
      Self {
        paths:    paths.into_iter().collect(),
        inverted: false,
        global:   false,
      }
    })
  }

  pub(super) fn read(&self) -> anyhow::Result<bool> {
    let mut enabled = true;
    for path in &self.paths {
      let value = fs::read_n::<u8>(path)?.with_context(|| {
        format!("boost control disappeared: {}", path.display())
      })?;
      if value > 1 {
        bail!("invalid boost value {value} at {}", path.display());
      }
      enabled &= (value == 1) != self.inverted;
    }
    Ok(enabled)
  }

  pub(super) fn write(&self, enabled: bool) -> anyhow::Result<()> {
    let value = if enabled != self.inverted { "1" } else { "0" };
    let failures: Vec<_> = self
      .paths
      .iter()
      .filter_map(|path| {
        write_verified(path, value, "CPU boost")
          .err()
          .map(|error| format!("{}: {error:#}", path.display()))
      })
      .collect();
    if !failures.is_empty() {
      bail!("CPU boost application failed: {}", failures.join("; "));
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::{
    fs as stdfs,
    io::ErrorKind,
    iter,
  };

  use super::*;
  use crate::test_support::SystemFixture;

  #[test]
  fn global_backends_share_read_write_semantics() {
    for (path, inverted) in GLOBAL_CONTROLS {
      let fixture = SystemFixture::new();
      fixture.write(path, if inverted { "1" } else { "0" });
      let capability = Cpu::boost_capabilities(iter::empty()).unwrap();
      assert!(capability.global);
      assert_eq!(capability.inverted, inverted);
      assert_eq!(capability.paths, vec![PathBuf::from(path)]);
      assert_eq!(Cpu::turbo(iter::empty()).unwrap(), Some(false));
      Cpu::set_turbo(true, iter::empty()).unwrap();
      assert_eq!(Cpu::turbo(iter::empty()).unwrap(), Some(true));
      fixture.write(path, "2");
      assert!(Cpu::turbo(iter::empty()).is_err());
    }
  }

  #[test]
  fn unreadable_boost_is_an_error_rather_than_disabled_or_unsupported() {
    let fixture = SystemFixture::new();
    assert_eq!(Cpu::turbo(iter::empty()).unwrap(), None);
    let path = GLOBAL_CONTROLS[0].0;
    fixture.write(path, "1");
    fixture.fail_read(path, ErrorKind::PermissionDenied);
    assert!(Cpu::turbo(iter::empty()).is_err());
    fixture.clear_io(path);
    assert_eq!(Cpu::turbo(iter::empty()).unwrap(), Some(false));
  }

  #[test]
  fn policy_boost_attempts_each_target_and_reports_partial_failure() {
    let fixture = SystemFixture::new();
    let first = fixture.policy(0, &[0, 1], "amd-pstate-epp").join("boost");
    let second = fixture.policy(2, &[2], "amd-pstate-epp").join("boost");
    fixture.cpu_telemetry(&[0, 1, 2]);
    fixture.write(&first, "0");
    fixture.write(&second, "0");
    let cpus = Cpu::all().unwrap();
    let capability = Cpu::boost_capabilities(cpus.iter()).unwrap();
    assert!(!capability.global);
    assert!(!capability.inverted);
    assert_eq!(capability.paths, vec![first.clone(), second.clone()]);
    fixture.write_result(&first, Err(ErrorKind::PermissionDenied));
    assert!(Cpu::set_turbo(true, cpus.iter()).is_err());
    assert_eq!(fixture.write_count(&first), 1);
    assert_eq!(fixture.write_count(&second), 1);
    assert_eq!(stdfs::read_to_string(&second).unwrap(), "1");
    assert_eq!(Cpu::turbo(cpus.iter()).unwrap(), Some(false));
    fixture.clear_io(&first);
    Cpu::set_turbo(true, cpus.iter()).unwrap();
    assert_eq!(Cpu::turbo(cpus.iter()).unwrap(), Some(true));
  }
}
