use std::{
  collections::HashMap,
  future::Future,
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

use super::auth::authorize;
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

  async fn add_owner_hold(
    &self,
    owner: &str,
    profile: PowerProfile,
    reason: String,
    application_id: String,
    is_connected: impl Future<Output = fdo::Result<bool>>,
  ) -> fdo::Result<u32> {
    let disconnected =
      || fdo::Error::AccessDenied("D-Bus caller disconnected".into());
    let check = self.state.write().await.begin_profile_owner_check(owner);
    if !is_connected.await? {
      return Err(disconnected());
    }
    self
      .state
      .write()
      .await
      .add_checked_profile_hold(check, profile, reason, application_id)
      .ok_or_else(disconnected)
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
    #[zbus(header)] header: Option<Header<'_>>,
  ) -> zbus::Result<()> {
    let header = header
      .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus header".into()))?;
    authorize(connection, &header, "dev.notashelf.watt.set-profile")
      .await
      .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    let profile = PowerProfile::from_str(profile).map_err(|_| {
      fdo::Error::InvalidArgs(format!(
        "invalid profile: {profile}, valid: performance, balanced, power-saver"
      ))
    })?;
    let signal_emitter =
      SignalEmitter::new(connection, "/net/hadess/PowerProfiles")?;

    let released = self
      .state
      .write()
      .await
      .set_active_profile(profile)
      .map_err(|error| fdo::Error::Failed(error.to_string()))?;
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
    authorize(
      signal_emitter.connection(),
      &header,
      "dev.notashelf.watt.hold-profile",
    )
    .await
    .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
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

    let bus = fdo::DBusProxy::new(signal_emitter.connection()).await?;
    let cookie = self
      .add_owner_hold(
        sender.as_str(),
        profile,
        reason,
        application_id,
        bus.name_has_owner(sender.clone().into()),
      )
      .await?;

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
    let released = self
      .state
      .write()
      .await
      .release_profile_hold_for(cookie, Some(owner.as_str()));
    match released {
      Err(ReleaseHoldError::NotOwner) => {
        authorize(
          signal_emitter.connection(),
          &header,
          "dev.notashelf.watt.release-any-profile-hold",
        )
        .await
        .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
        self
          .state
          .write()
          .await
          .release_profile_hold_for(cookie, None)
      },
      released => released,
    }
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
  use std::{
    sync::atomic::{
      AtomicBool,
      Ordering,
    },
    time::Duration,
  };

  use futures_util::StreamExt;
  use tokio::{
    sync::Notify,
    task::yield_now,
    time::timeout,
  };
  use zbus::{
    DBusError as _,
    Message,
    Proxy,
    connection,
    zvariant::OwnedValue,
  };

  use super::*;
  use crate::dbus::test_support::{
    AuthorizationResult,
    TestBus,
  };

  #[tokio::test]
  async fn disconnect_during_owner_query_rejects_late_reply() {
    let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
    let ppd = PowerProfilesInterface::new(state.clone());
    let started = Notify::new();
    let proceed = Notify::new();
    let request = ppd.add_owner_hold(
      ":1.42",
      PowerProfile::Performance,
      "render".into(),
      "test".into(),
      async {
        started.notify_one();
        proceed.notified().await;
        Ok(true)
      },
    );
    let cleanup = async {
      started.notified().await;
      state.write().await.release_profile_owner(":1.42");
      proceed.notify_one();
    };
    let (result, ()) = timeout(Duration::from_secs(2), async {
      tokio::join!(request, cleanup)
    })
    .await
    .expect("owner query blocked disconnect cleanup");
    assert!(matches!(result, Err(fdo::Error::AccessDenied(_))));
    assert!(state.read().await.profile_holds().is_empty());
  }

  struct PolicyAuthority(Arc<AtomicBool>);

  #[interface(name = "org.freedesktop.PolicyKit1.Authority")]
  impl PolicyAuthority {
    fn check_authorization(
      &self,
      _subject: (String, HashMap<String, OwnedValue>),
      action: &str,
      _details: HashMap<String, String>,
      _flags: u32,
      _cancellation_id: &str,
    ) -> AuthorizationResult {
      assert!(
        include_str!("../../dbus/dev.notashelf.watt.policy")
          .contains(&format!("\"{action}\"")),
        "{action} is not declared in the polkit policy"
      );
      AuthorizationResult {
        authorized: self.0.load(Ordering::SeqCst),
        challenge:  false,
        details:    HashMap::new(),
      }
    }
  }

  #[tokio::test]
  async fn exported_mutations_enforce_authorization_and_hold_ownership() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let allowed = Arc::new(AtomicBool::new(false));
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      let _service = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.PolicyKit1")
        .unwrap()
        .name("net.hadess.PowerProfiles")
        .unwrap()
        .serve_at(
          "/org/freedesktop/PolicyKit1/Authority",
          PolicyAuthority(allowed.clone()),
        )
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
      let denied = owner_proxy
        .set_property("ActiveProfile", "performance")
        .await
        .unwrap_err();
      assert!(matches!(denied, fdo::Error::AccessDenied(_)), "{denied:?}");
      let denied = owner_proxy
        .call::<_, _, u32>("HoldProfile", &("performance", "render", "test"))
        .await
        .unwrap_err();
      assert_eq!(
        fdo::Error::from(denied).name().as_str(),
        "org.freedesktop.DBus.Error.AccessDenied"
      );
      assert_eq!(state.read().await.active_profile(), PowerProfile::Balanced);
      assert!(state.read().await.profile_holds().is_empty());
      allowed.store(true, Ordering::SeqCst);
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
      allowed.store(false, Ordering::SeqCst);
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
      allowed.store(true, Ordering::SeqCst);
      let mut released =
        owner_proxy.receive_signal("ProfileReleased").await.unwrap();
      let cookie: u32 = owner_proxy
        .call("HoldProfile", &("power-saver", "battery", "test"))
        .await
        .unwrap();
      other_proxy
        .call::<_, _, ()>("ReleaseProfile", &(cookie,))
        .await
        .unwrap();
      let message = released.next().await.unwrap();
      assert_eq!(message.body().deserialize::<(u32,)>().unwrap(), (cookie,));
      assert!(state.read().await.profile_holds().is_empty());
      assert_eq!(
        state.read().await.active_profile(),
        PowerProfile::Performance
      );
      let cookie: u32 = owner_proxy
        .call("HoldProfile", &("power-saver", "battery", "test"))
        .await
        .unwrap();
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

  struct PendingAuthority {
    started: Arc<Notify>,
    proceed: Arc<Notify>,
  }

  #[interface(name = "org.freedesktop.PolicyKit1.Authority")]
  impl PendingAuthority {
    async fn check_authorization(
      &self,
      _subject: (String, HashMap<String, OwnedValue>),
      _action: &str,
      _details: HashMap<String, String>,
      _flags: u32,
      _cancellation_id: &str,
    ) -> AuthorizationResult {
      self.started.notify_one();
      self.proceed.notified().await;
      AuthorizationResult {
        authorized: true,
        challenge:  false,
        details:    HashMap::new(),
      }
    }
  }

  #[tokio::test]
  async fn hold_checks_owner_liveness_after_authorization() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let started = Arc::new(Notify::new());
      let proceed = Arc::new(Notify::new());
      let connection = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.PolicyKit1")
        .unwrap()
        .serve_at("/org/freedesktop/PolicyKit1/Authority", PendingAuthority {
          started: started.clone(),
          proceed: proceed.clone(),
        })
        .unwrap()
        .build()
        .await
        .unwrap();
      let owner = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      let ppd = PowerProfilesInterface::new(state.clone());
      let message =
        Message::method_call("/net/hadess/PowerProfiles", "HoldProfile")
          .unwrap()
          .sender(owner.unique_name().unwrap())
          .unwrap()
          .build(&())
          .unwrap();
      let emitter =
        SignalEmitter::new(&connection, "/net/hadess/PowerProfiles").unwrap();
      proceed.notify_one();
      let cookie = ppd
        .hold_profile(
          emitter.clone(),
          message.header(),
          "performance".into(),
          "render".into(),
          "test".into(),
        )
        .await
        .unwrap();
      started.notified().await;
      assert_eq!(
        state.read().await.active_profile(),
        PowerProfile::Performance
      );
      ppd
        .release_profile(emitter.clone(), message.header(), cookie)
        .await
        .unwrap();
      let request = ppd.hold_profile(
        emitter,
        message.header(),
        "performance".into(),
        "render".into(),
        "test".into(),
      );
      let disconnect = async {
        started.notified().await;
        owner.close().await.unwrap();
        let proxy = fdo::DBusProxy::new(&connection).await.unwrap();
        // Wait for the bus to observe the close before releasing authorization.
        while proxy
          .name_has_owner(message.header().sender().unwrap().clone().into())
          .await
          .unwrap()
        {
          yield_now().await;
        }
        proceed.notify_one();
      };
      let (result, ()) = tokio::join!(request, disconnect);
      assert!(
        matches!(result, Err(fdo::Error::AccessDenied(_))),
        "{result:?}"
      );
      assert!(state.read().await.profile_holds().is_empty());
    })
    .await
    .expect("authorization race test timed out");
  }
}
