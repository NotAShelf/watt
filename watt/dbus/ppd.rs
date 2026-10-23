use std::{
  collections::HashMap,
  str::FromStr as _,
  sync::Arc,
};

use tokio::sync::RwLock;
use zbus::{
  fdo,
  interface,
  message::Header,
  object_server::SignalEmitter,
  zvariant::Value,
};

use crate::{
  profile::{
    PowerProfile,
    ReleaseHoldError,
  },
  system::DaemonState,
};

const DRIVER_NAME: &str = "watt";
const CPU_DRIVER_NAME: &str = "watt";

pub struct PowerProfilesInterface {
  state: Arc<RwLock<DaemonState>>,
}

impl PowerProfilesInterface {
  pub fn new(state: Arc<RwLock<DaemonState>>) -> Self {
    Self { state }
  }

  /// Emits the signals that follow a change to the hold set. The state change
  /// is already committed, so failures are only logged.
  pub(crate) async fn notify_holds_changed(
    &self,
    emitter: &SignalEmitter<'_>,
    released: &[u32],
  ) {
    if let Err(error) = self.active_profile_changed(emitter).await {
      log::warn!("failed to emit ActiveProfile change signal: {error}");
    }
    if let Err(error) = self.active_profile_holds_changed(emitter).await {
      log::warn!("failed to emit ActiveProfileHolds change signal: {error}");
    }
    if let Err(error) = self.performance_inhibited_changed(emitter).await {
      log::warn!("failed to emit PerformanceInhibited change signal: {error}");
    }
    for &cookie in released {
      if let Err(error) = Self::profile_released(emitter, cookie).await {
        log::warn!("failed to emit ProfileReleased signal: {error}");
      }
    }
  }
}

#[interface(name = "net.hadess.PowerProfiles")]
impl PowerProfilesInterface {
  // Properties
  #[zbus(property)]
  async fn active_profile(&self) -> String {
    let state = self.state.read().await;
    state.active_profile().as_str().to_owned()
  }

  #[zbus(property)]
  async fn set_active_profile(
    &self,
    profile: &str,
    #[zbus(connection)] connection: &zbus::Connection,
  ) -> zbus::Result<()> {
    let profile = PowerProfile::from_str(profile).map_err(|_| {
      fdo::Error::InvalidArgs(format!(
        "invalid profile: {profile}, valid: performance, balanced, power-saver"
      ))
    })?;
    let signal_emitter =
      SignalEmitter::new(connection, "/net/hadess/PowerProfiles")?;

    let released = self.state.write().await.set_active_profile(profile);
    self.notify_holds_changed(&signal_emitter, &released).await;

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
        map.insert("Driver".to_owned(), Value::from(DRIVER_NAME));
        map.insert("CpuDriver".to_owned(), Value::from(CPU_DRIVER_NAME));
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
    state.performance_degraded().unwrap_or_default().to_owned()
  }

  #[zbus(property)]
  async fn performance_inhibited(&self) -> String {
    let state = self.state.read().await;
    match state.profile_holds().first() {
      Some(hold) => hold.reason.clone(),
      None => String::new(),
    }
  }

  #[zbus(property)]
  async fn active_profile_holds(&self) -> Vec<HashMap<String, Value<'_>>> {
    let state = self.state.read().await;
    state
      .profile_holds()
      .into_iter()
      .map(|hold| {
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
    #[zbus(header)] header: Header<'_>,
    profile: String,
    reason: String,
    application_id: String,
  ) -> fdo::Result<u32> {
    let sender = header
      .sender()
      .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus sender".into()))?;
    let profile = match PowerProfile::from_str(&profile) {
      Ok(profile) => profile,
      Err(_) => {
        return Err(fdo::Error::InvalidArgs(format!(
          "invalid profile: {profile}"
        )));
      },
    };

    if profile == PowerProfile::Balanced {
      return Err(fdo::Error::InvalidArgs(
        "profile holds only support performance and power-saver".to_owned(),
      ));
    }

    let cookie = self.state.write().await.add_profile_hold_for(
      profile,
      reason,
      application_id,
      sender.to_string(),
    );

    log::info!("D-Bus profile hold added, cookie={cookie}");

    self.notify_holds_changed(&signal_emitter, &[]).await;

    Ok(cookie)
  }

  async fn release_profile(
    &self,
    #[zbus(signal_emitter)] signal_emitter: SignalEmitter<'_>,
    #[zbus(header)] header: Header<'_>,
    cookie: u32,
  ) -> fdo::Result<()> {
    let owner = header
      .sender()
      .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus sender".into()))?;
    self
      .state
      .write()
      .await
      .release_profile_hold_for(cookie, owner.as_str())
      .map_err(|error| {
        match error {
          ReleaseHoldError::NotOwner => {
            fdo::Error::AccessDenied(error.to_string())
          },
          ReleaseHoldError::NotFound(_) => {
            fdo::Error::InvalidArgs(error.to_string())
          },
        }
      })?;

    log::info!("D-Bus profile hold released, cookie={cookie}");

    self.notify_holds_changed(&signal_emitter, &[cookie]).await;

    Ok(())
  }

  #[zbus(signal)]
  pub(crate) async fn profile_released(
    emitter: &SignalEmitter<'_>,
    cookie: u32,
  ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use futures_util::StreamExt;
  use tokio::time::timeout;
  use zbus::{
    DBusError as _,
    Proxy,
    connection,
  };

  use super::*;
  use crate::dbus::test_support::TestBus;

  #[tokio::test]
  async fn exported_hold_calls_enforce_ownership() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      let _service = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("net.hadess.PowerProfiles")
        .unwrap()
        .serve_at(
          "/net/hadess/PowerProfiles",
          PowerProfilesInterface::new(state.clone()),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
      let owner = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let other = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let owner_proxy = Proxy::new(
        &owner,
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
      )
      .await
      .unwrap();
      let other_proxy = Proxy::new(
        &other,
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
      )
      .await
      .unwrap();
      assert_eq!(
        owner_proxy
          .get_property::<String>("ActiveProfile")
          .await
          .unwrap(),
        "balanced"
      );
      owner_proxy
        .set_property("ActiveProfile", "performance")
        .await
        .unwrap();
      assert_eq!(
        state.read().await.active_profile(),
        PowerProfile::Performance
      );
      let cookie: u32 = owner_proxy
        .call("HoldProfile", &("power-saver", "battery", "test"))
        .await
        .unwrap();
      assert_eq!(
        state.read().await.active_profile(),
        PowerProfile::PowerSaver
      );
      let denied = other_proxy
        .call::<_, _, ()>("ReleaseProfile", &(cookie,))
        .await
        .unwrap_err();
      assert_eq!(
        fdo::Error::from(denied).name().as_str(),
        "org.freedesktop.DBus.Error.AccessDenied"
      );
      assert_eq!(state.read().await.profile_holds().len(), 1);
      owner_proxy
        .call::<_, _, ()>("ReleaseProfile", &(cookie,))
        .await
        .unwrap();
      assert_eq!(
        state.read().await.active_profile(),
        PowerProfile::Performance
      );
      assert!(state.read().await.profile_holds().is_empty());
      let missing = owner_proxy
        .call::<_, _, ()>("ReleaseProfile", &(cookie,))
        .await
        .unwrap_err();
      assert_eq!(
        fdo::Error::from(missing).name().as_str(),
        "org.freedesktop.DBus.Error.InvalidArgs"
      );
      let cookie: u32 = owner_proxy
        .call("HoldProfile", &("power-saver", "battery", "test"))
        .await
        .unwrap();
      let mut released =
        owner_proxy.receive_signal("ProfileReleased").await.unwrap();
      owner_proxy
        .set_property("ActiveProfile", "balanced")
        .await
        .unwrap();
      let message = released.next().await.unwrap();
      assert_eq!(message.body().deserialize::<(u32,)>().unwrap(), (cookie,));
      assert!(state.read().await.profile_holds().is_empty());
      assert_eq!(state.read().await.active_profile(), PowerProfile::Balanced);
    })
    .await
    .expect("PPD mutation calls timed out");
  }
}
