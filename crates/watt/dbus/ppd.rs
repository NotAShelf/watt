use std::{
  future::Future,
  marker::PhantomData,
  str::FromStr as _,
  sync::Arc,
};

use dbus::{
  Message,
  arg::{
    AppendAll,
    PropMap,
    RefArg,
    Variant,
  },
  channel::Sender as _,
  nonblock::{
    Proxy,
    SyncConnection,
  },
};
use dbus_crossroads::{
  Crossroads,
  IfaceToken,
  MethodErr,
};
use tokio::sync::RwLock;

use super::{
  auth::{
    Caller,
    authorize,
  },
  connection::CALL_TIMEOUT,
};
use crate::{
  profile::{
    PowerProfile,
    ReleaseHoldError,
  },
  system::DaemonState,
};

pub(crate) const PATH: &str = "/net/hadess/PowerProfiles";
pub(crate) const INTERFACE: &str = "net.hadess.PowerProfiles";
const DRIVER_NAME: &str = "watt";
const CPU_DRIVER_NAME: &str = "watt";

#[derive(Clone)]
pub struct PowerProfilesInterface {
  state: Arc<RwLock<DaemonState>>,
}

impl PowerProfilesInterface {
  pub fn new(state: Arc<RwLock<DaemonState>>) -> Self {
    Self { state }
  }

  async fn add_owner_hold(
    &self,
    owner: &str,
    profile: PowerProfile,
    reason: String,
    application_id: String,
    is_connected: impl Future<Output = Result<bool, MethodErr>>,
  ) -> Result<u32, MethodErr> {
    let disconnected = || access_denied("D-Bus caller disconnected");
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

  async fn set_active_profile(
    &self,
    connection: Arc<SyncConnection>,
    message: Message,
    profile: String,
  ) -> Result<(), MethodErr> {
    let caller = Caller::from_message(&message)
      .map_err(|error| access_denied(error.to_string()))?;
    authorize(connection.clone(), caller, "dev.notashelf.watt.set-profile")
      .await
      .map_err(|error| access_denied(error.to_string()))?;
    let profile = PowerProfile::from_str(&profile).map_err(|_| {
      invalid_args(format!(
        "invalid profile: {profile}, valid: performance, balanced, power-saver"
      ))
    })?;
    let released = self
      .state
      .write()
      .await
      .set_active_profile(profile)
      .map_err(|error| MethodErr::failed(&error))?;
    self.notify_holds_changed(&connection, &released);
    log::info!(
      "D-Bus: active profile set to {profile}",
      profile = profile.as_str()
    );
    Ok(())
  }

  async fn hold_profile(
    &self,
    connection: Arc<SyncConnection>,
    message: Message,
    profile: String,
    reason: String,
    application_id: String,
  ) -> Result<u32, MethodErr> {
    let caller = Caller::from_message(&message)
      .map_err(|error| access_denied(error.to_string()))?;
    let sender = caller.sender.clone();
    authorize(
      connection.clone(),
      caller,
      "dev.notashelf.watt.hold-profile",
    )
    .await
    .map_err(|error| access_denied(error.to_string()))?;
    let profile = PowerProfile::from_str(&profile)
      .map_err(|_| invalid_args(format!("invalid profile: {profile}")))?;
    if profile == PowerProfile::Balanced {
      return Err(invalid_args(
        "profile holds only support performance and power-saver",
      ));
    }

    let bus = Proxy::new(
      "org.freedesktop.DBus",
      "/org/freedesktop/DBus",
      CALL_TIMEOUT,
      connection.clone(),
    );
    let owner = sender.clone();
    let is_connected = async move {
      let (connected,): (bool,) = bus
        .method_call("org.freedesktop.DBus", "NameHasOwner", (owner,))
        .await
        .map_err(|error| MethodErr::failed(&error))?;
      Ok(connected)
    };
    let cookie = self
      .add_owner_hold(&sender, profile, reason, application_id, is_connected)
      .await?;
    log::info!("D-Bus profile hold added, cookie={cookie}");
    self.notify_holds_changed(&connection, &[]);
    Ok(cookie)
  }

  async fn release_profile(
    &self,
    connection: Arc<SyncConnection>,
    message: Message,
    cookie: u32,
  ) -> Result<(), MethodErr> {
    let caller = Caller::from_message(&message)
      .map_err(|error| access_denied(error.to_string()))?;
    let owner = caller.sender.clone();
    let released = self
      .state
      .write()
      .await
      .release_profile_hold_for(cookie, Some(&owner));
    match released {
      Err(ReleaseHoldError::NotOwner) => {
        authorize(
          connection.clone(),
          caller,
          "dev.notashelf.watt.release-any-profile-hold",
        )
        .await
        .map_err(|error| access_denied(error.to_string()))?;
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
        ReleaseHoldError::NotOwner => access_denied(error.to_string()),
        ReleaseHoldError::NotFound(_) => invalid_args(error.to_string()),
      }
    })?;
    log::info!("D-Bus profile hold released, cookie={cookie}");
    self.notify_holds_changed(&connection, &[cookie]);
    Ok(())
  }

  pub(crate) fn notify_holds_changed(
    &self,
    connection: &Arc<SyncConnection>,
    released: &[u32],
  ) {
    self.emit_changed(connection, "ActiveProfile");
    self.emit_changed(connection, "ActiveProfileHolds");
    self.emit_changed(connection, "PerformanceInhibited");
    for &cookie in released {
      if let Err(error) =
        send_signal(connection, PATH, INTERFACE, "ProfileReleased", (cookie,))
      {
        log::warn!("failed to emit ProfileReleased signal: {error}");
      }
    }
  }

  pub(crate) fn emit_changed(
    &self,
    connection: &Arc<SyncConnection>,
    property: &str,
  ) {
    if let Err(error) =
      properties_changed(connection, PATH, INTERFACE, property)
    {
      log::warn!("failed to emit {property} change signal: {error}");
    }
  }
}

pub(crate) fn register(
  crossroads: &mut Crossroads,
  connection: Arc<SyncConnection>,
) -> IfaceToken<PowerProfilesInterface> {
  crossroads.register(INTERFACE, move |builder| {
    builder
      .property::<String, _>("ActiveProfile")
      .get_async(|mut context, data: &mut PowerProfilesInterface| {
        let state = data.state.clone();
        async move {
          let profile = state.read().await.active_profile().as_str().to_owned();
          context.reply(Ok(profile))
        }
      })
      .set_async({
        let connection = connection.clone();
        move |mut context, data: &mut PowerProfilesInterface, profile| {
          let data = data.clone();
          let connection = connection.clone();
          let message = context.message().unwrap().duplicate().unwrap();
          async move {
            let result =
              data.set_active_profile(connection, message, profile).await;
            context.reply_noemit(result);
            PhantomData::<Option<String>>
          }
        }
      });

    builder
      .property::<Vec<PropMap>, _>("Profiles")
      .get(|_, _: &mut PowerProfilesInterface| Ok(profiles()));
    builder
      .property::<Vec<String>, _>("Actions")
      .get(|_, _: &mut PowerProfilesInterface| Ok(Vec::new()));
    builder
      .property::<String, _>("PerformanceDegraded")
      .get_async(|mut context, data: &mut PowerProfilesInterface| {
        let state = data.state.clone();
        async move {
          let degraded = state
            .read()
            .await
            .performance_degraded()
            .unwrap_or_default()
            .to_owned();
          context.reply(Ok(degraded))
        }
      });
    builder
      .property::<String, _>("PerformanceInhibited")
      .get_async(|mut context, data: &mut PowerProfilesInterface| {
        let state = data.state.clone();
        async move {
          let reason = state
            .read()
            .await
            .profile_holds()
            .first()
            .map_or_else(String::new, |hold| hold.reason.clone());
          context.reply(Ok(reason))
        }
      });
    builder
      .property::<Vec<PropMap>, _>("ActiveProfileHolds")
      .get_async(|mut context, data: &mut PowerProfilesInterface| {
        let state = data.state.clone();
        async move {
          let holds = state
            .read()
            .await
            .profile_holds()
            .into_iter()
            .map(|hold| {
              prop_map([
                ("Profile", boxed(hold.profile.as_str().to_owned())),
                ("Reason", boxed(hold.reason)),
                ("ApplicationId", boxed(hold.application_id)),
              ])
            })
            .collect();
          context.reply(Ok(holds))
        }
      });

    builder.method_with_cr_async(
      "HoldProfile",
      ("profile", "reason", "application_id"),
      ("cookie",),
      {
        let connection = connection.clone();
        move |mut context, crossroads, args: (String, String, String)| {
          let data = crossroads
            .data_mut::<PowerProfilesInterface>(context.path())
            .unwrap()
            .clone();
          let connection = connection.clone();
          let message = context.message().duplicate().unwrap();
          async move {
            let (profile, reason, application_id) = args;
            let result = data
              .hold_profile(
                connection,
                message,
                profile,
                reason,
                application_id,
              )
              .await
              .map(|cookie| (cookie,));
            context.reply(result)
          }
        }
      },
    );
    builder.method_with_cr_async("ReleaseProfile", ("cookie",), (), {
      let connection = connection.clone();
      move |mut context, crossroads, (cookie,): (u32,)| {
        let data = crossroads
          .data_mut::<PowerProfilesInterface>(context.path())
          .unwrap()
          .clone();
        let connection = connection.clone();
        let message = context.message().duplicate().unwrap();
        async move {
          let result = data.release_profile(connection, message, cookie).await;
          context.reply(result)
        }
      }
    });
    builder.signal::<(u32,), _>("ProfileReleased", ("cookie",));
  })
}

fn profiles() -> Vec<PropMap> {
  PowerProfile::all()
    .iter()
    .map(|profile| {
      prop_map([
        ("Profile", boxed(profile.as_str().to_owned())),
        ("Driver", boxed(DRIVER_NAME.to_owned())),
        ("CpuDriver", boxed(CPU_DRIVER_NAME.to_owned())),
      ])
    })
    .collect()
}

pub(crate) fn boxed(
  value: impl RefArg + 'static,
) -> Variant<Box<dyn RefArg + 'static>> {
  Variant(Box::new(value))
}

pub(crate) fn prop_map<const N: usize>(
  entries: [(&str, Variant<Box<dyn RefArg + 'static>>); N],
) -> PropMap {
  entries
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

pub(crate) fn properties_changed(
  connection: &Arc<SyncConnection>,
  path: &str,
  interface: &str,
  property: &str,
) -> Result<(), dbus::Error> {
  send_signal(
    connection,
    path,
    "org.freedesktop.DBus.Properties",
    "PropertiesChanged",
    (interface.to_owned(), PropMap::new(), vec![
      property.to_owned(),
    ]),
  )
}

pub(crate) fn send_signal<A: AppendAll>(
  connection: &Arc<SyncConnection>,
  path: &str,
  interface: &str,
  member: &str,
  args: A,
) -> Result<(), dbus::Error> {
  let mut message = Message::new_signal(path, interface, member)
    .map_err(|error| dbus::Error::new_failed(&error))?;
  message.append_all(args);
  connection
    .send(message)
    .map(|_| ())
    .map_err(|()| dbus::Error::new_failed("failed to queue D-Bus signal"))
}

fn access_denied(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.AccessDenied", message.into()).into()
}

fn invalid_args(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.InvalidArgs", message.into()).into()
}
