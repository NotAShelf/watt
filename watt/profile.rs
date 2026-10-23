use std::{
  collections::HashMap,
  error::Error,
  fmt,
  str::FromStr,
  sync::{
    Arc,
    Weak,
  },
  time::Instant,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseHoldError {
  NotOwner,
  NotFound(u32),
}

impl fmt::Display for ReleaseHoldError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::NotOwner => {
        f.write_str("profile hold belongs to another D-Bus owner")
      },
      Self::NotFound(cookie) => {
        write!(f, "hold with cookie {cookie} not found")
      },
    }
  }
}

impl Error for ReleaseHoldError {}

#[derive(Debug, Clone)]
pub struct ProfileHold {
  pub cookie:         u32,
  pub profile:        PowerProfile,
  pub reason:         String,
  pub application_id: String,
  pub owner:          String,
  pub timestamp:      Instant,
}

#[derive(Debug)]
pub(crate) struct OwnerCheck {
  owner:      String,
  generation: Arc<()>,
}

#[derive(Debug)]
pub struct ProfileState {
  preferred_profile: PowerProfile,
  holds:             HashMap<u32, ProfileHold>,
  next_cookie:       u32,
  owner_checks:      HashMap<String, Weak<()>>,
}

impl ProfileState {
  pub fn new() -> Self {
    Self {
      preferred_profile: PowerProfile::Balanced,
      holds:             HashMap::new(),
      next_cookie:       1,
      owner_checks:      HashMap::new(),
    }
  }

  pub fn add_hold(
    &mut self,
    profile: PowerProfile,
    reason: String,
    application_id: String,
  ) -> u32 {
    self.add_hold_for(profile, reason, application_id, String::new())
  }

  pub(crate) fn begin_owner_check(&mut self, owner: &str) -> OwnerCheck {
    // Cancelled requests leave weak entries, reclaimed by the next check.
    self
      .owner_checks
      .retain(|_, check| check.strong_count() > 0);
    let current = self.owner_checks.entry(owner.to_owned()).or_default();
    let generation = current.upgrade().unwrap_or_else(|| Arc::new(()));
    *current = Arc::downgrade(&generation);
    OwnerCheck {
      owner: owner.to_owned(),
      generation,
    }
  }

  pub(crate) fn add_checked_hold(
    &mut self,
    check: OwnerCheck,
    profile: PowerProfile,
    reason: String,
    application_id: String,
  ) -> Option<u32> {
    let current = self.owner_checks.get(&check.owner)?;
    if !current.ptr_eq(&Arc::downgrade(&check.generation)) {
      return None;
    }
    Some(self.add_hold_for(profile, reason, application_id, check.owner))
  }

  pub fn add_hold_for(
    &mut self,
    profile: PowerProfile,
    reason: String,
    application_id: String,
    owner: String,
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
      owner,
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
    self.release_hold_for(cookie, "").map_err(Into::into)
  }

  pub fn release_hold_for(
    &mut self,
    cookie: u32,
    owner: &str,
  ) -> Result<(), ReleaseHoldError> {
    if self
      .holds
      .get(&cookie)
      .is_some_and(|hold| hold.owner != owner)
    {
      return Err(ReleaseHoldError::NotOwner);
    }
    self
      .holds
      .remove(&cookie)
      .ok_or(ReleaseHoldError::NotFound(cookie))?;

    log::info!("profile hold released: cookie={cookie}");
    Ok(())
  }

  pub fn release_owner(&mut self, owner: &str) -> Vec<u32> {
    self.owner_checks.remove(owner);
    self
      .holds
      .extract_if(|_, hold| hold.owner == owner)
      .map(|(cookie, _)| cookie)
      .collect()
  }

  pub fn release_bus_holds(&mut self) {
    self.owner_checks.clear();
    self.holds.retain(|_, hold| hold.owner.is_empty());
  }

  pub fn set_preference(&mut self, profile: PowerProfile) -> Vec<u32> {
    if self.preferred_profile != profile {
      log::info!(
        "profile preference changed: {old:?} -> {new:?}",
        old = self.preferred_profile,
        new = profile,
      );
      self.preferred_profile = profile;
    }

    let removed = self.holds.keys().copied().collect();
    if !self.holds.is_empty() {
      log::info!(
        "clearing {count} profile holds after manual profile change",
        count = self.holds.len(),
      );
      self.holds.clear();
    }
    removed
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

  #[test]
  fn owners_cannot_release_each_others_holds() {
    let mut state = ProfileState::new();
    let cookie = state.add_hold_for(
      PowerProfile::Performance,
      "work".into(),
      "app".into(),
      ":1.10".into(),
    );
    assert!(state.release_hold_for(cookie, ":1.11").is_err());
    assert!(state.release_hold_for(cookie, "").is_err());
    assert!(state.release_hold(cookie).is_err());
    assert_eq!(state.get_holds().len(), 1);
    state.release_hold_for(cookie, ":1.10").unwrap();
    assert!(state.get_holds().is_empty());

    state.add_hold_for(
      PowerProfile::PowerSaver,
      "battery".into(),
      "app".into(),
      ":1.12".into(),
    );
    assert_eq!(state.release_owner(":1.12").len(), 1);
    assert!(state.get_holds().is_empty());
  }

  #[test]
  fn owner_checks_cannot_survive_disconnect_or_bus_replacement() {
    let mut state = ProfileState::new();
    let first = state.begin_owner_check(":1.42");
    let concurrent = state.begin_owner_check(":1.42");
    assert!(
      state
        .add_checked_hold(
          first,
          PowerProfile::Performance,
          "render".into(),
          "app".into()
        )
        .is_some()
    );
    assert_eq!(state.release_owner(":1.42").len(), 1);
    assert!(
      state
        .add_checked_hold(
          concurrent,
          PowerProfile::Performance,
          "render".into(),
          "app".into()
        )
        .is_none()
    );

    let old_bus = state.begin_owner_check(":1.42");
    state.release_bus_holds();
    let new_bus = state.begin_owner_check(":1.42");
    assert!(
      state
        .add_checked_hold(
          old_bus,
          PowerProfile::Performance,
          "render".into(),
          "app".into()
        )
        .is_none()
    );
    assert!(
      state
        .add_checked_hold(
          new_bus,
          PowerProfile::PowerSaver,
          "battery".into(),
          "app".into()
        )
        .is_some()
    );
    assert_eq!(state.get_holds().len(), 1);
  }
}
