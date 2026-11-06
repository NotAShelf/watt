use std::{
  io::{
    self,
    Write,
  },
  sync::Arc,
};

use dbus::{
  Error,
  arg::{
    AppendAll,
    ArgType,
    PropMap,
    ReadAll,
    RefArg,
  },
  message::MatchRule,
  nonblock::{
    Proxy,
    SyncConnection,
    stdintf::org_freedesktop_dbus::Properties as _,
  },
};
use futures_util::StreamExt;
use misstep::{
  ResultExt as _,
  bail,
  ensure,
};
use serde::Serialize;
use serde_json::{
  Map,
  Number,
  Value,
  json,
};

use super::{
  connection::Connection,
  ppd,
  watt,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
  Human,
  Json,
}

impl From<bool> for OutputFormat {
  fn from(json: bool) -> Self {
    if json { Self::Json } else { Self::Human }
  }
}

type DiagnosticRow = (String, String, u16, String, String);

struct WattClient {
  connection: Connection,
}

impl WattClient {
  fn connect() -> misstep::Result<Self> {
    Ok(Self {
      connection: Connection::system()
        .context("failed to connect to the system D-Bus")?,
    })
  }

  fn proxy(&self) -> Proxy<'_, Arc<SyncConnection>> {
    self.connection.proxy("dev.notashelf.Watt", watt::PATH)
  }

  async fn call<R: ReadAll + 'static>(
    &self,
    method: &str,
    args: impl AppendAll,
  ) -> Result<R, Error> {
    self
      .proxy()
      .method_call(watt::INTERFACE, method, args)
      .await
  }
}

pub async fn verify_configuration(
  contents: &str,
  hardware: bool,
) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (diagnostics,): (Vec<DiagnosticRow>,) = client
    .call("VerifyConfiguration", (contents, hardware))
    .await
    .context("daemon configuration verification failed")?;
  let mut invalid = false;
  for (severity, rule, priority, field, message) in diagnostics {
    eprintln!(
      "{severity}: rule {rule:?} (priority {priority}), {field}: {message}"
    );
    invalid |= severity == "error";
  }
  ensure!(!invalid, "configuration verification found errors");
  Ok(())
}

pub async fn loaded_config() -> misstep::Result<String> {
  let client = WattClient::connect()?;
  let (config,): (String,) = client
    .call("GetConfig", ())
    .await
    .context("failed to get the loaded config")?;
  Ok(config)
}

pub async fn reload() -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (): () = client
    .call("Reload", ())
    .await
    .context("daemon configuration reload failed")?;
  Ok(())
}

pub async fn print_errors(format: OutputFormat) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (errors,): (Vec<String>,) = client
    .call("GetRecentErrors", ())
    .await
    .context("failed to get daemon errors")?;
  if format == OutputFormat::Json {
    return print_payload(errors, format);
  }
  let mut stdout = io::stdout().lock();
  for error in errors {
    writeln!(stdout, "{error}")?;
  }
  Ok(())
}

pub async fn print_status(format: OutputFormat) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (status,): (PropMap,) = client
    .call("GetStatus", ())
    .await
    .context("failed to get daemon status")?;
  print_payload(prop_map_json(&status), format)
}

pub async fn print_configured_rules(
  format: OutputFormat,
) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (rules,): (Vec<(String, u16)>,) = client
    .call("GetRules", ())
    .await
    .context("failed to get daemon rules")?;
  if format == OutputFormat::Json {
    let rules: Vec<_> = rules
      .into_iter()
      .map(|(name, priority)| json!({"name": name, "priority": priority}))
      .collect();
    return print_payload(rules, format);
  }
  let mut stdout = io::stdout().lock();
  for (name, priority) in rules {
    writeln!(stdout, "{priority}\t{name}")?;
  }
  Ok(())
}

pub async fn print_capabilities(format: OutputFormat) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (cpus,): (Vec<PropMap>,) = client
    .call("GetCpuCapabilities", ())
    .await
    .context("failed to get CPU capabilities")?;
  let (devices,): (Vec<PropMap>,) = client
    .call("GetDeviceCapabilities", ())
    .await
    .context("failed to get device capabilities")?;
  print_payload(
    json!({
      "cpus": cpus.iter().map(prop_map_json).collect::<Vec<_>>(),
      "devices": devices.iter().map(prop_map_json).collect::<Vec<_>>(),
    }),
    format,
  )
}

pub async fn print_settings(format: OutputFormat) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (settings,): (Vec<PropMap>,) = client
    .call("GetAppliedSettingDetails", ())
    .await
    .context("failed to get applied settings")?;
  print_payload(
    settings.iter().map(prop_map_json).collect::<Vec<_>>(),
    format,
  )
}

pub async fn print_polling(format: OutputFormat) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let (polling,): (PropMap,) = client
    .call("GetPollingDiagnostics", ())
    .await
    .context("failed to get polling diagnostics")?;
  print_payload(prop_map_json(&polling), format)
}

pub async fn get_profile(format: OutputFormat) -> misstep::Result<()> {
  let connection =
    Connection::system().context("failed to connect to the system D-Bus")?;
  let proxy = connection.proxy("net.hadess.PowerProfiles", ppd::PATH);
  let profile: String = proxy
    .get(ppd::INTERFACE, "ActiveProfile")
    .await
    .context("failed to get active profile")?;
  if format == OutputFormat::Json {
    return print_payload(json!({"profile": profile}), format);
  }
  writeln!(io::stdout().lock(), "{profile}")?;
  Ok(())
}

pub async fn set_profile(profile: &str) -> misstep::Result<()> {
  let connection =
    Connection::system().context("failed to connect to the system D-Bus")?;
  connection
    .proxy("net.hadess.PowerProfiles", ppd::PATH)
    .set(ppd::INTERFACE, "ActiveProfile", profile)
    .await
    .context("failed to set active profile")?;
  Ok(())
}

fn print_payload(
  payload: impl Serialize,
  format: OutputFormat,
) -> misstep::Result<()> {
  let mut stdout = io::stdout().lock();
  match format {
    OutputFormat::Human => serde_json::to_writer_pretty(&mut stdout, &payload)?,
    OutputFormat::Json => serde_json::to_writer(&mut stdout, &payload)?,
  }
  writeln!(stdout)?;
  Ok(())
}

fn prop_map_json(properties: &PropMap) -> Value {
  Value::Object(
    properties
      .iter()
      .map(|(key, value)| (key.clone(), refarg_json(value.0.as_ref())))
      .collect(),
  )
}

fn refarg_json(value: &dyn RefArg) -> Value {
  match value.arg_type() {
    ArgType::Boolean => Value::Bool(value.as_i64().unwrap_or_default() != 0),
    ArgType::Byte
    | ArgType::Int16
    | ArgType::Int32
    | ArgType::Int64
    | ArgType::UnixFd => {
      Value::Number(value.as_i64().unwrap_or_default().into())
    },
    ArgType::UInt16 | ArgType::UInt32 | ArgType::UInt64 => {
      Value::Number(value.as_u64().unwrap_or_default().into())
    },
    ArgType::Double => {
      Number::from_f64(value.as_f64().unwrap_or_default())
        .map_or(Value::Null, Value::Number)
    },
    ArgType::String | ArgType::ObjectPath | ArgType::Signature => {
      Value::String(value.as_str().unwrap_or_default().to_owned())
    },
    ArgType::Variant => {
      value
        .as_iter()
        .and_then(|mut values| values.next().map(refarg_json))
        .unwrap_or(Value::Null)
    },
    ArgType::Array if value.signature().to_string().starts_with("a{") => {
      let mut object = Map::new();
      if let Some(mut values) = value.as_iter() {
        while let Some(key) = values.next() {
          let Some(value) = values.next() else { break };
          if let Some(key) = key.as_str() {
            object.insert(key.to_owned(), refarg_json(value));
          }
        }
      }
      Value::Object(object)
    },
    ArgType::Array | ArgType::Struct | ArgType::DictEntry => {
      Value::Array(
        value
          .as_iter()
          .map(|values| values.map(refarg_json).collect())
          .unwrap_or_default(),
      )
    },
    _ => Value::Null,
  }
}

pub async fn print_active_rules(
  watch: bool,
  format: OutputFormat,
) -> misstep::Result<()> {
  let client = WattClient::connect()?;
  let signals = if watch {
    let mut rule =
      MatchRule::new_signal(watt::INTERFACE, "AppliedRulesChanged");
    rule.sender = Some("dev.notashelf.Watt".into());
    rule.path = Some(watt::PATH.into());
    Some(
      client
        .connection
        .inner()
        .add_match(rule)
        .await
        .context("failed to watch active rules")?
        .msg_stream(),
    )
  } else {
    None
  };
  let (mut rules,): (Vec<String>,) = client
    .call("GetAppliedRules", ())
    .await
    .context("failed to get active rules")?;
  print_rules(&rules, false, format)?;

  let Some((_signal_match, mut signals)) = signals else {
    return Ok(());
  };
  while signals.next().await.is_some() {
    let (next,): (Vec<String>,) = client
      .call("GetAppliedRules", ())
      .await
      .context("failed to get active rules")?;
    if next != rules {
      print_rules(&next, true, format)?;
      rules = next;
    }
  }
  bail!("connection to the watt daemon closed")
}

fn print_rules(
  rules: &[String],
  separator: bool,
  format: OutputFormat,
) -> misstep::Result<()> {
  if format == OutputFormat::Json {
    return print_payload(rules, format);
  }
  let stdout = io::stdout();
  let mut stdout = stdout.lock();
  if separator {
    writeln!(stdout, "--")?;
  }
  for rule in rules {
    writeln!(stdout, "{rule}")?;
  }
  stdout.flush()?;
  Ok(())
}
