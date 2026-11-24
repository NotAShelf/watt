use std::{
  collections::HashMap,
  sync::Arc,
};

use dbus::{
  Message,
  arg::Variant,
  nonblock::{
    Proxy,
    SyncConnection,
  },
};
use misstep::{
  OptionExt as _,
  ResultExt as _,
  bail,
};

use super::connection::CALL_TIMEOUT;

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

pub(crate) struct Caller {
  pub sender: String,
  flags:      u32,
}

impl Caller {
  pub(crate) fn from_message(message: &Message) -> misstep::Result<Self> {
    let sender = message
      .sender()
      .context("missing D-Bus sender")?
      .to_string();
    let mut flags = 0;
    message
      .marshal::<(), _>(|bytes| {
        flags = u32::from(bytes.get(2).is_some_and(|byte| byte & 4 != 0));
        Ok(())
      })
      .expect("infallible D-Bus message inspection");
    Ok(Self { sender, flags })
  }
}

pub(crate) async fn authorize(
  connection: Arc<SyncConnection>,
  caller: Caller,
  action: &str,
) -> misstep::Result<()> {
  let authority =
    Proxy::new(AUTHORITY, AUTHORITY_PATH, CALL_TIMEOUT, connection);
  let mut subject_details = HashMap::new();
  subject_details.insert("name", Variant(caller.sender));
  let subject = ("system-bus-name", subject_details);
  let details: HashMap<&str, &str> = HashMap::new();
  let ((authorized, _challenge, _details),): ((
    bool,
    bool,
    HashMap<String, String>,
  ),) = authority
    .method_call(
      AUTHORITY_INTERFACE,
      "CheckAuthorization",
      (subject, action, details, caller.flags, ""),
    )
    .await
    .context("polkit authorization failed")?;
  if !authorized {
    bail!("D-Bus caller is not authorized for {action}");
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::{
    collections::HashMap,
    sync::{
      Arc,
      atomic::{
        AtomicU32,
        Ordering,
      },
    },
    time::Duration,
  };

  use dbus::{
    arg::PropMap,
    channel::MatchingReceiver as _,
    message::MatchRule,
  };
  use dbus_crossroads::Crossroads;
  use tokio::time::timeout;

  use super::*;
  use crate::dbus::test_support::TestBus;

  #[tokio::test]
  async fn authorization_decodes_polkit_result_and_fails_closed() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let service = bus.connect();
      let client = bus.connect();
      let flags = Arc::new(AtomicU32::new(u32::MAX));
      let observed_flags = flags.clone();
      let mut crossroads = Crossroads::new();
      let authority =
        crossroads.register(AUTHORITY_INTERFACE, move |builder| {
          builder.method(
            "CheckAuthorization",
            ("subject", "action", "details", "flags", "cancellation_id"),
            ("result",),
            move |_,
                  _,
                  (subject, action, _, value, _): (
              (String, PropMap),
              String,
              HashMap<String, String>,
              u32,
              String,
            )| {
              observed_flags.store(value, Ordering::SeqCst);
              assert_eq!(subject.0, "system-bus-name");
              assert_eq!(subject.1["name"].0.as_str().unwrap(), ":1.42");
              Ok(((
                action == "dev.notashelf.watt.set-profile",
                false,
                HashMap::<String, String>::new(),
              ),))
            },
          );
        });
      crossroads.insert(AUTHORITY_PATH, &[authority], ());
      service.connection.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |message, connection| {
          crossroads.handle_message(message, connection).unwrap();
          true
        }),
      );
      service
        .connection
        .request_name(AUTHORITY, false, false, true)
        .await
        .unwrap();

      authorize(
        client.connection.clone(),
        Caller {
          sender: ":1.42".into(),
          flags:  0,
        },
        "dev.notashelf.watt.set-profile",
      )
      .await
      .unwrap();
      assert_eq!(flags.load(Ordering::SeqCst), 0);
      authorize(
        client.connection.clone(),
        Caller {
          sender: ":1.42".into(),
          flags:  1,
        },
        "dev.notashelf.watt.set-profile",
      )
      .await
      .unwrap();
      assert_eq!(flags.load(Ordering::SeqCst), 1);
      let denied = authorize(
        client.connection.clone(),
        Caller {
          sender: ":1.42".into(),
          flags:  0,
        },
        "dev.notashelf.watt.hold-profile",
      )
      .await
      .unwrap_err();
      assert!(denied.to_string().contains("not authorized"), "{denied}");
      service.connection.release_name(AUTHORITY).await.unwrap();
      let unavailable = authorize(
        client.connection.clone(),
        Caller {
          sender: ":1.42".into(),
          flags:  0,
        },
        "dev.notashelf.watt.set-profile",
      )
      .await
      .unwrap_err();
      assert!(
        unavailable
          .to_string()
          .contains("polkit authorization failed"),
        "{unavailable}"
      );
    })
    .await
    .expect("authorization calls timed out");
  }
}
