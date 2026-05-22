use std::{
  collections::HashMap,
  fmt,
  str::FromStr,
  time::Instant,
};

use serde::{
  Deserialize,
  Serialize,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl std::error::Error for InvalidPowerProfile {}

#[derive(Debug, Clone)]
pub struct ProfileHold {
  pub cookie:         u32,
  pub profile:        PowerProfile,
  pub reason:         String,
  pub application_id: String,
  pub timestamp:      Instant,
}

#[derive(Debug)]
pub struct ProfileState {
  preferred_profile: PowerProfile,
  holds:             HashMap<u32, ProfileHold>,
  next_cookie:       u32,
}

impl ProfileState {
  pub fn new() -> Self {
    Self {
      preferred_profile: PowerProfile::Balanced,
      holds:             HashMap::new(),
      next_cookie:       1,
    }
  }

  pub fn add_hold(
    &mut self,
    profile: PowerProfile,
    reason: String,
    application_id: String,
  ) -> u32 {
    // Find an unused cookie, we'll want to handle wrap-around collision
    let mut cookie = self.next_cookie;
    while self.holds.contains_key(&cookie) {
      cookie = cookie.wrapping_add(1);
    }
    self.next_cookie = cookie.wrapping_add(1);

    let hold = ProfileHold {
      cookie,
      profile,
      reason,
      application_id,
      timestamp: Instant::now(),
    };

    log::info!(
      "profile hold added: cookie={cookie}, profile={profile:?}, app={app}",
      app = hold.application_id,
    );

    self.holds.insert(cookie, hold);
    cookie
  }

  pub fn release_hold(&mut self, cookie: u32) -> anyhow::Result<()> {
    self
      .holds
      .remove(&cookie)
      .ok_or_else(|| anyhow::anyhow!("hold with cookie {cookie} not found"))?;

    log::info!("profile hold released: cookie={cookie}");
    Ok(())
  }

  pub fn set_preference(&mut self, profile: PowerProfile) {
    if self.preferred_profile != profile {
      log::info!(
        "profile preference changed: {old:?} -> {new:?}",
        old = self.preferred_profile,
        new = profile,
      );
      self.preferred_profile = profile;
    }

    if !self.holds.is_empty() {
      log::info!(
        "clearing {count} profile holds after manual profile change",
        count = self.holds.len(),
      );
      self.holds.clear();
    }
  }

  pub fn get_preference(&self) -> PowerProfile {
    self.preferred_profile
  }

  pub fn get_effective_profile(&self) -> PowerProfile {
    for profile in [
      PowerProfile::PowerSaver,
      PowerProfile::Performance,
      PowerProfile::Balanced,
    ] {
      if self.holds.values().any(|hold| hold.profile == profile) {
        return profile;
      }
    }

    self.preferred_profile
  }

  pub fn get_holds(&self) -> Vec<ProfileHold> {
    self.holds.values().cloned().collect()
  }
}

impl Default for ProfileState {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::{
    PowerProfile,
    ProfileState,
  };

  #[test]
  fn profile_holds_prefer_power_saver_over_performance() {
    let mut state = ProfileState::new();

    state.add_hold(
      PowerProfile::Performance,
      "high-performance task".to_owned(),
      "test.performance".to_owned(),
    );
    state.add_hold(
      PowerProfile::PowerSaver,
      "low battery".to_owned(),
      "test.power-saver".to_owned(),
    );

    assert_eq!(state.get_effective_profile(), PowerProfile::PowerSaver);
  }

  #[test]
  fn manual_profile_change_clears_holds() {
    let mut state = ProfileState::new();

    state.add_hold(
      PowerProfile::Performance,
      "high-performance task".to_owned(),
      "test.performance".to_owned(),
    );
    state.set_preference(PowerProfile::Balanced);

    assert_eq!(state.get_effective_profile(), PowerProfile::Balanced);
    assert!(state.get_holds().is_empty());
  }
}
