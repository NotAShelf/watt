use anyhow::Context;

use crate::fs;

const AUDIO_MODULES: &[&str] = &["snd_hda_intel", "snd_ac97_codec"];

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub timeout_seconds:  Option<fs::Setting<u64>>,
  pub reset_controller: Option<fs::Setting<bool>>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.timeout_seconds.is_some() && self.reset_controller.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      timeout_seconds:  fs::prefer(self.timeout_seconds, &that.timeout_seconds),
      reset_controller: fs::prefer(
        self.reset_controller,
        &that.reset_controller,
      ),
    }
  }

  pub fn apply(&self) -> anyhow::Result<()> {
    for module in AUDIO_MODULES {
      if let Some(timeout) = &self.timeout_seconds {
        timeout.apply(|value| {
          write_if_exists(module, "power_save", &value.to_string())
        })?;
      }
      if let Some(reset) = &self.reset_controller {
        reset.apply(|&value| {
          write_if_exists(
            module,
            "power_save_controller",
            if value { "Y" } else { "N" },
          )
        })?;
      }
    }

    Ok(())
  }
}

fn write_if_exists(
  module: &str,
  parameter: &str,
  value: &str,
) -> anyhow::Result<()> {
  let path = format!("/sys/module/{module}/parameters/{parameter}");
  if fs::exists(&path) {
    fs::write(&path, value).with_context(|| {
      format!("failed to set audio module parameter '{path}'")
    })?;
  }

  Ok(())
}
