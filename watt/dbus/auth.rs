use std::collections::HashMap;

use anyhow::{
  Context,
  bail,
};
use zbus::{
  Connection,
  Proxy,
  message::{
    Flags,
    Header,
  },
  zvariant::Value,
};

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

pub(crate) async fn authorize(
  connection: &Connection,
  header: &Header<'_>,
  action: &str,
) -> anyhow::Result<()> {
  let sender = header.sender().context("missing D-Bus sender")?;
  let flags = u32::from(
    header
      .primary()
      .flags()
      .contains(Flags::AllowInteractiveAuth),
  );
  let authority =
    Proxy::new(connection, AUTHORITY, AUTHORITY_PATH, AUTHORITY_INTERFACE)
      .await?;
  let mut subject_details = HashMap::new();
  subject_details.insert("name".to_owned(), Value::from(sender.as_str()));
  let subject = ("system-bus-name", subject_details);
  let details: HashMap<&str, &str> = HashMap::new();
  let ((authorized, _challenge, _details),): ((
    bool,
    bool,
    HashMap<String, String>,
  ),) = authority
    .call("CheckAuthorization", &(subject, action, details, flags, ""))
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
    sync::{
      Arc,
      atomic::{
        AtomicU32,
        Ordering,
      },
    },
    time::Duration,
  };

  use tokio::time::timeout;
  use zbus::{
    Message,
    connection,
    interface,
    zvariant::OwnedValue,
  };

  use super::*;
  use crate::dbus::test_support::{
    AuthorizationResult,
    TestBus,
  };

  struct Authority(Arc<AtomicU32>);

  #[interface(name = "org.freedesktop.PolicyKit1.Authority")]
  impl Authority {
    fn check_authorization(
      &self,
      subject: (String, HashMap<String, OwnedValue>),
      action: &str,
      _details: HashMap<String, String>,
      flags: u32,
      _cancellation_id: &str,
    ) -> AuthorizationResult {
      self.0.store(flags, Ordering::SeqCst);
      assert_eq!(subject.0, "system-bus-name");
      assert_eq!(subject.1["name"].downcast_ref::<&str>().unwrap(), ":1.42");
      AuthorizationResult {
        authorized: action == "dev.notashelf.watt.set-profile",
        challenge:  false,
        details:    HashMap::new(),
      }
    }
  }

  #[tokio::test]
  async fn authorization_decodes_polkit_result_and_fails_closed() {
    let bus = TestBus::new();
    let flags = Arc::new(AtomicU32::new(u32::MAX));
    timeout(Duration::from_secs(10), async {
      let service = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(AUTHORITY)
        .unwrap()
        .serve_at(AUTHORITY_PATH, Authority(flags.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
      let client = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let request =
        Message::method_call("/net/hadess/PowerProfiles", "HoldProfile")
          .unwrap()
          .sender(":1.42")
          .unwrap()
          .with_flags(Flags::NoAutoStart)
          .unwrap()
          .build(&())
          .unwrap();
      authorize(&client, &request.header(), "dev.notashelf.watt.set-profile")
        .await
        .unwrap();
      assert_eq!(flags.load(Ordering::SeqCst), 0);
      let interactive =
        Message::method_call("/net/hadess/PowerProfiles", "HoldProfile")
          .unwrap()
          .sender(":1.42")
          .unwrap()
          .with_flags(Flags::AllowInteractiveAuth)
          .unwrap()
          .build(&())
          .unwrap();
      authorize(
        &client,
        &interactive.header(),
        "dev.notashelf.watt.set-profile",
      )
      .await
      .unwrap();
      assert_eq!(flags.load(Ordering::SeqCst), 1);
      let denied = authorize(
        &client,
        &request.header(),
        "dev.notashelf.watt.hold-profile",
      )
      .await
      .unwrap_err();
      assert!(denied.to_string().contains("not authorized"), "{denied:#}");
      service.release_name(AUTHORITY).await.unwrap();
      let unavailable =
        authorize(&client, &request.header(), "dev.notashelf.watt.set-profile")
          .await
          .unwrap_err();
      assert!(
        unavailable
          .to_string()
          .contains("polkit authorization failed"),
        "{unavailable:#}"
      );
    })
    .await
    .expect("authorization calls timed out");
  }
}
