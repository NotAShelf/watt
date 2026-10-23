# Loading, verification and daemon administration

## Installing Without NixOS

The NixOS module installs everything below. Elsewhere, install the `watt`
binary and the two files from the repository's `dbus` directory:

```sh
install -Dm644 dbus/net.hadess.PowerProfiles.conf \
  /etc/dbus-1/system.d/net.hadess.PowerProfiles.conf
install -Dm644 dbus/dev.notashelf.watt.policy \
  /usr/share/polkit-1/actions/dev.notashelf.watt.policy
systemctl reload dbus
```

`net.hadess.PowerProfiles.conf` is the system bus policy. It lets root own
`dev.notashelf.Watt` and `net.hadess.PowerProfiles` and lets any user call
them. `dev.notashelf.watt.policy` defines the polkit actions listed under
[Daemon Commands](#daemon-commands). Without it, every request that needs
authorization is denied.

Watt ships no D-Bus activation file, so a service manager has to start it. It
must run as root, since the bus policy only lets root own its names. A systemd
unit matching the NixOS module:

```ini
[Unit]
Description=Watt power management daemon
Conflicts=power-profiles-daemon.service tlp.service auto-cpufreq.service

[Service]
ExecStart=/usr/bin/watt
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
StateDirectory=watt
Environment=WATT_CONFIG=/etc/watt.toml
Environment=WATT_STATE_PATH=%S/watt/profile.toml

[Install]
WantedBy=multi-user.target
```

Leave out `WATT_CONFIG` to use the built-in default configuration. Stop
power-profiles-daemon first: it owns `net.hadess.PowerProfiles`, and Watt cannot
register its interfaces while that name is taken. Without the bus, Watt still
tunes, reloads on SIGHUP and restores settings, but `watt config` commands and
other clients cannot reach it.

## Configuration Sources

Configuration is loaded in this order:

- `--config /path/to/config.toml`
- `WATT_CONFIG=/path/to/config.toml`
- the built-in default at `watt/config.toml`

Send SIGHUP to reload a file-backed configuration. If the new file fails to
parse or validate, Watt keeps the previous configuration. On the built-in
defaults, SIGHUP and `Reload` succeed without changing anything. Changes to the
metrics listener require a restart, and a reload that changes it is rejected.

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
`dev.notashelf.watt.hold-profile`. A client can always release its own profile
hold. Releasing a hold that belongs to another client requires
`dev.notashelf.watt.release-any-profile-hold`, which defaults to administrator
authentication. If polkit is unavailable or denies the request, the operation
fails with an access-denied error. Authentication prompts are only shown when
the caller sets the D-Bus `AllowInteractiveAuth` flag.

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

| Property              | Signature | Contents                                                       |
| --------------------- | --------- | -------------------------------------------------------------- |
| `Version`             | `s`       | Watt version, such as `1.4.0`                                  |
| `InterfaceVersion`    | `u`       | Native interface version                                       |
| `RuleCount`           | `u`       | Number of rules in the loaded configuration                    |
| `CpuCount`            | `u`       | Number of CPUs found by the latest scan                        |
| `ErrorCount`          | `u`       | Number of current runtime errors, at most 32                   |
| `LatestError`         | `s`       | Most recent current error, or empty                            |
| `PerformanceDegraded` | `s`       | Degradation reason in the performance profile, or empty        |
| `DegradedConditions`  | `aa{sv}`  | Current runtime errors as `context` and `message` dictionaries |

<!--markdownlint-enable MD013-->

All properties are read-only. `ErrorCount`, `LatestError`,
`PerformanceDegraded` and `DegradedConditions` emit `PropertiesChanged`;
`RuleCount` and `CpuCount` do not, so read them again after a reload or a
hardware change.

`GetStatus` returns `profile`, `is-discharging`, `error-count` and
`performance-degraded`, plus `cpu-usage` (percent), `cpu-temperature` (degrees
Celsius), `polling-delay-ms` and `latest-error` when they are available.

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
`cpu-usage-since` and `load-average-since` windows with the `metric`, the
`rule` and `priority` that use it, `window-seconds`, `required-samples`,
`sample-count` and `available`. Windows computed at runtime omit
`window-seconds`.

`VerifyConfiguration(contents, hardware)` accepts up to 1 MiB of TOML and runs
one request at a time. Oversized or concurrent requests fail with
`org.freedesktop.DBus.Error.LimitsExceeded`, as do reloads while the reload
queue is full.

`PerformanceDegraded` is only set while the performance profile is active, and
reports one reason, checked in this order:

- `policy-error`: at least one runtime operation is failing. The failures are
  in `DegradedConditions` and `watt config errors`.
- `high-operating-temperature`: a CPU temperature has reached
  `thermal.degradation-threshold-celsius`. Disabled unless that is set.
- `turbo-disabled`: CPU boost is disabled by something other than Watt, such as
  firmware or another tool. Boost disabled by Watt's own rules is not reported.

The `DegradedConditions` property lists current failures in any profile, as
dictionaries with `context` and `message`. Entries disappear once the operation
succeeds or is no longer requested.

Watt emits `AppliedRulesChanged`, `AppliedSettingsChanged` and
`CapabilitiesChanged` when the corresponding snapshot changes; query it again to
read the new state. `ConfigurationReloaded(success, message)` reports the
outcome of every SIGHUP and `Reload`. A successful reload can still be followed
by errors when the new rules are applied. Changes are checked once per second,
so rapid changes may coalesce, and missed signals are not replayed.

### Power Profiles Interface

`net.hadess.PowerProfiles` follows power-profiles-daemon, so desktop profile
switchers work unchanged. The profiles are `performance`, `balanced` and
`power-saver`.

<!--markdownlint-disable MD013-->

| Member                 | Kind     | Signature | Contents                                                                                     |
| ---------------------- | -------- | --------- | -------------------------------------------------------------------------------------------- |
| `ActiveProfile`        | property | `s`       | Effective profile; writing it sets the preferred profile                                     |
| `Profiles`             | property | `aa{sv}`  | `Profile`, `Driver` and `CpuDriver` for each profile; drivers are `watt`                     |
| `Actions`              | property | `as`      | Always empty                                                                                 |
| `PerformanceDegraded`  | property | `s`       | Same value as the native property                                                            |
| `PerformanceInhibited` | property | `s`       | Reason of one active hold, or empty                                                          |
| `ActiveProfileHolds`   | property | `aa{sv}`  | `Profile`, `Reason` and `ApplicationId` of each hold                                         |
| `HoldProfile`          | method   | `sss`     | Hold `performance` or `power-saver` with a reason and application ID; returns a cookie (`u`) |
| `ReleaseProfile`       | method   | `u`       | Release a hold by cookie                                                                     |
| `ProfileReleased`      | signal   | `u`       | A hold ended                                                                                 |

<!--markdownlint-enable MD013-->

Writing `ActiveProfile` requires `dev.notashelf.watt.set-profile` and releases
every hold. `HoldProfile` requires `dev.notashelf.watt.hold-profile`. While
holds exist, `power-saver` wins over `performance`. A hold ends when its owner
releases it, when its owner disconnects from the bus, or when the profile is
set.

## Setting Restoration

When a rule stops managing a setting or the daemon shuts down cleanly, Watt
restores the value it read before its first write, unless another program has
changed the setting since. Failed restorations are reported and retried on
later polls. Watt refuses to write a setting whose original value it cannot
read.

`hdparm` changes are not restored. Closing the DMA latency handle releases
Watt's request. A crash, `SIGKILL` or reboot prevents shutdown restoration.

Devices that disappear or reappear while Watt runs are covered in
[Hardware Changes](hardware.md#hardware-changes).

## Profile State

Watt persists the preferred power profile only when `WATT_STATE_PATH` names a
state file. There is no default path: without it, every start uses `balanced`.
The NixOS module sets it to `/var/lib/watt/profile.toml`.

Only the preferred profile, as set through `watt config profile set` or the
`ActiveProfile` property, is stored. Profile holds, applied settings and their
original values, errors and telemetry are not, so a restart drops holds and a
crash leaves changed settings in place.

The file is TOML:

```toml
version = 1
profile = "power-saver"
```

Watt writes a temporary file with mode `0600` in the same directory, syncs it
and renames it over the old file, creating the directory if needed. If any step
before the rename fails, the old file stays intact and the profile change is
rejected with an error.

A missing file means `balanced`. If the file cannot be read or parsed, has
unknown fields, an unknown profile or an unsupported version, Watt logs a
warning and starts with `balanced`. The file is left as it is until the next
profile change replaces it.

Versions are migrated forward only. Watt reads every version up to the one it
writes, currently `1`, and rewrites the file in the current version on the next
profile change. A file without `version` predates versioning and is read as
version `0`. Watt refuses versions newer than its own instead of guessing, so
after a downgrade it starts with `balanced` and leaves the newer file alone
until the profile is set. A change to the format increments the version and
keeps reading older versions.

## Troubleshooting

These commands query the running daemon; see [Daemon Commands](#daemon-commands)
and add `--json` for scripts.

- `watt config status` shows the effective profile, whether the system is
  discharging, the error count, the degradation reason, the latest error and
  the current polling delay. If it fails to connect, the daemon is not running
  or the D-Bus policy from [Installing Without NixOS](#installing-without-nixos)
  is missing.
- `watt config errors` lists up to 32 current runtime errors, newest first, as
  `context: message`, for example a rejected write for one CPU or a failed
  device scan. An error disappears once the operation succeeds or no rule
  requests it any more. The same failures are logged.
- `watt config settings` lists every setting Watt manages with the requested
  and observed value, the verification result and the owning rule. `clamped`
  means the kernel limited a frequency bound, often to a firmware or hardware
  limit. `mismatch` means the control holds another value, usually because
  another tool changed it. `unverified` means it could not be read back. A
  setting that is absent was not requested by any matching rule, or its control
  was not found; `watt config capabilities` shows which controls Watt found.
- `watt config polling` explains the polling interval and shows whether each
  `cpu-usage-since` and `load-average-since` window has enough samples. A window
  without enough samples makes its expression unavailable.

A `PerformanceDegraded` reason means:

- `policy-error`: something in `watt config errors` is failing. Fix the rule or
  the control it names; the reason clears on the next successful cycle.
- `high-operating-temperature`: the hottest CPU reached
  `thermal.degradation-threshold-celsius`. The hardware may be throttling.
- `turbo-disabled`: boost is off and no Watt rule turned it off. Firmware, a
  BIOS setting or another tool disabled it.

An access-denied error from `reload` or `profile set` comes from polkit; check
that the policy file is installed and that the caller is allowed the action.
