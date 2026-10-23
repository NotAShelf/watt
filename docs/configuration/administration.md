# Loading, verification and daemon administration

## Configuration Sources

Configuration is loaded in this order:

- `--config /path/to/config.toml`
- `WATT_CONFIG=/path/to/config.toml`
- the built-in default at `watt/config.toml`

Send SIGHUP to reload a file-backed configuration. If the new file fails to
parse or validate, Watt keeps the previous configuration. On the built-in
defaults, SIGHUP and `Reload` succeed without changing anything. Changes to the metrics listener require a
restart, and a reload that changes it is rejected.

Thermal degradation reporting is disabled by default. Set
`thermal.degradation-threshold-celsius` to report `high-operating-temperature`
when a CPU temperature reaches that value.

Metrics are configured with a top-level `[metrics]` table when Watt is built
with `--features metrics`:

```toml
[metrics]
listen-addr = "[::]"
port = 9790
```

The endpoint exports health and polling gauges: `watt_healthy`,
`watt_runtime_error_count`, `watt_poll_*`, `watt_evaluation_elapsed_seconds`,
`watt_cpu_sample_count`, `watt_duration_metrics_available` and
`watt_duration_metrics_omitted`. `watt_evaluations_total` counts completed
evaluations. Rule names, paths and error messages are never used as labels.

## Verifying Configuration

`watt --config /path/to/config.toml --verify-config` checks a file without
starting the daemon or touching hardware, and exits nonzero if it is invalid.
Without `--config` it checks `WATT_CONFIG` or the built-in default.

`watt config verify /path/to/config.toml` sends the file to the running daemon
for verification instead; add `--hardware` to check it against the daemon's
host. Errors give a nonzero exit status, warnings alone do not. The supplied
configuration is never applied.

Loading rejects errors in anything Watt can check without runtime data,
including in rules whose conditions are false:

- TOML syntax, unknown fields and duplicate rule names or priorities.
- Type errors in conditions and actions, such as a string where `cpu.turbo`
  expects a boolean, or a non-list `for` selector.
- Out-of-range literals: CPU and uncore frequency bounds, P-State percentages,
  charge thresholds, VM dirty limits, disk, USB, audio, GPU and latency values,
  and hugepage modes.
- Minimums above maximums, and setting both a VM dirty byte limit and its ratio.
- Constant arithmetic that overflows or produces NaN or infinity.

Expressions that need no runtime data, such as constant arithmetic, are
evaluated before these checks. Expressions that depend on telemetry or hardware
are checked when the rule runs.

Verification also warns about likely mistakes: conditions that are always false
or always true, rules fully shadowed by higher-priority rules, empty
`first-available-*` lists, and `cpu-usage-since` or `load-average-since` windows
that are zero or longer than the retained history.

`--verify-config=hardware` additionally compares the configuration with the
discovered CPUs, power supplies and peripherals without writing anything or
running `hdparm`. It warns about missing controls, unsupported values, ordered
alternatives with no advertised match, and CPU settings that will also change
unselected CPUs sharing a cpufreq policy. Incomplete hardware discovery is an
error. Whether the driver accepts a write, and whether Watt has permission to
make it, is only known at runtime.

## Daemon Commands

These commands query the running daemon over the system D-Bus:

- `watt config show` prints the loaded configuration as normalized TOML.
  Comments and formatting are not preserved. `--default` prints the built-in
  default instead and does not need the daemon.
- `watt config active` prints the rules selected by the latest polling pass, in
  priority order. `--watch` prints a new snapshot after each change, separated
  by `--`; rapid changes may be coalesced.
- `watt config status`, `errors`, `rules`, `capabilities`, `settings`,
  `polling` and `profile get` print daemon state. `settings` shows which rule
  owns each managed setting, and `polling` explains the current polling
  interval.
- `watt config reload` reloads the configuration file.
- `watt config profile set PROFILE` selects a power profile.

`watt config schema` prints a JSON Schema for the configuration accepted by the
current build. Feature-gated fields such as `metrics` only appear when the
feature is enabled.

Reloading and changing or holding a profile are authorized through the polkit
actions `dev.notashelf.watt.reload`, `dev.notashelf.watt.set-profile` and
`dev.notashelf.watt.hold-profile`. If polkit is unavailable or denies the
request, the operation fails with an access-denied error. Authentication prompts
are only shown when the caller sets the D-Bus `AllowInteractiveAuth` flag.

## Scripting Output

Add `--json` to `status`, `errors`, `rules`, `capabilities`, `settings`,
`polling`, `active` or `profile get` to print one newline-terminated JSON
record. `active --watch --json` prints JSON Lines. Missing optional fields mean
the information is unavailable, and key order is not stable. Mutations,
verification, `show` and `schema` reject `--json`.

```sh
watt config --json settings
watt config --json polling
watt config --json active --watch
```

## D-Bus API

The native service and interface are `dev.notashelf.Watt` at
`/dev/notashelf/Watt`. The power-profiles-daemon compatible interface is
`net.hadess.PowerProfiles` at `/net/hadess/PowerProfiles`.

The native `InterfaceVersion` property is currently `1`. Within an interface
version, existing wire types and field meanings stay compatible. New methods,
signals and dictionary keys may be added, so clients must ignore unknown keys.

<!--markdownlint-disable MD013-->

| Method                     | Result signature | Contents                                                                  |
| -------------------------- | ---------------- | ------------------------------------------------------------------------- |
| `GetStatus`                | `a{sv}`          | Profile, telemetry, errors and the current polling interval               |
| `GetRules`                 | `a(sq)`          | Configured rule names and priorities                                      |
| `GetAppliedRules`          | `as`             | Rules matched by the latest polling pass                                  |
| `GetCpuPolicies`           | `a(ssaus)`       | Policy name, sysfs path, related CPUs (including offline) and driver      |
| `GetCpuCapabilities`       | `aa{sv}`         | Per-CPU controls and optional integer `min-khz`/`max-khz` hardware bounds |
| `GetBoostCapabilities`     | `a{sv}`          | Boost control paths, `global` or `policy` scope, and inversion            |
| `GetDeviceCapabilities`    | `aa{sv}`         | `subsystem`, `name`, `path`, `controls`, `available-values`               |
| `GetAppliedSettings`       | `a(ssss)`        | Path, requested value, observed value and verification                    |
| `GetAppliedSettingDetails` | `aa{sv}`         | The same values plus the owning `rule`, `priority` and affected `cpus`    |
| `GetPollingDiagnostics`    | `a{sv}`          | Polling interval calculation and sample availability                      |
| `GetRecentErrors`          | `as`             | Up to 32 current errors, newest first                                     |
| `GetConfig`                | `s`              | Loaded configuration as normalized TOML                                   |
| `VerifyConfiguration`      | `a(ssqss)`       | Severity, rule, priority, field and message for the supplied TOML         |
| `Reload`                   | empty            | Reload the configuration file and wait for the result                     |

<!--markdownlint-enable MD013-->

Queries return snapshots taken by the polling loop and never rescan hardware. A
listed control may still be unwritable, or reject a value.

Setting verification is `verified` for an exact readback, `clamped` when the
kernel adjusted a frequency bound, `translated` when Intel P-State reported a
named EPP for a numeric write, `mismatch` for a different value and
`unverified` when the control cannot be read back. DMA latency requests and
`hdparm` settings are not listed.

`GetAppliedSettingDetails` adds `cpus` (`au`) to CPU policy and per-CPU
settings. For a policy control it lists every logical CPU in the policy,
including offline CPUs, since one write affects all of them. Global controls
such as Intel P-State `no_turbo` omit it.

`GetPollingDiagnostics` reports `base-interval-seconds`,
`current-interval-seconds`, `next-interval-seconds`, `idle-factor`,
`volatility-factor`, `requested-delay-seconds`, `smoothed-delay-seconds`,
`clamped-delay-seconds` and `evaluation-elapsed-seconds`, along with sample
counts and availability flags. `duration-metrics` lists up to 32 configured
`cpu-usage-since` and `load-average-since` windows with their `window-seconds`,
`required-samples`, `sample-count` and `available`. Windows computed at runtime
omit `window-seconds`.

`VerifyConfiguration(contents, hardware)` accepts up to 1 MiB of TOML and runs
one request at a time. Oversized or concurrent requests fail with
`org.freedesktop.DBus.Error.LimitsExceeded`, as do reloads while the reload
queue is full.

`PerformanceDegraded` reports `policy-error` while the performance profile is
active and runtime operations are failing. It reports `turbo-disabled` when CPU
boost is disabled by something other than Watt, such as firmware or another
tool; boost disabled by Watt's own rules is not reported. The `DegradedConditions` property
lists current failures in any profile, as dictionaries with `context` and
`message`. Entries disappear once the operation succeeds or is no longer
requested.

Watt emits `AppliedRulesChanged`, `AppliedSettingsChanged` and
`CapabilitiesChanged` when the corresponding snapshot changes; query it again to
read the new state. `ConfigurationReloaded(success, message)` reports the
outcome of every SIGHUP and `Reload`. A successful reload can still be followed
by errors when the new rules are applied. Changes are checked once per second,
so rapid changes may coalesce, and missed signals are not replayed.

## Setting Restoration

When a rule stops managing a setting or the daemon shuts down cleanly, Watt
restores the value it read before its first write, unless another program has
changed the setting since. Failed restorations are reported and retried on
later polls. Watt refuses to write a setting whose original value it cannot
read.

`hdparm` changes are not restored. Closing the DMA latency handle releases
Watt's request. A crash, `SIGKILL` or reboot prevents shutdown restoration.
