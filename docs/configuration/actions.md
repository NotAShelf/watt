# Actions and targets

## CPU Actions

CPU actions go under `cpu`. Use `cpu.for` to target CPU numbers; otherwise the
setting applies to every detected CPU where that setting is per-CPU.

Governor, EPP and frequency bounds apply to the whole cpufreq policy containing
each selected CPU, while EPB and resume latency apply only to the selected CPUs.
Rule priority applies across all CPUs sharing a policy. If CPU-dependent
expressions within one rule disagree on a shared setting, the value from the
lowest-numbered CPU wins. The native D-Bus method `GetCpuPolicies` lists policy
membership, including offline CPUs.

Availability predicates and ordered alternatives in a rule's `if` condition only
accept values advertised by every discovered CPU. Inside a CPU action they use
that CPU's values, so different policies may pick different fallbacks.

```toml
[[rule]]
name = "performance-cpus"
priority = 10
cpu.for = [0, 1]
cpu.governor = "performance"
```

Current frequency is read from `cpuinfo_cur_freq`, or `scaling_cur_freq` when
the former is absent. Both are driver-reported and may be estimates.
Configuration values are in MHz and rounded to the nearest kHz.

Numeric EPP values (`0` to `255`) require an `intel_pstate` or `amd-pstate-epp`
interface, and the driver may still reject them. Prefer advertised names and
ordered alternatives for portable rules. Intel P-State reads back a preference
name when the written value equals it: `performance` for `0`, `balance_power`
for `192`, `power` for `255` and `balance_performance` for its model-specific
value. Watt reports a matching name as `translated` and any other name as
`mismatch`.

The native D-Bus `GetBoostCapabilities` method returns the selected boost
control paths, `global` or `policy` scope, and whether `1` disables boost. An
empty result means Watt found no supported control. A control can exist without
write permission. Watt reports read and write failures through daemon errors.

Supported CPU fields:

- `cpu.for`: list of CPU IDs
- `cpu.governor`: CPU frequency governor string
- `cpu.energy-performance-preference`: EPP name or integer from `0` to `255`
- `cpu.energy-perf-bias`: EPB string
- `cpu.frequency-mhz-minimum`: positive minimum scaling frequency in MHz,
  rounded to the nearest kHz
- `cpu.frequency-mhz-maximum`: positive maximum scaling frequency in MHz,
  rounded to the nearest kHz
- `cpu.turbo`: global turbo/boost boolean
- `cpu.pstate-min-performance-percent`: Intel P-State minimum percentage
- `cpu.pstate-max-performance-percent`: Intel P-State maximum percentage
- `cpu.dma-latency-us`: global `/dev/cpu_dma_latency` request in microseconds up
  to `2147483647`
- `cpu.pm-qos-resume-latency-us`: per-CPU PM QoS resume latency in microseconds
  or `"n/a"`

Example:

<!--markdownlint-disable MD013-->

```toml
[[rule]]
name = "battery-cpu-policy"
if.all = ["?discharging", { is-less-than = 0.5, value = "%power-supply-charge" }]
priority = 80

cpu.governor = { first-available-governor = ["powersave", "schedutil"] }
cpu.energy-performance-preference = { first-available-energy-performance-preference = ["power", "balance_power"] }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 1800 }
cpu.turbo = { if = "?turbo-available", then = false }
cpu.pstate-max-performance-percent = 60
cpu.pm-qos-resume-latency-us = "n/a"
```

<!--markdownlint-enable MD013-->

## Power Supply Actions

Power supply actions go under `power`. Use `power.for` to target power supply
names such as `BAT0`; otherwise battery threshold settings apply to every
detected power supply that supports them.

Supported power fields:

- `power.for`: list of power supply names
- `power.charge-threshold-start`: percentage where charging starts
- `power.charge-threshold-end`: percentage where charging stops
- `power.platform-profile`: global ACPI platform profile string

<!--markdownlint-disable MD013-->

```toml
[[rule]]
name = "battery-platform-policy"
if = "?discharging"
priority = 20

power.platform-profile = { first-available-platform-profile = ["low-power", "quiet"] }
power.charge-threshold-start = 40
power.charge-threshold-end = 80
```

<!--markdownlint-enable MD013-->

Available battery threshold controls depend on hardware. Watt supports Linux
power supply threshold paths used by Lenovo, ASUS, Huawei, Framework and devices
using the standard `power_supply` API.

## Intel Uncore Actions

Uncore actions go under `uncore`. They apply to Intel uncore frequency devices
under `/sys/devices/system/cpu/intel_uncore_frequency`.

Supported uncore fields:

- `uncore.for`: list of uncore device names
- `uncore.frequency-khz-minimum`: minimum uncore frequency in kHz
- `uncore.frequency-khz-maximum`: maximum uncore frequency in kHz

```toml
[[rule]]
name = "uncore-limit"
priority = 5
uncore.frequency-khz-maximum = 4000000
```

## VM Memory Actions

VM memory actions go under `vm`. Here, VM means Linux virtual memory, not a
virtualized machine. Dirty bytes and dirty ratios are mutually exclusive pairs,
matching the kernel sysctl behavior.

Supported VM fields:

- `vm.dirty-bytes`
- `vm.dirty-ratio`
- `vm.dirty-background-bytes`
- `vm.dirty-background-ratio`
- `vm.transparent-hugepages`: `"always"`, `"madvise"` or `"never"`
- `vm.transparent-hugepage-defrag`: `"always"`, `"defer"`, `"defer+madvise"`,
  `"madvise"` or `"never"`

```toml
[[rule]]
name = "memory-writeback-policy"
priority = 10

vm.dirty-ratio = 30
vm.dirty-background-ratio = 10
vm.transparent-hugepages = "madvise"
```

## Disk Actions

Disk actions go under `disk`. Use `disk.for` to target block device names such
as `sda` or `nvme0n1`; otherwise per-disk settings apply to all detected
non-loop, non-removable block devices.

Supported disk fields:

- `disk.for`: list of disk names
- `disk.scheduler`: block I/O scheduler string
- `disk.readahead-kib`: read-ahead size in KiB
- `disk.apm`: drive Advanced Power Management value passed to `hdparm -B`
- `disk.spindown`: drive spindown timeout passed to `hdparm -S`
- `disk.alpm`: global SATA ALPM policy for SCSI hosts

Watt skips SATA ALPM writes on AHCI ports reported as external or
hotplug-capable to preserve reliable hotplug removal detection.

```toml
[[rule]]
name = "battery-disk-policy"
if = "?discharging"
priority = 25

disk.readahead-kib = 4096
disk.alpm = "med_power_with_dipm"
```

`disk.apm` and `disk.spindown` require `hdparm` and compatible drives. Watt only
invokes `hdparm` if those fields are configured.

## USB Actions

USB actions go under `usb`. Use `usb.for` to target USB sysfs device names such
as `1-1`; otherwise settings apply to all detected USB devices with power
control files.

Supported USB fields:

- `usb.for`: list of USB device names
- `usb.autosuspend`: boolean; `true` writes `auto`, `false` writes `on`
- `usb.autosuspend-delay-seconds`: autosuspend delay in seconds

```toml
[[rule]]
name = "battery-usb-policy"
if = "?discharging"
priority = 20

usb.autosuspend = true
usb.autosuspend-delay-seconds = 2
```

## Audio Actions

Audio actions go under `audio`. They target supported module parameters for
`snd_hda_intel` and `snd_ac97_codec` when those modules expose the relevant
sysfs parameters.

Supported audio fields:

- `audio.timeout-seconds`: codec power-save timeout
- `audio.reset-controller`: boolean controller reset policy

```toml
[[rule]]
name = "battery-audio-policy"
if = "?discharging"
priority = 20

audio.timeout-seconds = 10
audio.reset-controller = true
```

## GPU Actions

GPU actions go under `gpu`. Use `gpu.for` to target DRM card names such as
`card0`; otherwise GPU settings apply to detected DRM cards with supported AMD
power files.

Supported GPU fields:

- `gpu.for`: list of DRM card names
- `gpu.panel-power-savings`: AMDGPU panel power savings level from `0` to `4`
- `gpu.radeon-powersave`: Radeon power mode string

Supported `gpu.radeon-powersave` values are `default`, `auto`, `low`, `mid`,
`high`, `dynpm`, `dpm-battery`, `dpm-balanced` and `dpm-performance`.

```toml
[[rule]]
name = "battery-panel-policy"
if.all = ["?discharging", { is-chassis-type = "laptop" }]
priority = 20

gpu.panel-power-savings = 3
```

## Targeting Summary

Sections with selectors:

- `cpu.for`: CPU numbers
- `power.for`: power supply names
- `uncore.for`: Intel uncore device names
- `disk.for`: block device names
- `usb.for`: USB sysfs device names
- `gpu.for`: DRM card names

Sections without selectors apply globally or to their known supported targets:

- `vm`
- `audio`
- `disk.alpm`
- `power.platform-profile`

## Compatibility Notes

Most Watt settings write to Linux sysfs, procfs or device files and require root
privileges. Unsupported hardware usually appears as a missing sysfs file, an
unavailable predicate or an apply-time error from the kernel. The
[support matrix](hardware.md#support-matrix) lists which drivers and controls
the tests cover.

Use availability predicates and ordered alternatives for portable rules:

<!--markdownlint-disable MD013-->

```toml
cpu.governor = { first-available-governor = ["schedutil", "powersave"] }
power.platform-profile = { first-available-platform-profile = ["balanced", "low-power"] }
cpu.turbo = { if = "?turbo-available", then = false }
```

<!--markdownlint-enable MD013-->

Run `watt --verify-config=hardware` to check which controls exist on a machine.
Writes the driver rejects at runtime show up in `watt config errors`.
