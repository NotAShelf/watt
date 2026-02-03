use std::{
  collections::HashMap,
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
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::Performance => "performance",
      Self::Balanced => "balanced",
      Self::PowerSaver => "power-saver",
    }
  }

  // FIXME: change this to a less ambigious name
  pub fn from_str(value: &str) -> Option<Self> {
    match value {
      "performance" => Some(Self::Performance),
      "balanced" => Some(Self::Balanced),
      "power-saver" => Some(Self::PowerSaver),
      _ => None,
    }
  }

  pub fn all() -> [Self; 3] {
    [Self::Performance, Self::Balanced, Self::PowerSaver]
  }
}

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
  }

  pub fn get_preference(&self) -> PowerProfile {
    self.preferred_profile
  }

  pub fn get_effective_profile(&self) -> PowerProfile {
    for profile in [
      PowerProfile::Performance,
      PowerProfile::Balanced,
      PowerProfile::PowerSaver,
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
