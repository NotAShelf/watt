# Configuring Watt

Watt is configured with TOML rules. Each `[[rule]]` may have a `name`, a unique
`priority`, an optional `if` condition and one or more action sections. Rules
are evaluated from highest to lowest priority. When several matching rules set
the same option, the highest-priority value wins and lower-priority rules only
fill settings that were not set yet.

## Configuration Sources

Configuration is loaded in this order:

- `--config /path/to/config.toml`
- `WATT_CONFIG=/path/to/config.toml`
- the built-in default at `watt/config.toml`

Metrics are configured with a top-level `[metrics]` table when Watt is built
with `--features metrics`:

```toml
[metrics]
listen-addr = "[::]"
port = 9790
```

## Rule Structure

<!--markdownlint-disable MD013-->

```toml
[[rule]]
name = "battery-low-power"
priority = 50
if.all = ["?discharging", { is-less-than = 0.5, value = "%power-supply-charge" }]

cpu.governor = { first-available-governor = ["powersave", "schedutil"] }
cpu.turbo = { if = "?turbo-available", then = false }
power.platform-profile = { first-available-platform-profile = ["low-power", "quiet"] }
usb.autosuspend = true
```

<!--markdownlint-enable MD013-->

If `if` is omitted, the rule always applies. `priority` is a `u16`, so valid
values are `0` through `65535`.

## Expressions

Expressions are used in rule conditions and action values. A setting may resolve
to no value; in that case Watt leaves that setting unset and lower-priority
rules may still fill it.

Constants:

- `true` and `false`
- numbers such as `42` and `0.75`
- strings such as `"powersave"`
- lists such as `["schedutil", "powersave"]`

System state expressions:

- `{ cpu-usage-since = "<duration>" }`
- `"$cpu-usage-volatility"`
- `"$cpu-temperature"`
- `"$cpu-temperature-volatility"`
- `"$cpu-idle-seconds"`
- `"$cpu-frequency-maximum"`
- `"$cpu-frequency-minimum"`
- `"$cpu-scaling-maximum"`
- `"%cpu-core-count"`
- `{ load-average-since = "<duration>" }`
- `"$hour-of-day"`
- `"?lid-closed"`
- `"?virtual-machine"`
- `"%power-supply-charge"`
- `"%power-supply-discharge-rate"`
- `"$battery-cycles"`
- `"%battery-health"`
- `{ battery-cycles-for = "BAT0" }`
- `{ battery-health-for = "BAT0" }`
- `"?discharging"`
- `"?frequency-available"`
- `"?turbo-available"`
- `"$power-profile-preference"`

Predicates:

- `{ is-governor-available = "powersave" }`
- `{ is-energy-performance-preference-available = "power" }`
- `{ is-energy-perf-bias-available = "balance-power" }`
- `{ is-platform-profile-available = "low-power" }`
- `{ is-driver-loaded = "intel_pstate" }`
- `{ is-battery-available = "BAT0" }`
- `{ is-chassis-type = "laptop" }`

Fallback selectors:

- `{ first-available-governor = ["schedutil", "powersave"] }`
- `{ first-available-energy-performance-preference = ["balance_power", "power"] }`
- `{ first-available-energy-perf-bias = ["balance-power", "power"] }`
- `{ first-available-platform-profile = ["low-power", "quiet"] }`

Operators:

- `{ is-less-than = 80.0, value = "$cpu-temperature" }`
- `{ is-more-than = 0.8, value = { cpu-usage-since = "2sec" } }`
- `{ is-equal = 12.0, value = "$hour-of-day", leeway = 0.5 }`
- `{ value = "$cpu-temperature", minus = 50.0 }`
- `{ value = "$cpu-frequency-maximum", multiply = 0.65 }`
- `{ value = 10.0, plus = 5.0 }`
- `{ value = 10.0, divide = 2.0 }`
- `{ value = 2.0, power = 3.0 }`
- `{ all = ["?discharging", { is-less-than = 0.5, value = "%power-supply-charge" }] }`
- `{ any = ["?virtual-machine", { is-chassis-type = "desktop" }] }`
- `{ not = "?discharging" }`
- `{ minimum = ["$cpu-temperature", 80.0] }`
- `{ maximum = ["$cpu-frequency-minimum", 1000.0] }`

Conditional values use `if`, `then` and optional `else`:

```toml
cpu.turbo = { if = "?turbo-available", then = false }
cpu.governor = { if.is-governor-available = "schedutil", then = "schedutil", else = "powersave" }
```

## CPU Actions

CPU actions go under `cpu`. Use `cpu.for` to target CPU numbers; otherwise the
setting applies to every detected CPU where that setting is per-CPU.

```toml
[[rule]]
priority = 10
cpu.for = [0, 1]
cpu.governor = "performance"
```

Supported CPU fields:

- `cpu.for`: list of CPU IDs
- `cpu.governor`: CPU frequency governor string
- `cpu.energy-performance-preference`: EPP string
- `cpu.energy-perf-bias`: EPB string
- `cpu.frequency-mhz-minimum`: minimum scaling frequency in MHz
- `cpu.frequency-mhz-maximum`: maximum scaling frequency in MHz
- `cpu.turbo`: global turbo/boost boolean
- `cpu.pstate-min-performance-percent`: Intel P-State minimum percentage
- `cpu.pstate-max-performance-percent`: Intel P-State maximum percentage
- `cpu.dma-latency-us`: global `/dev/cpu_dma_latency` request in microseconds up
  to `2147483647`
- `cpu.pm-qos-resume-latency-us`: per-CPU PM QoS resume latency in microseconds
  or `"n/a"`

Example:

```toml
[[rule]]
if.all = ["?discharging", { is-less-than = 0.5, value = "%power-supply-charge" }]
priority = 80

cpu.governor = { first-available-governor = ["powersave", "schedutil"] }
cpu.energy-performance-preference = { first-available-energy-performance-preference = ["power", "balance_power"] }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 1800 }
cpu.turbo = { if = "?turbo-available", then = false }
cpu.pstate-max-performance-percent = 60
cpu.pm-qos-resume-latency-us = "n/a"
```

## Power Supply Actions

Power supply actions go under `power`. Use `power.for` to target power supply
names such as `BAT0`; otherwise battery threshold settings apply to every
detected power supply that supports them.

Supported power fields:

- `power.for`: list of power supply names
- `power.charge-threshold-start`: percentage where charging starts
- `power.charge-threshold-end`: percentage where charging stops
- `power.platform-profile`: global ACPI platform profile string

```toml
[[rule]]
if = "?discharging"
priority = 20

power.platform-profile = { first-available-platform-profile = ["low-power", "quiet"] }
power.charge-threshold-start = 40
power.charge-threshold-end = 80
```

Battery thresholds are hardware-sensitive. Watt supports common Linux power
supply threshold paths used by Lenovo, ASUS, Huawei, Framework and devices using
the standard `power_supply` API.

## Intel Uncore Actions

Uncore actions go under `uncore`. They apply to Intel uncore frequency devices
under `/sys/devices/system/cpu/intel_uncore_frequency`.

Supported uncore fields:

- `uncore.for`: list of uncore device names
- `uncore.frequency-khz-minimum`: minimum uncore frequency in kHz
- `uncore.frequency-khz-maximum`: maximum uncore frequency in kHz

```toml
[[rule]]
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
unavailable predicate or an apply-time error from the kernel.

Availability helpers are preferred for portable configs:

```toml
cpu.governor = { first-available-governor = ["schedutil", "powersave"] }
power.platform-profile = { first-available-platform-profile = ["balanced", "low-power"] }
cpu.turbo = { if = "?turbo-available", then = false }
```
