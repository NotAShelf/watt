use std::{
  error::Error,
  fmt,
  str::FromStr,
};

use serde::{
  Deserialize,
  Serialize,
};

#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  Hash,
  Serialize,
  Deserialize,
  schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum PowerProfile {
  Performance,
  Balanced,
  PowerSaver,
}

impl PowerProfile {
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Performance => "performance",
      Self::Balanced => "balanced",
      Self::PowerSaver => "power-saver",
    }
  }

  pub const fn all() -> [Self; 3] {
    [Self::Performance, Self::Balanced, Self::PowerSaver]
  }
}

impl fmt::Display for PowerProfile {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl FromStr for PowerProfile {
  type Err = InvalidPowerProfile;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    match value {
      "performance" => Ok(Self::Performance),
      "balanced" => Ok(Self::Balanced),
      "power-saver" => Ok(Self::PowerSaver),
      _ => Err(InvalidPowerProfile),
    }
  }
}

#[derive(Debug, Clone, Copy)]
pub struct InvalidPowerProfile;

impl fmt::Display for InvalidPowerProfile {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("invalid power profile")
  }
}

impl Error for InvalidPowerProfile {}
