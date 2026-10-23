use std::{
  sync::Arc,
  time::Duration,
};

use futures_util::StreamExt as _;
use tokio::{
  sync::{
    RwLock,
    watch,
  },
  time::{
    interval,
    sleep,
  },
};
use zbus::{
  Connection,
  MatchRule,
  Message,
  MessageStream,
  connection,
  message::Type,
  object_server::SignalEmitter,
};

use crate::{
  dbus::{
    ppd::PowerProfilesInterface,
    watt::WattInterface,
  },
  fs::AppliedSetting,
  system::{
    DaemonState,
    DegradedCondition,
  },
};

pub async fn start(
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
) -> zbus::Result<()> {
  log::info!("starting D-Bus server...");

  let mut attempt: u32 = 0;
  loop {
    match try_start(state.clone(), applied_rules.clone()).await {
      Ok(()) => return Ok(()),
      Err(e) => {
        attempt += 1;
        log::error!("D-Bus server error on attempt {attempt}: {e}");

        if attempt >= 5 {
          log::error!("D-Bus server failed after {attempt} attempts, bailing");
          return Err(e);
        }

        let delay = Duration::from_secs(2 * attempt as u64);
        log::info!(
          "retrying D-Bus in {delay_secs}s",
          delay_secs = delay.as_secs()
        );
        sleep(delay).await;
      },
    }
  }
}

async fn try_start(
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
) -> zbus::Result<()> {
  let connection = connection::Builder::system()?.build().await?;
  run_connection(connection, state, applied_rules).await
}

pub(crate) async fn run_connection(
  connection: Connection,
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
) -> zbus::Result<()> {
  let result = async {
    let owner_changes = serve(&connection, state.clone()).await?;
    log::info!("D-Bus server started");
    let emitter = SignalEmitter::new(&connection, "/dev/notashelf/Watt")?;
    monitor(emitter, state.clone(), applied_rules, owner_changes).await
  }
  .await;
  // Close before clearing holds so pending requests fail their liveness check.
  let closed = connection.close().await;
  state.write().await.release_bus_holds();
  result.and(closed)
}

async fn subscribe_owners(
  connection: &Connection,
) -> zbus::Result<MessageStream> {
  let owner_rule = MatchRule::builder()
    .msg_type(Type::Signal)
    .sender("org.freedesktop.DBus")?
    .path("/org/freedesktop/DBus")?
    .interface("org.freedesktop.DBus")?
    .member("NameOwnerChanged")?
    .build();
  MessageStream::for_match_rule(owner_rule, connection, None).await
}

async fn serve(
  connection: &Connection,
  state: Arc<RwLock<DaemonState>>,
) -> zbus::Result<MessageStream> {
  // Subscribe before exposing methods that can create owner-bound holds.
  let owner_changes = subscribe_owners(connection).await?;
  connection
    .object_server()
    .at(
      "/net/hadess/PowerProfiles",
      PowerProfilesInterface::new(state.clone()),
    )
    .await?;
  connection
    .object_server()
    .at("/dev/notashelf/Watt", WattInterface::new(state))
    .await?;
  connection.request_name("net.hadess.PowerProfiles").await?;
  connection.request_name("dev.notashelf.Watt").await?;
  Ok(owner_changes)
}

async fn owner_changed(
  connection: &Connection,
  state: &Arc<RwLock<DaemonState>>,
  message: Message,
) -> zbus::Result<()> {
  let (name, _old_owner, new_owner) =
    message.body().deserialize::<(String, String, String)>()?;
  if !new_owner.is_empty() || !name.starts_with(':') {
    return Ok(());
  }
  let released = state.write().await.release_profile_owner(&name);
  if released.is_empty() {
    return Ok(());
  }
  log::info!(
    "removed {} profile hold(s) for disconnected owner {name}",
    released.len()
  );
  let emitter = SignalEmitter::new(connection, "/net/hadess/PowerProfiles")?;
  PowerProfilesInterface::new(state.clone())
    .notify_holds_changed(&emitter, &released)
    .await;
  Ok(())
}

struct StatusSnapshot {
  error_count:          usize,
  latest_error:         Option<String>,
  performance_degraded: Option<String>,
  settings:             Vec<AppliedSetting>,
  conditions:           Vec<DegradedCondition>,
}

impl StatusSnapshot {
  fn new(state: &DaemonState) -> Self {
    Self {
      error_count:          state.error_count(),
      latest_error:         state.latest_error().map(str::to_owned),
      performance_degraded: state.performance_degraded().map(str::to_owned),
      settings:             state.applied_settings(),
      conditions:           state.degraded_conditions().to_vec(),
    }
  }

  async fn notify_changes(
    &mut self,
    state: &Arc<RwLock<DaemonState>>,
    emitter: &SignalEmitter<'_>,
  ) -> zbus::Result<()> {
    let current = Self::new(&*state.read().await);
    let watt = WattInterface::new(state.clone());
    if current.error_count != self.error_count {
      watt.error_count_changed(emitter).await?;
    }
    if current.latest_error != self.latest_error {
      watt.latest_error_changed(emitter).await?;
    }
    if current.performance_degraded != self.performance_degraded {
      watt.performance_degraded_changed(emitter).await?;
      let ppd_emitter =
        SignalEmitter::new(emitter.connection(), "/net/hadess/PowerProfiles")?;
      PowerProfilesInterface::new(state.clone())
        .performance_degraded_changed(&ppd_emitter)
        .await?;
    }
    if current.conditions != self.conditions {
      watt.degraded_conditions_changed(emitter).await?;
    }
    if current.settings != self.settings {
      WattInterface::applied_settings_changed(emitter).await?;
    }
    *self = current;
    Ok(())
  }
}

async fn monitor(
  emitter: SignalEmitter<'_>,
  state: Arc<RwLock<DaemonState>>,
  mut applied_rules: watch::Receiver<Vec<String>>,
  mut owner_changes: MessageStream,
) -> zbus::Result<()> {
  let mut ticker = interval(Duration::from_secs(1));
  let mut previous = StatusSnapshot::new(&*state.read().await);
  loop {
    tokio::select! {
      _ = ticker.tick() => previous.notify_changes(&state, &emitter).await?,
      changed = applied_rules.changed() => {
        changed.map_err(|_| {
          zbus::Error::Failure("applied rules notification channel closed".into())
        })?;
        applied_rules.borrow_and_update();
        WattInterface::applied_rules_changed(&emitter).await?;
      },
      message = owner_changes.next() => {
        let message = message.ok_or_else(|| {
          zbus::Error::Failure("D-Bus owner notification stream closed".into())
        })??;
        owner_changed(emitter.connection(), &state, message).await?;
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use std::collections::{
    HashMap,
    HashSet,
  };

  use tokio::{
    task::yield_now,
    time::timeout,
  };
  use zbus::{
    Proxy,
    zvariant::OwnedValue,
  };

  use super::*;
  use crate::{
    dbus::test_support::TestBus,
    profile::PowerProfile,
  };

  #[tokio::test]
  async fn connection_loss_discards_bus_holds_without_changing_local_intent() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let connection = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let client = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      {
        let mut state = state.write().await;
        state.set_active_profile(PowerProfile::Performance).unwrap();
        state.add_profile_hold(
          PowerProfile::Performance,
          "local".into(),
          "test".into(),
        );
        state.add_profile_hold_for(
          PowerProfile::PowerSaver,
          "battery".into(),
          "test".into(),
          client.unique_name().unwrap().to_string(),
        );
      }
      let (_updates, applied_rules) = watch::channel(Vec::new());
      let closing = connection.clone();
      let disconnect = async {
        let proxy = zbus::fdo::DBusProxy::new(&client).await.unwrap();
        while !proxy
          .name_has_owner("dev.notashelf.Watt".try_into().unwrap())
          .await
          .unwrap()
        {
          yield_now().await;
        }
        closing.close().await.unwrap();
      };
      let (result, ()) = tokio::join!(
        run_connection(connection, state.clone(), applied_rules),
        disconnect
      );
      assert!(result.is_err());
      let mut state = state.write().await;
      let holds = state.profile_holds();
      assert_eq!(holds.len(), 1);
      assert!(holds[0].owner.is_empty());
      state.release_profile_hold(holds[0].cookie).unwrap();
      assert_eq!(state.active_profile(), PowerProfile::Performance);
    })
    .await
    .expect("D-Bus connection cleanup timed out");
  }

  #[tokio::test]
  async fn disconnected_owner_releases_only_its_holds_and_notifies_clients() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let connection = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let owner = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let observer = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      {
        let mut state = state.write().await;
        state.add_profile_hold_for(
          PowerProfile::PowerSaver,
          "battery".into(),
          "first".into(),
          owner.unique_name().unwrap().to_string(),
        );
        state.add_profile_hold_for(
          PowerProfile::Performance,
          "render".into(),
          "second".into(),
          observer.unique_name().unwrap().to_string(),
        );
      }
      let owner_changes = serve(&connection, state.clone()).await.unwrap();
      let proxy = Proxy::new(
        &observer,
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "org.freedesktop.DBus.Properties",
      )
      .await
      .unwrap();
      let mut changes =
        proxy.receive_signal("PropertiesChanged").await.unwrap();
      let (_updates, applied_rules) = watch::channel(Vec::new());
      let monitor_state = state.clone();
      let task = tokio::spawn(async move {
        let emitter =
          SignalEmitter::new(&connection, "/dev/notashelf/Watt").unwrap();
        monitor(emitter, monitor_state, applied_rules, owner_changes).await
      });
      owner.close().await.unwrap();
      let mut notified = HashSet::new();
      while notified.len() < 3 {
        let message = changes.next().await.unwrap();
        let (interface, values, _invalidated): (
          String,
          HashMap<String, OwnedValue>,
          Vec<String>,
        ) = message.body().deserialize().unwrap();
        assert_eq!(interface, "net.hadess.PowerProfiles");
        notified.extend(values.into_keys());
      }
      assert_eq!(
        notified,
        HashSet::from([
          "ActiveProfileHolds".into(),
          "ActiveProfile".into(),
          "PerformanceInhibited".into()
        ])
      );
      let state = state.read().await;
      assert_eq!(state.active_profile(), PowerProfile::Performance);
      let holds = state.profile_holds();
      assert_eq!(holds.len(), 1);
      assert_eq!(holds[0].owner, observer.unique_name().unwrap().as_str());
      task.abort();
      assert!(task.await.unwrap_err().is_cancelled());
    })
    .await
    .expect("owner cleanup did not notify clients");
  }
}
