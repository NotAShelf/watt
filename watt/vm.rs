use std::path::Path;

use anyhow::{
  Context,
  bail,
};

use crate::fs;

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub dirty_bytes:                 Option<u64>,
  pub dirty_ratio:                 Option<u8>,
  pub dirty_background_bytes:      Option<u64>,
  pub dirty_background_ratio:      Option<u8>,
  pub transparent_hugepages:       Option<String>,
  pub transparent_hugepage_defrag: Option<String>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.dirty_bytes.is_some()
      && self.dirty_ratio.is_some()
      && self.dirty_background_bytes.is_some()
      && self.dirty_background_ratio.is_some()
      && self.transparent_hugepages.is_some()
      && self.transparent_hugepage_defrag.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      dirty_bytes:                 self.dirty_bytes.or(that.dirty_bytes),
      dirty_ratio:                 self.dirty_ratio.or(that.dirty_ratio),
      dirty_background_bytes:      self
        .dirty_background_bytes
        .or(that.dirty_background_bytes),
      dirty_background_ratio:      self
        .dirty_background_ratio
        .or(that.dirty_background_ratio),
      transparent_hugepages:       self
        .transparent_hugepages
        .or_else(|| that.transparent_hugepages.clone()),
      transparent_hugepage_defrag: self
        .transparent_hugepage_defrag
        .or_else(|| that.transparent_hugepage_defrag.clone()),
    }
  }

  pub fn validate(&self) -> anyhow::Result<()> {
    if self.dirty_bytes.is_some() && self.dirty_ratio.is_some() {
      bail!("`vm.dirty-bytes` conflicts with `vm.dirty-ratio`");
    }
    if self.dirty_background_bytes.is_some()
      && self.dirty_background_ratio.is_some()
    {
      bail!(
        "`vm.dirty-background-bytes` conflicts with \
         `vm.dirty-background-ratio`"
      );
    }

    if let Some(value) = &self.transparent_hugepages
      && !matches!(&**value, "always" | "madvise" | "never")
    {
      bail!("invalid `vm.transparent-hugepages`: {value}");
    }

    if let Some(value) = &self.transparent_hugepage_defrag
      && !matches!(
        &**value,
        "always" | "defer" | "defer+madvise" | "madvise" | "never"
      )
    {
      bail!("invalid `vm.transparent-hugepage-defrag`: {value}");
    }

    Ok(())
  }

  pub fn apply(&self) -> anyhow::Result<()> {
    self.validate()?;

    if let Some(value) = self.dirty_bytes {
      write_proc_sys_vm("dirty_bytes", value)?;
    }
    if let Some(value) = self.dirty_ratio {
      write_proc_sys_vm("dirty_ratio", value)?;
    }
    if let Some(value) = self.dirty_background_bytes {
      write_proc_sys_vm("dirty_background_bytes", value)?;
    }
    if let Some(value) = self.dirty_background_ratio {
      write_proc_sys_vm("dirty_background_ratio", value)?;
    }

    if let Some(value) = &self.transparent_hugepages {
      write_transparent_hugepage("enabled", value)?;
    }
    if let Some(value) = &self.transparent_hugepage_defrag {
      write_transparent_hugepage("defrag", value)?;
    }

    Ok(())
  }
}

fn write_proc_sys_vm(
  name: &str,
  value: impl std::fmt::Display,
) -> anyhow::Result<()> {
  fs::write(format!("/proc/sys/vm/{name}"), &value.to_string())
    .with_context(|| format!("failed to set vm sysctl '{name}'"))
}

fn write_transparent_hugepage(name: &str, value: &str) -> anyhow::Result<()> {
  let base = transparent_hugepage_path()
    .context("transparent hugepage control is not available")?;
  fs::write(base.join(name), value)
    .with_context(|| format!("failed to set transparent hugepage '{name}'"))
}

fn transparent_hugepage_path() -> Option<&'static Path> {
  let path = Path::new("/sys/kernel/mm/transparent_hugepage");
  if path.exists() {
    return Some(path);
  }

  let path = Path::new("/sys/kernel/mm/redhat_transparent_hugepage");
  path.exists().then_some(path)
}
