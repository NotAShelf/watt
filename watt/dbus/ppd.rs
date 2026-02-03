use std::{
  collections::HashMap,
  sync::Arc,
};

use tokio::sync::RwLock;
use zbus::{
  fdo,
  interface,
  object_server::SignalEmitter,
  zvariant::Value,
};

use crate::{
  profile::{
    PowerProfile,
    ProfileHold,
  },
  system::DaemonState,
};

pub struct PowerProfilesInterface {
  state: Arc<RwLock<DaemonState>>,
}

impl PowerProfilesInterface {
  pub fn new(state: Arc<RwLock<DaemonState>>) -> Self {
    Self { state }
  }
}

#[interface(name = "net.hadess.PowerProfiles")]
impl PowerProfilesInterface {
  // Properties
  #[zbus(property)]
  async fn active_profile(&self) -> String {
    let state = self.state.read().await;
    state.profile.get_effective_profile().as_str().to_owned()
  }

  #[zbus(property)]
  async fn set_active_profile(&self, profile: &str) -> zbus::Result<()> {
    let profile = match PowerProfile::from_str(profile) {
      Some(profile) => profile,
      None => {
        return Err(zbus::Error::from(fdo::Error::InvalidArgs(format!(
          "invalid profile: {profile}, valid: performance, balanced, \
           power-saver"
        ))));
      },
    };

    let mut state = self.state.write().await;
    state.profile.set_preference(profile);

    log::info!(
      "D-Bus: active profile set to {profile}",
      profile = profile.as_str()
    );

    Ok(())
  }

  #[zbus(property)]
  async fn profiles(&self) -> Vec<HashMap<String, Value<'_>>> {
    PowerProfile::all()
      .iter()
      .map(|profile| {
        let mut map = HashMap::new();
        map.insert("Profile".to_owned(), Value::from(profile.as_str()));
        map.insert("Driver".to_owned(), Value::from("watt"));
        map.insert("CpuDriver".to_owned(), Value::from("unknown"));
        map
      })
      .collect()
  }

  #[zbus(property)]
  async fn actions(&self) -> Vec<String> {
    Vec::new()
  }

  #[zbus(property)]
  async fn performance_degraded(&self) -> String {
    let state = self.state.read().await;
    state.performance_degraded.clone().unwrap_or_default()
  }

  #[zbus(property)]
  async fn performance_inhibited(&self) -> String {
    let state = self.state.read().await;
    match state.profile.get_holds().first() {
      Some(hold) => hold.reason.clone(),
      None => String::new(),
    }
  }

  #[zbus(property)]
  async fn active_profile_holds(&self) -> Vec<HashMap<String, Value<'_>>> {
    let state = self.state.read().await;
    state
      .profile
      .get_holds()
      .into_iter()
      .map(|hold: ProfileHold| {
        let mut map = HashMap::new();
        map.insert("Profile".to_owned(), Value::from(hold.profile.as_str()));
        map.insert("Reason".to_owned(), Value::from(hold.reason));
        map
          .insert("ApplicationId".to_owned(), Value::from(hold.application_id));
        map
      })
      .collect()
  }

  async fn hold_profile(
    &self,
    #[zbus(signal_emitter)] signal_emitter: SignalEmitter<'_>,
    profile: String,
    reason: String,
    application_id: String,
  ) -> fdo::Result<u32> {
    let profile = match PowerProfile::from_str(&profile) {
      Some(profile) => profile,
      None => {
        return Err(fdo::Error::InvalidArgs(format!(
          "invalid profile: {profile}"
        )));
      },
    };

    let mut state = self.state.write().await;
    let cookie = state.profile.add_hold(profile, reason, application_id);

    log::info!("D-Bus profile hold added, cookie={cookie}");

    // Emit property change signals
    drop(state); // release lock before emitting signals

    // Log signal failures but don't fail the operation.
    // State was already mutated.
    if let Err(e) = self.active_profile_holds_changed(&signal_emitter).await {
      log::warn!("failed to emit ActiveProfileHolds change signal: {e}");
    }

    if let Err(e) = self.active_profile_changed(&signal_emitter).await {
      log::warn!("failed to emit ActiveProfile change signal: {e}");
    }

    Ok(cookie)
  }

  async fn release_profile(
    &self,
    #[zbus(signal_emitter)] signal_emitter: SignalEmitter<'_>,
    cookie: u32,
  ) -> fdo::Result<()> {
    let mut state = self.state.write().await;
    state
      .profile
      .release_hold(cookie)
      .map_err(|error| fdo::Error::Failed(error.to_string()))?;

    log::info!("D-Bus profile hold released, cookie={cookie}");

    drop(state);

    if let Err(e) = self.active_profile_holds_changed(&signal_emitter).await {
      log::warn!("Failed to emit ActiveProfileHolds change signal: {e}");
    }

    if let Err(e) = self.active_profile_changed(&signal_emitter).await {
      log::warn!("Failed to emit ActiveProfile change signal: {e}");
    }

    Ok(())
  }
}
