pub mod verify;

mod eval;
mod rule;
pub use rule::EvaluatedRule;

#[cfg(test)] mod eval_tests;
#[cfg(test)] mod test_support;

#[cfg(feature = "metrics")] use std::net::IpAddr;
use std::{
  collections::{
    HashMap,
    HashSet,
    VecDeque,
  },
  fs,
  mem,
  path::Path,
  sync::Arc,
};

use anyhow::{
  Context,
  bail,
};
use serde::{
  Deserialize,
  Serialize,
};
use verify::Severity;

use crate::{
  audio,
  cpu,
  disk,
  gpu,
  power_supply,
  profile::PowerProfile,
  system::CpuLog,
  uncore,
  usb,
  vm,
};

type CpuDeltas = HashMap<Arc<cpu::Cpu>, cpu::Delta>;
type CpuEvalResult = anyhow::Result<(CpuDeltas, cpu::GlobalDelta)>;
type PowerSupplyDeltas =
  HashMap<Arc<power_supply::PowerSupply>, power_supply::Delta>;
type PowerSupplyEvalResult =
  anyhow::Result<(PowerSupplyDeltas, Option<String>)>;
type UncoreDeltas = HashMap<Arc<uncore::Uncore>, uncore::Delta>;
type DiskDeltas = HashMap<Arc<disk::Disk>, disk::Delta>;
type UsbDeltas = HashMap<Arc<usb::UsbDevice>, usb::Delta>;
type GpuDeltas = HashMap<Arc<gpu::Gpu>, gpu::Delta>;

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
  *value == T::default()
}

/// Find a power supply entry by name, excluding peripheral batteries.
fn find_battery<'a>(
  power_supplies: &'a HashSet<Arc<power_supply::PowerSupply>>,
  name: &str,
) -> Option<&'a power_supply::PowerSupply> {
  power_supplies
    .iter()
    .find(|ps| {
      ps.name == name && ps.type_ == "Battery" && !ps.is_from_peripheral
    })
    .map(|arc| arc.as_ref())
}

/// Find all non-peripheral battery power supplies.
pub fn find_batteries(
  power_supplies: &HashSet<Arc<power_supply::PowerSupply>>,
) -> Vec<&power_supply::PowerSupply> {
  // XXX: this repetition could be avoided if we extract the filter into
  // something like is_battery() also at the module level but I don't think
  // it's actually worth creating a function for.
  power_supplies
    .iter()
    .filter(|ps| ps.type_ == "Battery" && !ps.is_from_peripheral)
    .map(|arc| arc.as_ref())
    .collect()
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct CpusDelta {
  /// The CPUs to apply the changes to. When unspecified, will be applied to
  /// all CPUs.
  ///
  /// Type: `Vec<u32>`.
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Expression>,

  /// Set the CPU governor.
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub governor: Option<Expression>,

  /// Set CPU Energy Performance Preference (EPP).
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub energy_performance_preference: Option<Expression>,
  /// Set CPU Energy Performance Bias (EPB).
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub energy_perf_bias:              Option<Expression>,

  /// Set minimum CPU frequency in MHz.
  ///
  /// Type: positive number, in MHz with kHz precision.
  #[serde(skip_serializing_if = "is_default")]
  pub frequency_mhz_minimum: Option<Expression>,
  /// Set maximum CPU frequency in MHz.
  ///
  /// Type: positive number, in MHz with kHz precision.
  #[serde(skip_serializing_if = "is_default")]
  pub frequency_mhz_maximum: Option<Expression>,

  /// Set turbo boost behaviour. Has to be for all CPUs.
  ///
  /// Type: `bool`.
  #[serde(skip_serializing_if = "is_default")]
  pub turbo: Option<Expression>,

  /// Set Intel P-State minimum performance as a percentage.
  ///
  /// Type: `u8`.
  #[serde(skip_serializing_if = "is_default")]
  pub pstate_min_performance_percent: Option<Expression>,

  /// Set Intel P-State maximum performance as a percentage.
  ///
  /// Type: `u8`.
  #[serde(skip_serializing_if = "is_default")]
  pub pstate_max_performance_percent: Option<Expression>,

  /// Set the system-wide PM QoS DMA latency request in microseconds.
  ///
  /// Type: `i32`.
  #[serde(skip_serializing_if = "is_default")]
  pub dma_latency_us: Option<Expression>,

  /// Set per-CPU PM QoS resume latency in microseconds, or `n/a`.
  ///
  /// Type: `u32 | String`.
  #[serde(skip_serializing_if = "is_default")]
  pub pm_qos_resume_latency_us: Option<Expression>,
}

impl CpusDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> CpuEvalResult {
    let cpus = self.selected_cpus(state)?;
    let mut deltas = HashMap::with_capacity(cpus.len());
    for cpu in cpus {
      let delta = self.eval_cpu(&state.in_context(EvalContext::Cpu(&cpu)))?;
      Self::resolve_policy(&mut deltas, &cpu, &delta, state);
    }
    Ok((deltas, self.eval_global(state)?))
  }

  fn resolve_policy(
    deltas: &mut CpuDeltas,
    selected: &Arc<cpu::Cpu>,
    delta: &cpu::Delta,
    state: &EvalState<'_, '_>,
  ) {
    let local = deltas.entry(Arc::clone(selected)).or_default();
    *local = mem::take(local).or(&delta.cpu_settings());
    let shared = delta.policy_settings();
    for cpu in state.cpus {
      let same_policy = selected
        .policy
        .as_ref()
        .zip(cpu.policy.as_ref())
        .is_some_and(|(selected, candidate)| selected.path == candidate.path);
      if cpu.number == selected.number || same_policy {
        let resolved = deltas.entry(Arc::clone(cpu)).or_default();
        *resolved = mem::take(resolved).or(&shared);
      }
    }
  }

  fn selected_cpus(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Vec<Arc<cpu::Cpu>>> {
    let mut cpus: Vec<_> = match &self.for_ {
      Some(expression) => {
        let numbers = expression
          .eval(state)?
          .context("`cpu.for` resolved to undefined")?
          .try_into_list()
          .context("`cpu.for` was not a list")?
          .into_iter()
          .map(Self::cpu_number)
          .collect::<anyhow::Result<HashSet<_>>>()?;
        state
          .cpus
          .iter()
          .filter(|cpu| numbers.contains(&cpu.number))
          .cloned()
          .collect()
      },
      None => state.cpus.iter().cloned().collect(),
    };
    cpus.sort_by_key(|cpu| cpu.number);
    Ok(cpus)
  }

  fn cpu_number(expression: Expression) -> anyhow::Result<u32> {
    let number = expression
      .try_into_number()
      .context("`cpu.for` item was not a number")?;
    if number.fract() != 0.0 || !(0.0..=f64::from(u32::MAX)).contains(&number) {
      bail!("invalid CPU in `cpu.for`: {number}");
    }
    Ok(number as u32)
  }

  fn eval_cpu(&self, state: &EvalState<'_, '_>) -> anyhow::Result<cpu::Delta> {
    Ok(cpu::Delta {
      governor:                      eval_string(
        &self.governor,
        state,
        "cpu.governor",
      )?,
      energy_performance_preference: eval_epp(
        &self.energy_performance_preference,
        state,
        "cpu.energy-performance-preference",
      )?,
      energy_perf_bias:              eval_string(
        &self.energy_perf_bias,
        state,
        "cpu.energy-perf-bias",
      )?,
      frequency_minimum:             eval_frequency(
        &self.frequency_mhz_minimum,
        state,
        "cpu.frequency-mhz-minimum",
      )?,
      frequency_maximum:             eval_frequency(
        &self.frequency_mhz_maximum,
        state,
        "cpu.frequency-mhz-maximum",
      )?,
      pm_qos_resume_latency_us:      self.eval_resume_latency(state)?,
    })
  }

  fn eval_resume_latency(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Option<String>> {
    let Some(expression) = &self.pm_qos_resume_latency_us else {
      return Ok(None);
    };
    let Some(value) = expression.eval(state)? else {
      return Ok(None);
    };
    match value {
      Expression::String(value) if value == "n/a" => Ok(Some(value)),
      value => {
        let value = value.try_into_number().context(
          "`cpu.pm-qos-resume-latency-us` was not a number or \"n/a\"",
        )?;
        unsigned_integer(value)
          .context("invalid `cpu.pm-qos-resume-latency-us`")
          .map(|value| Some(value.to_string()))
      },
    }
  }

  fn eval_global(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<cpu::GlobalDelta> {
    Ok(cpu::GlobalDelta {
      turbo:                          eval_bool(
        &self.turbo,
        state,
        "cpu.turbo",
      )?,
      pstate_min_performance_percent: eval_percent(
        &self.pstate_min_performance_percent,
        state,
        "cpu.pstate-min-performance-percent",
      )?,
      pstate_max_performance_percent: eval_percent(
        &self.pstate_max_performance_percent,
        state,
        "cpu.pstate-max-performance-percent",
      )?,
      dma_latency_us:                 eval_i32(
        &self.dma_latency_us,
        state,
        "cpu.dma-latency-us",
      )?,
    })
  }
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct UncoresDelta {
  /// The uncore devices to apply the changes to. When unspecified, will be
  /// applied to all uncore devices.
  ///
  /// Type: `Vec<String>`.
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Expression>,

  /// Set minimum uncore frequency in kHz.
  ///
  /// Type: `u64`.
  #[serde(skip_serializing_if = "is_default")]
  pub frequency_khz_minimum: Option<Expression>,

  /// Set maximum uncore frequency in kHz.
  ///
  /// Type: `u64`.
  #[serde(skip_serializing_if = "is_default")]
  pub frequency_khz_maximum: Option<Expression>,
}

impl UncoresDelta {
  pub fn eval(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<UncoreDeltas> {
    log::debug!("evaluating uncore deltas...");

    let uncores = match &self.for_ {
      Some(names) => {
        let names = names
          .eval(state)?
          .context("`uncore.for` resolved to undefined")?;
        let names = names
          .try_into_list()
          .context("`uncore.for` was not a list")?;

        let mut uncores = Vec::with_capacity(names.len());

        for name in names {
          let name = name
            .try_into_string()
            .context("`uncore.for` item was not a string")?;

          uncores.push(name);
        }

        state
          .uncores
          .iter()
          .filter(|uncore| uncores.contains(&uncore.name))
          .cloned()
          .collect()
      },
      None => state.uncores.clone(),
    };

    let mut deltas = HashMap::with_capacity(uncores.len());

    for uncore in uncores {
      let delta = uncore::Delta {
        frequency_khz_minimum: eval_u64(
          &self.frequency_khz_minimum,
          state,
          "uncore.frequency-khz-minimum",
        )?,
        frequency_khz_maximum: eval_u64(
          &self.frequency_khz_maximum,
          state,
          "uncore.frequency-khz-maximum",
        )?,
      };

      deltas.insert(Arc::clone(&uncore), delta);
    }

    Ok(deltas)
  }
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct VmDelta {
  #[serde(skip_serializing_if = "is_default")]
  pub dirty_bytes:                 Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub dirty_ratio:                 Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub dirty_background_bytes:      Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub dirty_background_ratio:      Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub transparent_hugepages:       Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub transparent_hugepage_defrag: Option<Expression>,
}

impl VmDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> anyhow::Result<vm::Delta> {
    let delta = vm::Delta {
      dirty_bytes:                 eval_u64(
        &self.dirty_bytes,
        state,
        "vm.dirty-bytes",
      )?,
      dirty_ratio:                 eval_percent(
        &self.dirty_ratio,
        state,
        "vm.dirty-ratio",
      )?,
      dirty_background_bytes:      eval_u64(
        &self.dirty_background_bytes,
        state,
        "vm.dirty-background-bytes",
      )?,
      dirty_background_ratio:      eval_percent(
        &self.dirty_background_ratio,
        state,
        "vm.dirty-background-ratio",
      )?,
      transparent_hugepages:       eval_string(
        &self.transparent_hugepages,
        state,
        "vm.transparent-hugepages",
      )?,
      transparent_hugepage_defrag: eval_string(
        &self.transparent_hugepage_defrag,
        state,
        "vm.transparent-hugepage-defrag",
      )?,
    };

    delta.validate()?;
    Ok(delta)
  }
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct DisksDelta {
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_:          Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub scheduler:     Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub readahead_kib: Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub apm:           Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub spindown:      Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub alpm:          Option<Expression>,
}

impl DisksDelta {
  pub fn eval(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<(DiskDeltas, disk::GlobalDelta)> {
    let disks = match &self.for_ {
      Some(names) => {
        let names = names
          .eval(state)?
          .context("`disk.for` resolved to undefined")?;
        let names =
          names.try_into_list().context("`disk.for` was not a list")?;

        let names = names
          .into_iter()
          .map(|name| name.try_into_string())
          .collect::<anyhow::Result<Vec<_>>>()?;

        state
          .disks
          .iter()
          .filter(|disk| names.contains(&disk.name))
          .cloned()
          .collect()
      },
      None => state.disks.clone(),
    };

    let apm = eval_u8(&self.apm, state, "disk.apm")?;
    let spindown = eval_u8(&self.spindown, state, "disk.spindown")?;
    let mut deltas = HashMap::with_capacity(disks.len());

    for disk in disks {
      deltas.insert(Arc::clone(&disk), disk::Delta {
        scheduler: eval_string(&self.scheduler, state, "disk.scheduler")?,
        readahead_kib: eval_u64(
          &self.readahead_kib,
          state,
          "disk.readahead-kib",
        )?,
        apm,
        spindown,
      });
    }

    Ok((deltas, disk::GlobalDelta {
      alpm: eval_string(&self.alpm, state, "disk.alpm")?,
    }))
  }
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct UsbsDelta {
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_:                      Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub autosuspend:               Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub autosuspend_delay_seconds: Option<Expression>,
}

impl UsbsDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> anyhow::Result<UsbDeltas> {
    let devices = match &self.for_ {
      Some(names) => {
        let names = names
          .eval(state)?
          .context("`usb.for` resolved to undefined")?
          .try_into_list()
          .context("`usb.for` was not a list")?;
        let names = names
          .into_iter()
          .map(|name| name.try_into_string())
          .collect::<anyhow::Result<Vec<_>>>()?;

        state
          .usb_devices
          .iter()
          .filter(|device| names.contains(&device.name))
          .cloned()
          .collect()
      },
      None => state.usb_devices.clone(),
    };

    let delay_ms = eval_u64(
      &self.autosuspend_delay_seconds,
      state,
      "usb.autosuspend-delay-seconds",
    )?
    .map(|seconds| {
      seconds.checked_mul(1000).with_context(|| {
        "`usb.autosuspend-delay-seconds` is too large to convert to \
         milliseconds"
      })
    })
    .transpose()?;
    let mut deltas = HashMap::with_capacity(devices.len());

    for device in devices {
      deltas.insert(Arc::clone(&device), usb::Delta {
        autosuspend:          eval_bool(
          &self.autosuspend,
          state,
          "usb.autosuspend",
        )?,
        autosuspend_delay_ms: delay_ms,
      });
    }

    Ok(deltas)
  }
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct AudioDelta {
  #[serde(skip_serializing_if = "is_default")]
  pub timeout_seconds:  Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub reset_controller: Option<Expression>,
}

impl AudioDelta {
  pub fn eval(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<audio::Delta> {
    Ok(audio::Delta {
      timeout_seconds:  eval_u64(
        &self.timeout_seconds,
        state,
        "audio.timeout-seconds",
      )?,
      reset_controller: eval_bool(
        &self.reset_controller,
        state,
        "audio.reset-controller",
      )?,
    })
  }
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct GpusDelta {
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_:                Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub panel_power_savings: Option<Expression>,
  #[serde(skip_serializing_if = "is_default")]
  pub radeon_powersave:    Option<Expression>,
}

impl GpusDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> anyhow::Result<GpuDeltas> {
    let gpus = match &self.for_ {
      Some(names) => {
        let names = names
          .eval(state)?
          .context("`gpu.for` resolved to undefined")?
          .try_into_list()
          .context("`gpu.for` was not a list")?;
        let names = names
          .into_iter()
          .map(|name| name.try_into_string())
          .collect::<anyhow::Result<Vec<_>>>()?;

        state
          .gpus
          .iter()
          .filter(|gpu| names.contains(&gpu.name))
          .cloned()
          .collect()
      },
      None => state.gpus.clone(),
    };

    let panel_power_savings = eval_percent(
      &self.panel_power_savings,
      state,
      "gpu.panel-power-savings",
    )?;
    if let Some(value) = panel_power_savings
      && value > 4
    {
      bail!("`gpu.panel-power-savings` must be between 0 and 4, got {value}");
    }

    let mut deltas = HashMap::with_capacity(gpus.len());

    for gpu in gpus {
      deltas.insert(Arc::clone(&gpu), gpu::Delta {
        panel_power_savings,
        radeon_powersave: eval_string(
          &self.radeon_powersave,
          state,
          "gpu.radeon-powersave",
        )?,
      });
    }

    Ok(deltas)
  }
}

fn eval_frequency(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<cpu::frequency::Frequency>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  let Some(value) = expression.eval(state)? else {
    return Ok(None);
  };
  let value = value
    .try_into_number()
    .with_context(|| format!("`{name}` was not a number"))?;
  cpu::frequency::Frequency::from_mhz(value)
    .with_context(|| format!("invalid `{name}`"))
    .map(Some)
}

fn eval_i32(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<i32>> {
  let Some(value) = eval_u64(expression, state, name)? else {
    return Ok(None);
  };

  if value > i32::MAX as u64 {
    bail!("invalid `{name}`: {value}; maximum is {}", i32::MAX);
  }

  Ok(Some(value as i32))
}

fn eval_u8(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<u8>> {
  let Some(value) = eval_u64(expression, state, name)? else {
    return Ok(None);
  };

  if value > u8::MAX as u64 {
    bail!("invalid `{name}`: {value}");
  }

  Ok(Some(value as u8))
}
fn eval_u64(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<u64>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  let Some(value) = expression.eval(state)? else {
    return Ok(None);
  };

  let value = value
    .try_into_number()
    .with_context(|| format!("`{name}` was not a number"))?;

  unsigned_integer(value)
    .with_context(|| format!("invalid `{name}`"))
    .map(Some)
}

fn unsigned_integer(value: f64) -> anyhow::Result<u64> {
  if value.fract() != 0.0 || !(0.0..u64::MAX as f64).contains(&value) {
    bail!("value is not a representable unsigned integer: {value}");
  }
  Ok(value as u64)
}

fn eval_string(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<String>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  let Some(value) = expression.eval(state)? else {
    return Ok(None);
  };

  value
    .try_into_string()
    .with_context(|| format!("`{name}` was not a string"))
    .map(Some)
}

fn eval_epp(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<String>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  let Some(value) = expression.eval(state)? else {
    return Ok(None);
  };
  match value {
    Expression::String(value) => Ok(Some(value)),
    Expression::Number(value) => {
      if value.fract() != 0.0 || !(0.0..=255.0).contains(&value) {
        bail!("invalid `{name}`: numeric EPP must be an integer from 0 to 255");
      }
      Ok(Some((value as u8).to_string()))
    },
    _ => bail!("`{name}` was not a string or numeric EPP value"),
  }
}

fn eval_bool(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<bool>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  let Some(value) = expression.eval(state)? else {
    return Ok(None);
  };

  value
    .try_into_boolean()
    .with_context(|| format!("`{name}` was not a boolean"))
    .map(Some)
}

fn eval_percent(
  expression: &Option<Expression>,
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<u8>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  expression
    .eval(state)?
    .map(|value| {
      value
        .try_into_number()
        .and_then(percentage)
        .with_context(|| format!("invalid `{name}`"))
    })
    .transpose()
}

fn percentage(value: f64) -> anyhow::Result<u8> {
  if value.fract() != 0.0 || !(0.0..=100.0).contains(&value) {
    bail!("percentage must be an integer between 0 and 100, got {value}");
  }
  Ok(value as u8)
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct PowersDelta {
  /// The power supplies to apply the changes to. When unspecified, will be
  /// applied to all power supplies.
  ///
  /// Type: `Vec<String>`.
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Expression>,

  /// Set the percentage that the power supply has to drop under for charging
  /// to start. Short form: --charge-start.
  ///
  /// Type: `u8`.
  #[serde(skip_serializing_if = "is_default")]
  pub charge_threshold_start: Option<Expression>,

  /// Set the percentage where charging will stop. Short form: --charge-end.
  ///
  /// Type: `u8`.
  #[serde(skip_serializing_if = "is_default")]
  pub charge_threshold_end: Option<Expression>,

  /// Set ACPI platform profile. Has to be for all power supplies.
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub platform_profile: Option<Expression>,
}

impl PowersDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> PowerSupplyEvalResult {
    log::debug!("evaluating power supply deltas...");

    let power_supplies = match &self.for_ {
      Some(names) => {
        let names = names
          .eval(state)?
          .context("`power.for` resolved to undefined")?;
        let names = names
          .try_into_list()
          .context("`power.for` was not a list")?;

        let mut power_supplies = Vec::with_capacity(names.len());

        for name in names {
          let name = name
            .try_into_string()
            .context("`power.for` item was not a string")?;

          power_supplies.push(name);
        }

        state
          .power_supplies
          .iter()
          .filter(|power_supply| power_supplies.contains(&power_supply.name))
          .cloned()
          .collect()
      },
      None => state.power_supplies.clone(),
    };

    let mut deltas = HashMap::with_capacity(power_supplies.len());

    log::trace!("filtering power supplies by name: {power_supplies:?}");

    for power_supply in power_supplies {
      let state = state.in_context(EvalContext::PowerSupply(&power_supply));
      let mut delta = power_supply::Delta::default();

      if let Some(threshold_start) = &self.charge_threshold_start
        && let Some(threshold_start) = threshold_start.eval(&state)?
      {
        let threshold_start = threshold_start
          .try_into_number()
          .context("`power.charge-threshold-start` was not a number")?;

        delta.charge_threshold_start = Some(threshold_start / 100.0);
      }

      if let Some(threshold_end) = &self.charge_threshold_end
        && let Some(threshold_end) = threshold_end.eval(&state)?
      {
        let threshold_end = threshold_end
          .try_into_number()
          .context("`power.charge-threshold-end` was not a number")?;

        delta.charge_threshold_end = Some(threshold_end / 100.0);
      }

      deltas.insert(Arc::clone(&power_supply), delta);
    }

    let platform_profile = if let Some(platform_profile) =
      &self.platform_profile
      && let Some(platform_profile) = platform_profile.eval(state)?
    {
      let platform_profile = platform_profile
        .try_into_string()
        .context("`power.platform-profile` was not a string")?;

      Some(platform_profile)
    } else {
      None
    };

    Ok((deltas, platform_profile))
  }
}

mod expression {
  macro_rules! named {
    ($variant:ident => $value:literal) => {
      pub mod $variant {
        pub fn serialize<S: serde::Serializer>(
          serializer: S,
        ) -> Result<S::Ok, S::Error> {
          serializer.serialize_str($value)
        }

        pub fn deserialize<'de, D: serde::Deserializer<'de>>(
          deserializer: D,
        ) -> Result<(), D::Error> {
          struct Visitor;

          impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ();

            fn expecting(
              &self,
              writer: &mut std::fmt::Formatter,
            ) -> std::fmt::Result {
              writer.write_str(concat!("\"", $value, "\""))
            }

            fn visit_str<E: serde::de::Error>(
              self,
              value: &str,
            ) -> Result<Self::Value, E> {
              if value != $value {
                return Err(E::invalid_value(
                  serde::de::Unexpected::Str(value),
                  &self,
                ));
              }

              Ok(())
            }
          }

          deserializer.deserialize_str(Visitor)
        }
      }
    };
  }

  named!(frequency_available => "?frequency-available");
  named!(turbo_available => "?turbo-available");

  named!(cpu_usage => "%cpu-usage");
  named!(cpu_usage_volatility => "$cpu-usage-volatility");
  named!(cpu_temperature => "$cpu-temperature");
  named!(cpu_temperature_volatility => "$cpu-temperature-volatility");
  named!(cpu_idle_seconds => "$cpu-idle-seconds");
  named!(cpu_frequency_maximum => "$cpu-frequency-maximum");
  named!(cpu_frequency_minimum => "$cpu-frequency-minimum");

  named!(cpu_scaling_maximum => "$cpu-scaling-maximum");

  named!(cpu_core_count => "%cpu-core-count");

  named!(lid_closed => "?lid-closed");
  named!(virtual_machine => "?virtual-machine");

  named!(hour_of_day => "$hour-of-day");

  named!(power_supply_charge => "%power-supply-charge");
  named!(power_supply_discharge_rate => "%power-supply-discharge-rate");

  named!(battery_cycles => "$battery-cycles");
  named!(battery_health => "%battery-health");

  named!(discharging => "?discharging");
  named!(power_profile_preference => "$power-profile-preference");
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(untagged)]
#[must_use]
pub enum Expression {
  IsGovernorAvailable {
    #[serde(rename = "is-governor-available")]
    value: Box<Expression>,
  },
  IsEnergyPerformancePreferenceAvailable {
    #[serde(rename = "is-energy-performance-preference-available")]
    value: Box<Expression>,
  },
  IsEnergyPerfBiasAvailable {
    #[serde(rename = "is-energy-perf-bias-available")]
    value: Box<Expression>,
  },
  IsPlatformProfileAvailable {
    #[serde(rename = "is-platform-profile-available")]
    value: Box<Expression>,
  },
  IsChassisType {
    #[serde(rename = "is-chassis-type")]
    value: Box<Expression>,
  },

  FirstAvailableGovernor {
    #[serde(rename = "first-available-governor")]
    values: Vec<Expression>,
  },
  FirstAvailableEnergyPerformancePreference {
    #[serde(rename = "first-available-energy-performance-preference")]
    values: Vec<Expression>,
  },
  FirstAvailableEnergyPerfBias {
    #[serde(rename = "first-available-energy-perf-bias")]
    values: Vec<Expression>,
  },
  FirstAvailablePlatformProfile {
    #[serde(rename = "first-available-platform-profile")]
    values: Vec<Expression>,
  },

  IsDriverLoaded {
    #[serde(rename = "is-driver-loaded")]
    value: Box<Expression>,
  },

  IsBatteryAvailable {
    #[serde(rename = "is-battery-available")]
    value: Box<Expression>,
  },

  #[serde(with = "expression::frequency_available")]
  #[schemars(with = "String")]
  FrequencyAvailable,

  #[serde(with = "expression::turbo_available")]
  #[schemars(with = "String")]
  TurboAvailable,

  #[serde(with = "expression::cpu_usage")]
  #[schemars(with = "String")]
  CpuUsage,

  #[serde(with = "expression::cpu_usage_volatility")]
  #[schemars(with = "String")]
  CpuUsageVolatility,

  CpuUsageSince {
    #[serde(rename = "cpu-usage-since")]
    duration: Box<Expression>,
  },

  #[serde(with = "expression::cpu_temperature")]
  #[schemars(with = "String")]
  CpuTemperature,

  #[serde(with = "expression::cpu_temperature_volatility")]
  #[schemars(with = "String")]
  CpuTemperatureVolatility,

  #[serde(with = "expression::cpu_idle_seconds")]
  #[schemars(with = "String")]
  CpuIdleSeconds,

  #[serde(with = "expression::cpu_frequency_maximum")]
  #[schemars(with = "String")]
  CpuFrequencyMaximum,

  #[serde(with = "expression::cpu_frequency_minimum")]
  #[schemars(with = "String")]
  CpuFrequencyMinimum,

  #[serde(with = "expression::cpu_scaling_maximum")]
  #[schemars(with = "String")]
  CpuScalingMaximum,

  #[serde(with = "expression::cpu_core_count")]
  #[schemars(with = "String")]
  CpuCoreCount,

  LoadAverageSince {
    #[serde(rename = "load-average-since")]
    duration: Box<Expression>,
  },

  #[serde(with = "expression::lid_closed")]
  #[schemars(with = "String")]
  LidClosed,

  #[serde(with = "expression::virtual_machine")]
  #[schemars(with = "String")]
  VirtualMachine,

  #[serde(with = "expression::hour_of_day")]
  #[schemars(with = "String")]
  HourOfDay,

  #[serde(with = "expression::power_supply_charge")]
  #[schemars(with = "String")]
  PowerSupplyCharge,

  #[serde(with = "expression::power_supply_discharge_rate")]
  #[schemars(with = "String")]
  PowerSupplyDischargeRate,

  #[serde(with = "expression::battery_cycles")]
  #[schemars(with = "String")]
  BatteryCycles,

  #[serde(with = "expression::battery_health")]
  #[schemars(with = "String")]
  BatteryHealth,

  BatteryCyclesFor {
    #[serde(rename = "battery-cycles-for")]
    name: String,
  },

  BatteryHealthFor {
    #[serde(rename = "battery-health-for")]
    name: String,
  },

  #[serde(with = "expression::discharging")]
  #[schemars(with = "String")]
  Discharging,

  #[serde(with = "expression::power_profile_preference")]
  #[schemars(with = "String")]
  PowerProfilePreference,

  Boolean(bool),

  Number(f64),

  String(String),

  List(Vec<Expression>),

  // NUMBER OPERATIONS
  Plus {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "plus")]
    b: Box<Expression>,
  },
  Minus {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "minus")]
    b: Box<Expression>,
  },
  Multiply {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "multiply")]
    b: Box<Expression>,
  },
  Power {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "power")]
    b: Box<Expression>,
  },
  Divide {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "divide")]
    b: Box<Expression>,
  },

  LessThan {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "is-less-than")]
    b: Box<Expression>,
  },
  MoreThan {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "is-more-than")]
    b: Box<Expression>,
  },

  Minimum {
    #[serde(rename = "minimum")]
    numbers: Vec<Expression>,
  },
  Maximum {
    #[serde(rename = "maximum")]
    numbers: Vec<Expression>,
  },

  // BOOLEAN OPERATIONS
  IfElse {
    #[serde(rename = "if")]
    condition:   Box<Expression>,
    #[serde(rename = "then")]
    consequence: Box<Expression>,
    #[serde(default, rename = "else", skip_serializing_if = "is_default")]
    alternative: Option<Box<Expression>>,
  },

  IsUnset {
    #[serde(rename = "is-unset")]
    a: Box<Expression>,
  },

  And {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "and")]
    b: Box<Expression>,
  },
  Or {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "or")]
    b: Box<Expression>,
  },

  All {
    all: Vec<Expression>,
  },
  Any {
    any: Vec<Expression>,
  },

  Not {
    not: Box<Expression>,
  },

  // OTHER OPERATIONS
  Equal {
    #[serde(rename = "value")]
    a:      Box<Expression>,
    #[serde(rename = "is-equal")]
    b:      Box<Expression>,
    leeway: Box<Expression>,
  },
}

impl Expression {
  pub fn try_into_number(self) -> anyhow::Result<f64> {
    let Self::Number(number) = self else {
      bail!("tried to cast '{self:?}' to a number, failed")
    };

    Ok(number)
  }

  pub fn try_into_boolean(self) -> anyhow::Result<bool> {
    let Self::Boolean(boolean) = self else {
      bail!("tried to cast '{self:?}' to a boolean, failed")
    };

    Ok(boolean)
  }

  pub fn try_into_string(self) -> anyhow::Result<String> {
    let Self::String(string) = self else {
      bail!("tried to cast '{self:?}' to a string, failed")
    };

    Ok(string)
  }

  pub fn try_into_list(self) -> anyhow::Result<Vec<Expression>> {
    let Self::List(list) = self else {
      bail!("tried to cast '{self:?}' to a list, failed")
    };

    Ok(list)
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalState<'peripherals, 'context> {
  pub frequency_available: bool,
  pub turbo_available:     bool,

  pub cpu_usage:                  f64,
  pub cpu_usage_volatility:       Option<f64>,
  pub cpu_temperature:            Option<f64>,
  pub cpu_temperature_volatility: Option<f64>,
  pub cpu_idle_seconds:           f64,
  pub cpu_frequency_maximum:      Option<f64>,
  pub cpu_frequency_minimum:      Option<f64>,

  pub lid_closed:      bool,
  pub virtual_machine: bool,
  pub chassis_type:    Option<&'peripherals str>,

  pub power_supply_charge:         Option<f64>,
  pub power_supply_discharge_rate: Option<f64>,

  pub battery_cycles: Option<f64>,
  pub battery_health: Option<f64>,

  pub discharging: bool,

  pub power_profile_preference: PowerProfile,

  pub context: EvalContext<'context>,

  pub cpus:           &'peripherals HashSet<Arc<cpu::Cpu>>,
  pub uncores:        &'peripherals HashSet<Arc<uncore::Uncore>>,
  pub disks:          &'peripherals HashSet<Arc<disk::Disk>>,
  pub usb_devices:    &'peripherals HashSet<Arc<usb::UsbDevice>>,
  pub gpus:           &'peripherals HashSet<Arc<gpu::Gpu>>,
  pub power_supplies: &'peripherals HashSet<Arc<power_supply::PowerSupply>>,
  pub cpu_log:        &'peripherals VecDeque<CpuLog>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalContext<'a> {
  Cpu(&'a cpu::Cpu),
  PowerSupply(&'a power_supply::PowerSupply),
  WidestPossible,
}

impl<'peripherals> EvalState<'peripherals, '_> {
  fn cpu_value_available(
    &self,
    value: &str,
    available: impl Fn(&cpu::Cpu) -> &[String],
  ) -> bool {
    let supports = |cpu: &cpu::Cpu| {
      available(cpu).iter().any(|candidate| candidate == value)
    };

    match self.context {
      EvalContext::Cpu(cpu) => supports(cpu),
      EvalContext::PowerSupply(_) => false,
      EvalContext::WidestPossible => {
        !self.cpus.is_empty() && self.cpus.iter().all(|cpu| supports(cpu))
      },
    }
  }

  pub fn in_context<'context>(
    &self,
    context: EvalContext<'context>,
  ) -> EvalState<'peripherals, 'context> {
    EvalState { context, ..*self }
  }
}

fn literal_true() -> Expression {
  Expression::Boolean(true)
}

fn literal_is_true(expression: &Expression) -> bool {
  expression == &literal_true()
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Rule {
  pub name:     String,
  pub priority: u16,

  #[serde(
    default = "literal_true",
    rename = "if",
    skip_serializing_if = "literal_is_true"
  )]
  pub condition: Expression,

  #[serde(default, skip_serializing_if = "is_default")]
  pub cpu:    CpusDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub uncore: UncoresDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub vm:     VmDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub disk:   DisksDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub usb:    UsbsDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub audio:  AudioDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub gpu:    GpusDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub power:  PowersDelta,
}

impl Default for Rule {
  fn default() -> Self {
    Self {
      name:      String::default(),
      priority:  u16::default(),
      condition: literal_true(),
      cpu:       CpusDelta::default(),
      uncore:    UncoresDelta::default(),
      vm:        VmDelta::default(),
      disk:      DisksDelta::default(),
      usb:       UsbsDelta::default(),
      audio:     AudioDelta::default(),
      gpu:       GpusDelta::default(),
      power:     PowersDelta::default(),
    }
  }
}

#[derive(Serialize, schemars::JsonSchema, Default, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "metrics", derive(Deserialize))]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[cfg_attr(not(feature = "metrics"), non_exhaustive)]
pub struct DaemonConfig {
  pub thermal: ThermalConfig,

  #[cfg(feature = "metrics")]
  #[serde(skip_serializing_if = "Option::is_none")]
  pub metrics: Option<MetricsConfig>,

  #[serde(rename = "rule")]
  pub rules: Vec<Rule>,
}

#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq, Default,
)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThermalConfig {
  /// Report thermal performance degradation above this Celsius threshold.
  pub degradation_threshold_celsius: Option<u16>,
}

#[cfg(feature = "metrics")]
#[derive(
  Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MetricsConfig {
  pub listen_addr: IpAddr,
  pub port:        u16,
}

#[cfg(not(feature = "metrics"))]
impl<'de> Deserialize<'de> for DaemonConfig {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    use serde::de::{
      Error as _,
      IgnoredAny,
    };

    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
    struct RawDaemonConfig {
      thermal: ThermalConfig,
      metrics: Option<IgnoredAny>,

      #[serde(rename = "rule")]
      rules: Vec<Rule>,
    }

    let raw = RawDaemonConfig::deserialize(deserializer)?;

    if raw.metrics.is_some() {
      return Err(D::Error::custom(
        "metrics config requires building watt with --features metrics",
      ));
    }

    Ok(Self {
      thermal: raw.thermal,
      rules:   raw.rules,
    })
  }
}

impl DaemonConfig {
  const DEFAULT: &str = include_str!("config.toml");

  pub fn load_from(path: Option<&Path>) -> anyhow::Result<Self> {
    let contents = if let Some(path) = path {
      log::info!("loading config from '{path}'", path = path.display());

      &fs::read_to_string(path).with_context(|| {
        format!("failed to read config from '{path}'", path = path.display())
      })?
    } else {
      log::info!("loading default config");

      Self::DEFAULT
    };

    let mut config: Self = toml::from_str(contents).with_context(|| {
      path.map_or(
        "failed to parse builtin default config, this is a bug".to_owned(),
        |p| format!("failed to parse file at '{path}'", path = p.display()),
      )
    })?;

    let (errors, warnings): (Vec<_>, Vec<_>) = config
      .verify_static()
      .into_iter()
      .partition(|diagnostic| matches!(diagnostic.severity, Severity::Error));

    for warning in warnings {
      log::warn!("{warning}");
    }

    if !errors.is_empty() {
      bail!(
        "{}",
        errors
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join("\n")
      );
    }

    // This is just for debug traces.
    if log::max_level() >= log::LevelFilter::Debug {
      if config.rules.is_sorted_by_key(|rule| rule.priority) {
        log::debug!(
          "config rules are sorted by increasing priority, not doing anything"
        );
      } else {
        log::debug!("config rules aren't sorted by priority, sorting");
      }
    }

    config.rules.sort_by_key(|rule| rule.priority);

    log::debug!("sorted {len} rules by priority", len = config.rules.len());

    log::debug!("loaded config: {config:#?}");

    Ok(config)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use proptest::prelude::*;

  use super::*;

  #[test]
  fn loading_rejects_a_misspelled_rule_table() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, "[[rules]]\nname = 'power'\npriority = 1\n").unwrap();
    let error = DaemonConfig::load_from(Some(&path)).unwrap_err();
    assert!(format!("{error:#}").contains("unknown field `rules`"));
    fs::write(&path, "[[rule]]\nname = 'power'\npriority = 1\n").unwrap();
    let config = DaemonConfig::load_from(Some(&path)).unwrap();
    assert_eq!(config.rules.len(), 1);
    assert_eq!(config.rules[0].name, "power");
  }

  fn frequency_mhz(mhz: u64) -> cpu::frequency::Frequency {
    cpu::frequency::Frequency::from_mhz(mhz as f64)
      .expect("valid test frequency")
  }

  proptest! {
    #[test]
    fn test_multiply_float(
      base_freq in 1000u64..10000u64,
      multiplier in 0.1f64..1.0f64
    ) {
      // Create a mock CPU with all required fields
      // FIXME: figure out if there is a less ugly way of testing this and
      // share CPU state across tests
      let cpu = Arc::new(cpu::Cpu {
        number: 0,
        policy: None,
        has_cpufreq: true,
        frequency_control: true,
        available_governors: vec![],
        governor: None,
        frequency: Some(frequency_mhz(base_freq)),
        frequency_minimum: Some(frequency_mhz(1000)),
        frequency_maximum: Some(frequency_mhz(base_freq)),
        available_epps: vec![],
        epp: None,
        available_epbs: vec![],
        epb: None,
        stat: cpu::CpuStat::default(),
        previous_stat: None,
        info: None,
      });

      let mut cpus = HashSet::new();
      cpus.insert(cpu.clone());

      let power_supplies = HashSet::new();
      let uncores = HashSet::new();
      let disks = HashSet::new();
      let usb_devices = HashSet::new();
      let gpus = HashSet::new();
      let cpu_log = VecDeque::new();

      // Create an eval state with the base frequency
      let state = EvalState {
        frequency_available: true,
        turbo_available: false,
        cpu_usage: 0.5,
        cpu_usage_volatility: Some(0.1),
        cpu_temperature: Some(50.0),
        cpu_temperature_volatility: Some(5.0),
        cpu_idle_seconds: 10.0,
        cpu_frequency_maximum: Some(base_freq as f64),
        cpu_frequency_minimum: Some(1000.0),
        lid_closed: false,
        virtual_machine: false,
        chassis_type: None,
        power_supply_charge: Some(0.8),
        power_supply_discharge_rate: Some(10.0),
        battery_cycles: Some(100.0),
        battery_health: Some(0.95),
        discharging: false,
        power_profile_preference: PowerProfile::Balanced,
        context: EvalContext::Cpu(&cpu),
        cpus: &cpus,
        uncores: &uncores,
        disks: &disks,
        usb_devices: &usb_devices,
        gpus: &gpus,
        power_supplies: &power_supplies,
        cpu_log: &cpu_log,
      };

      // Create an expression like: { value = "$cpu-frequency-maximum", multiply = 0.65 }
      let expr = Expression::Multiply {
        a: Box::new(Expression::CpuFrequencyMaximum),
        b: Box::new(Expression::Number(multiplier)),
      };

      // Evaluate the expression
      let result = expr.eval(&state);

      // Before the fix, this would succeed but then crash when converting to u64
      // After the fix, this should succeed and round the result
      prop_assert!(result.is_ok());

      if let Ok(Some(Expression::Number(value))) = result {
        // The result might be a float
        let _expected_float = base_freq as f64 * multiplier;

        // Create a CpusDelta with the frequency_mhz_maximum field
        let cpu_delta = CpusDelta {
          for_: None,
          governor: None,
          energy_performance_preference: None,
          energy_perf_bias: None,
          frequency_mhz_minimum: None,
          frequency_mhz_maximum: Some(Expression::Number(value)),
          turbo: None,
          pstate_min_performance_percent: None,
          pstate_max_performance_percent: None,
          dma_latency_us: None,
          pm_qos_resume_latency_us: None,
        };

        // Try to evaluate it - this should not panic after the fix
        let eval_result = cpu_delta.eval(&state);

        // This test should pass after the fix is applied
        prop_assert!(
          eval_result.is_ok(),
          "Evaluation should succeed with float frequency values"
        );
      }
    }
  }

  // Specific test case that would have crashed before the fix
  // Example: 5000 MHz * 0.65 = 3250.0 (no fractional part, but it's a float)
  // Example: 3333 MHz * 0.65 = 2166.45 (has fractional part)
  #[test]
  fn test_rounding() {
    let cpu = Arc::new(cpu::Cpu {
      number:              0,
      policy:              None,
      has_cpufreq:         true,
      frequency_control:   true,
      available_governors: vec![],
      governor:            None,
      frequency:           Some(frequency_mhz(3333)),
      frequency_minimum:   Some(frequency_mhz(1000)),
      frequency_maximum:   Some(frequency_mhz(3333)),
      available_epps:      vec![],
      epp:                 None,
      available_epbs:      vec![],
      epb:                 None,
      stat:                cpu::CpuStat::default(),
      previous_stat:       None,
      info:                None,
    });

    let mut cpus = HashSet::new();
    cpus.insert(cpu.clone());

    let power_supplies = HashSet::new();
    let uncores = HashSet::new();
    let disks = HashSet::new();
    let usb_devices = HashSet::new();
    let gpus = HashSet::new();
    let cpu_log = VecDeque::new();

    let state = EvalState {
      frequency_available:         true,
      turbo_available:             false,
      cpu_usage:                   0.5,
      cpu_usage_volatility:        Some(0.1),
      cpu_temperature:             Some(50.0),
      cpu_temperature_volatility:  Some(5.0),
      cpu_idle_seconds:            10.0,
      cpu_frequency_maximum:       Some(3333.0),
      cpu_frequency_minimum:       Some(1000.0),
      lid_closed:                  false,
      virtual_machine:             false,
      chassis_type:                None,
      power_supply_charge:         Some(0.8),
      power_supply_discharge_rate: Some(10.0),
      battery_cycles:              Some(100.0),
      battery_health:              Some(0.95),
      discharging:                 false,
      power_profile_preference:    PowerProfile::Balanced,
      context:                     EvalContext::Cpu(&cpu),
      cpus:                        &cpus,
      uncores:                     &uncores,
      disks:                       &disks,
      usb_devices:                 &usb_devices,
      gpus:                        &gpus,
      power_supplies:              &power_supplies,
      cpu_log:                     &cpu_log,
    };

    // 3333 * 0.65 = 2166.45
    let cpu_delta = CpusDelta {
      for_:                           None,
      governor:                       None,
      energy_performance_preference:  None,
      energy_perf_bias:               None,
      frequency_mhz_minimum:          None,
      frequency_mhz_maximum:          Some(Expression::Multiply {
        a: Box::new(Expression::CpuFrequencyMaximum),
        b: Box::new(Expression::Number(0.65)),
      }),
      turbo:                          None,
      pstate_min_performance_percent: None,
      pstate_max_performance_percent: None,
      dma_latency_us:                 None,
      pm_qos_resume_latency_us:       None,
    };

    // Previously this would bail! with "invalid number for ...". With the
    // rounding changes this should succeed, and round to 2166
    let result = cpu_delta.eval(&state);

    assert!(
      result.is_ok(),
      "Should handle float results from multiplication"
    );

    if let Ok((deltas, _)) = result {
      let delta = deltas.get(&cpu).unwrap();
      assert!(delta.frequency_maximum.is_some());
      let frequency = delta.frequency_maximum.unwrap();
      assert_eq!(frequency.as_khz(), 2_166_450);
    }
  }

  #[test]
  fn test_volatility_expressions_with_insufficient_data() {
    let cpu = Arc::new(cpu::Cpu {
      number:              0,
      policy:              None,
      has_cpufreq:         true,
      frequency_control:   true,
      available_governors: vec![],
      governor:            None,
      frequency:           Some(frequency_mhz(3333)),
      frequency_minimum:   Some(frequency_mhz(1000)),
      frequency_maximum:   Some(frequency_mhz(3333)),
      available_epps:      vec![],
      epp:                 None,
      available_epbs:      vec![],
      epb:                 None,
      stat:                cpu::CpuStat::default(),
      previous_stat:       None,
      info:                None,
    });

    let mut cpus = HashSet::new();
    cpus.insert(cpu.clone());

    let power_supplies = HashSet::new();
    let uncores = HashSet::new();
    let disks = HashSet::new();
    let usb_devices = HashSet::new();
    let gpus = HashSet::new();
    let cpu_log = VecDeque::new();

    let state = EvalState {
      frequency_available:         true,
      turbo_available:             false,
      cpu_usage:                   0.0,
      cpu_usage_volatility:        None,
      cpu_temperature:             None,
      cpu_temperature_volatility:  None,
      cpu_idle_seconds:            0.0,
      cpu_frequency_maximum:       Some(3333.0),
      cpu_frequency_minimum:       Some(1000.0),
      lid_closed:                  false,
      virtual_machine:             false,
      chassis_type:                None,
      power_supply_charge:         None,
      power_supply_discharge_rate: None,
      battery_cycles:              None,
      battery_health:              None,
      discharging:                 false,
      power_profile_preference:    PowerProfile::Balanced,
      context:                     EvalContext::Cpu(&cpu),
      cpus:                        &cpus,
      uncores:                     &uncores,
      disks:                       &disks,
      usb_devices:                 &usb_devices,
      gpus:                        &gpus,
      power_supplies:              &power_supplies,
      cpu_log:                     &cpu_log,
    };

    let result = Expression::CpuUsageVolatility.eval(&state);
    assert!(
      result.is_ok() && result.as_ref().unwrap().is_none(),
      "CpuUsageVolatility should return None with insufficient data"
    );

    let result = Expression::CpuTemperature.eval(&state);
    assert!(
      result.is_ok() && result.as_ref().unwrap().is_none(),
      "CpuTemperature should return None with insufficient data"
    );

    let result = Expression::CpuTemperatureVolatility.eval(&state);
    assert!(
      result.is_ok() && result.as_ref().unwrap().is_none(),
      "CpuTemperatureVolatility should return None with insufficient data"
    );
  }

  #[test]
  fn cpu_capabilities_follow_the_current_evaluation_scope() {
    let cpu = Arc::new(cpu::Cpu {
      number:              0,
      policy:              None,
      has_cpufreq:         true,
      frequency_control:   true,
      available_governors: vec!["powersave".to_owned(), "schedutil".to_owned()],
      governor:            None,
      frequency:           Some(frequency_mhz(3333)),
      frequency_minimum:   Some(frequency_mhz(1000)),
      frequency_maximum:   Some(frequency_mhz(3333)),
      available_epps:      vec!["balance_power".into(), "power".into()],
      epp:                 None,
      available_epbs:      vec!["balance-power".into(), "power".into()],
      epb:                 None,
      stat:                cpu::CpuStat::default(),
      previous_stat:       None,
      info:                None,
    });

    let mut cpus = HashSet::new();
    cpus.insert(cpu.clone());
    let mut other_cpu = (*cpu).clone();
    other_cpu.number = 1;
    other_cpu.available_governors = vec!["powersave".to_owned()];
    other_cpu.available_epps = vec!["power".into()];
    other_cpu.available_epbs = vec!["power".into()];
    cpus.insert(Arc::new(other_cpu));

    let power_supplies = HashSet::new();
    let uncores = HashSet::new();
    let disks = HashSet::new();
    let usb_devices = HashSet::new();
    let gpus = HashSet::new();
    let cpu_log = VecDeque::new();

    let state = EvalState {
      frequency_available:         true,
      turbo_available:             false,
      cpu_usage:                   0.0,
      cpu_usage_volatility:        None,
      cpu_temperature:             None,
      cpu_temperature_volatility:  None,
      cpu_idle_seconds:            0.0,
      cpu_frequency_maximum:       Some(3333.0),
      cpu_frequency_minimum:       Some(1000.0),
      lid_closed:                  false,
      virtual_machine:             false,
      chassis_type:                None,
      power_supply_charge:         None,
      power_supply_discharge_rate: None,
      battery_cycles:              None,
      battery_health:              None,
      discharging:                 false,
      power_profile_preference:    PowerProfile::Balanced,
      context:                     EvalContext::Cpu(&cpu),
      cpus:                        &cpus,
      uncores:                     &uncores,
      disks:                       &disks,
      usb_devices:                 &usb_devices,
      gpus:                        &gpus,
      power_supplies:              &power_supplies,
      cpu_log:                     &cpu_log,
    };

    let result = Expression::FirstAvailableGovernor {
      values: vec![
        Expression::String("performance".to_owned()),
        Expression::String("schedutil".to_owned()),
        Expression::String("powersave".to_owned()),
      ],
    }
    .eval(&state)
    .unwrap();

    assert_eq!(result, Some(Expression::String("schedutil".to_owned())));

    let alternatives = Expression::FirstAvailableGovernor {
      values: vec![
        Expression::String("schedutil".to_owned()),
        Expression::String("powersave".to_owned()),
      ],
    };
    let global = state.in_context(EvalContext::WidestPossible);
    for (predicate, exclusive, common) in [
      ("is-governor-available", "schedutil", "powersave"),
      (
        "is-energy-performance-preference-available",
        "balance_power",
        "power",
      ),
      ("is-energy-perf-bias-available", "balance-power", "power"),
    ] {
      let exclusive: Expression =
        toml::from_str(&format!("{predicate} = {exclusive:?}")).unwrap();
      assert_eq!(
        exclusive.eval(&state).unwrap(),
        Some(Expression::Boolean(true))
      );
      assert_eq!(
        exclusive.eval(&global).unwrap(),
        Some(Expression::Boolean(false))
      );
      let common: Expression =
        toml::from_str(&format!("{predicate} = {common:?}")).unwrap();
      assert_eq!(
        common.eval(&global).unwrap(),
        Some(Expression::Boolean(true))
      );
      assert_eq!(
        common
          .eval(&EvalState {
            cpus: &HashSet::new(),
            context: EvalContext::WidestPossible,
            ..global
          })
          .unwrap(),
        Some(Expression::Boolean(false)),
      );
    }
    assert_eq!(
      alternatives.eval(&global).unwrap(),
      Some(Expression::String("powersave".to_owned())),
    );
    let empty_cpus = HashSet::new();
    assert_eq!(
      alternatives
        .eval(&EvalState {
          cpus: &empty_cpus,
          ..global
        })
        .unwrap(),
      None,
    );

    let result = Expression::FirstAvailableGovernor {
      values: vec![
        Expression::String("performance".to_owned()),
        Expression::String("ondemand".to_owned()),
      ],
    }
    .eval(&state)
    .unwrap();

    assert_eq!(result, None);
  }
}

#[cfg(test)]
#[path = "config/frequency_tests.rs"]
mod frequency_tests;

#[cfg(test)]
#[path = "config/usage_tests.rs"]
mod usage_tests;
