<!-- markdownlint-disable no-duplicate-heading -->

# Watt Changelog

<!--
This is the Watt changelog. It aims to describe changes that occurred within
the codebase, to the extent that concerns *both users and contributors*. If you
are a contributor, please add your changes under the "Unreleased" section as
tags will be created at the discretion of maintainers. If your changes fix an
existing bug, you must describe the new behaviour (ideally in comparison to the
old one) and put it under the "Fixed" subsection. Linking the relevant open
issue is not necessary, but good to have. Otherwise, general-purpose changes
can be put in the "Changed" section or, if it's just to remove code or
functionality, under the "Removed" section.
-->

## Unreleased

## 2.0.0

This release involves a grand architectural refactor that has been planned for
Watt for a long while. It took so long, because it's _massive_. Among other
things, the gigantic 2.0.0 release makes Watt safer to run unattended. Watt can
now:

- verify what it writes,
- restores the settings it owns,
- survives partial hardware failures, and
- can be managed over D-Bus with polkit authorization.

Configuration loading has also gotten stricter. While most users will not be
affected, you should note that a file that loaded under 1.x may be rejected. You
are encouraged to run `watt --config FILE --verify-config` before upgrading. Due
to the nature of this release, you are encouraged to create issues for anything
that _feels_ odd if not _acts broken_. There will be bugs, and they will be
fixed.

As a part of this release, most of the documentation was rewritten. Hopefully
this will provide a smoother experience for people trying to become power users.
If anything is missing, or if you would prefer anything to be documented in
depth, please create an issue.

### Added

- `watt --verify-config` checks a configuration without starting the daemon or
  touching hardware, and exits nonzero if it is invalid.
  - `--verify-config=hardware` also compares it with the discovered CPUs, power
    supplies and peripherals such as missing controls, unsupported values,
    ordered alternatives with no advertised match, and CPU settings that will
    also change unselected CPUs sharing a cpufreq policy.
  - Watt verifies every value it writes by reading it back, and reports each
    setting as `verified`, `clamped` when the kernel limited a frequency bound,
    `translated`, `mismatch` or `unverified`. Verified settings are left alone
    instead of being rewritten on every poll, and a clamp is logged once when it
    starts.
  - Watt restores the original value of settings it owns when a rule stops
    managing them, after a cpufreq policy change, and on SIGTERM. It does not
    overwrite a setting another program changed since, refuses to write a
    setting whose original value it cannot read, and verifies each restoration.
- New `watt config` commands query and manage the running daemon over the system
  bus: `verify FILE [--hardware]`, `reload`, `status`, `errors`, `rules`,
  `capabilities`, `settings`, `polling`, `profile get` and
  `profile set PROFILE`. `settings` shows which rule owns each managed setting,
  and `polling` explains the current polling interval.
  - SIGHUP and `watt config reload` reload a file-backed configuration between
    polling cycles. An invalid file keeps the previous configuration, reloading
    the built-in default succeeds without changes, and a reload that changes the
    metrics listener is rejected because the listener only changes on restart.
- `--json` prints one JSON record for `status`, `errors`, `rules`,
  `capabilities`, `settings`, `polling`, `active` and `profile get`.
  `active --watch --json` prints JSON Lines.

- The native `dev.notashelf.Watt` interface gained methods for status, rules,
  CPU policies, CPU, boost and device capabilities, applied settings with their
  owning rule and affected CPUs, polling diagnostics, recent errors,
  configuration verification and reload. New properties report the version,
  interface version, rule and CPU counts, error count, latest error, performance
  degradation and degraded conditions. `AppliedSettingsChanged`,
  `CapabilitiesChanged` and `ConfigurationReloaded(success, message)` signal
  changes. See
  [daemon administration](docs/configuration/administration.md#d-bus-api).
- Mutating D-Bus calls are authorized with polkit actions
  `dev.notashelf.watt.reload`, `dev.notashelf.watt.set-profile`,
  `dev.notashelf.watt.hold-profile` and
  `dev.notashelf.watt.release-any-profile-hold`. The policy file is installed to
  `/usr/share/polkit-1/actions`. Authentication prompts are only shown when the
  caller sets `AllowInteractiveAuth`.

- The preferred power profile persists across restarts when `WATT_STATE_PATH`
  names a state file. Holds, applied settings and telemetry are not persisted.
  The file is versioned and written atomically with mode `0600`.
- `PerformanceDegraded` now reports `policy-error` when runtime operations fail,
  `high-operating-temperature` when a CPU reaches
  `thermal.degradation-threshold-celsius` (disabled by default), and
  `turbo-disabled` when boost was disabled by something other than Watt.

- Shared cpufreq policies are discovered from sysfs. Governor, EPP and frequency
  bounds are written once per policy, and `GetCpuPolicies` lists each policy's
  related CPUs, including offline ones.

- `cpu.energy-performance-preference` accepts integers from `0` to `255` on
  `intel_pstate` and `amd-pstate-epp`. When `intel_pstate` reads a number back
  as its matching named preference, the setting is reported as `translated`; any
  other name is a `mismatch`.

- EPB is discovered on CPUs without cpufreq. Boost detection covers the AMD
  P-State switches and per-policy cpufreq `boost` files as well as Intel
  `no_turbo` and the global cpufreq switch, and the same backend is used for
  `?turbo-available` and for writes.

- With `--features metrics`, the endpoint exports health and polling gauges such
  as `watt_healthy`, `watt_runtime_error_count` and `watt_evaluations_total`.

### Changed

Configuration loading now rejects everything it can check without runtime data,
including in rules whose conditions are false: unknown top-level fields,
duplicate rule names, type errors in conditions and actions, out-of-range
literals, minimums above maximums, setting both a VM dirty byte limit and its
ratio, and constant arithmetic that overflows or produces NaN or infinity.
Previously most of these were accepted and failed, or were silently ignored, at
runtime. Likely mistakes such as always-false conditions, shadowed rules and
history windows longer than the retained samples are reported as warnings.

- The power-profiles-daemon interface now requires polkit authorization to set
  the active profile or hold one. Profile holds belong to the client that
  created them: only that client, or an administrator, can release them, and
  they end when the client disconnects from the bus.
- A rule whose evaluation fails is skipped as a whole, and lower-priority rules
  still apply. Previously the failure aborted the whole polling cycle.
- `is-*-available` and `first-available-*` now require a value to be supported
  by every CPU a rule targets, instead of any CPU.
- Rule priority applies across all CPUs sharing a cpufreq policy. When
  CPU-dependent expressions in one rule disagree on a shared setting, the
  lowest-numbered CPU wins.
- Temperature volatility is measured in degrees Celsius over the last five
  minutes, and the threshold that speeds up polling changed from 0.02 to 1.0 °C.
- Routine polling messages are logged at trace level.
- The NixOS module enables polkit by default, installs the polkit policy, keeps
  the profile state in `/var/lib/watt`, and reloads the daemon when the
  generated configuration changes.

### Fixed

- The NixOS module now passes the generated configuration to the daemon.
  `services.watt.settings` was written to `/etc/watt.toml` but never read, so
  the daemon always ran the built-in default.
- A failing hardware scan or write no longer stops the daemon. Each CPU and
  global CPU setting is applied independently: a failed governor write no longer
  skips EPP and frequency bounds on the same policy, and a failed P-State
  minimum no longer skips the maximum, boost or DMA latency. All failures are
  reported together. Partial CPU, power-supply, disk, USB, GPU and uncore
  discovery keeps the devices that were found and their restoration records, and
  recovers when the device returns. Removed devices are retired without restore
  errors, and devices are tracked through alias renames.
- CPU hotplug triggers a rescan, so new CPUs and policies receive the current
  settings.
- Frequency bounds are validated as a pair and written in an order that never
  produces an invalid intermediate range. Each bound is checked again after the
  first write, so a clamped first write no longer inverts the range. Restoring a
  frequency range widens it before tightening it. Frequencies keep kHz
  precision.
- `cpu-usage-since` and `load-average-since` work with a single recent sample
  ([#109](https://github.com/notashelf/watt/issues/109)). Discharge rates use
  valid elapsed intervals, and volatility only covers recent samples.
- CPU temperatures are cleared when thermal zones disappear, instead of keeping
  stale readings.
- Runtime errors keep their full cause chain, and missing uncore frequency files
  are named by full path.
