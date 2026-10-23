# Hardware support and changes

## Support Matrix

Watt selects CPU controls by the sysfs files present, not by the frequency
driver's name. The exception is numeric EPP, which requires `intel_pstate` or
`amd-pstate-epp`. The matrix lists what the test suite covers and names the
tests. Tests run against an emulated sysfs tree, so they show what Watt reads
and writes, not how a particular kernel or firmware responds.

<!--markdownlint-disable MD013-->

| Control                     | Interface                                                                      | Covered drivers or devices                       |
| --------------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------ |
| Governor                    | `cpufreq/policyN/scaling_governor`                                             | `intel_pstate`, `amd-pstate-epp`, `acpi-cpufreq` |
| Frequency bounds            | `scaling_min_freq`, `scaling_max_freq`                                         | `intel_pstate`, `amd-pstate-epp`, `acpi-cpufreq` |
| Legacy per-CPU cpufreq      | `cpuN/cpufreq` as a directory, not a policy link                               | `acpi-cpufreq`                                   |
| EPP, named                  | `energy_performance_preference`                                                | `intel_pstate`, `amd-pstate-epp`                 |
| EPP, numeric                | `energy_performance_preference`                                                | `intel_pstate`, `amd-pstate-epp`                 |
| EPP, named readback         | `energy_performance_preference`                                                | `intel_pstate`                                   |
| EPB                         | `cpuN/power/energy_perf_bias`                                                  | Any CPU, with or without cpufreq                 |
| Boost, global               | `intel_pstate/no_turbo`, written inverted                                      | `intel_pstate`                                   |
| Boost, global               | `amd_pstate/cpufreq/boost`, `cpufreq/amd_pstate_enable_boost`, `cpufreq/boost` | Each listed file                                 |
| Boost, per policy           | `cpufreq/policyN/boost`                                                        | `amd-pstate-epp`                                 |
| Boost disabled outside Watt | Any boost control above                                                        | `intel_pstate/no_turbo`, `amd-pstate-epp` policy |
| P-State limits              | `intel_pstate/min_perf_pct`, `intel_pstate/max_perf_pct`                       | `intel_pstate`                                   |
| Uncore frequency            | `intel_uncore_frequency/*/min_freq_khz`, `max_freq_khz`                        | Intel uncore devices                             |
| Platform profile            | `/sys/firmware/acpi/platform_profile`                                          | ACPI platform profile                            |
| Charge thresholds           | `charge_control_start_threshold`, `charge_control_end_threshold`               | Standard `power_supply` API                      |

<!--markdownlint-enable MD013-->

Watt uses the first global boost file present, in the order listed, and falls
back to the per-policy files.

## Hardware Changes

Watt rescans CPUs, power supplies, disks, USB devices, GPUs and uncore devices
on every polling cycle. Rules apply to devices that appear, such as CPUs brought
online or a newly connected USB device, from the next cycle on.
`CapabilitiesChanged` is emitted when the reported capabilities change.

Watt tracks the settings it changed by their resolved sysfs path, not by the
device name, so a device that reappears under another alias, such as `card1`
instead of `card0`, keeps its restoration state.

- **Device removed.** When a device's sysfs directory is gone, Watt forgets the
  original values it recorded for that device. Nothing is restored and no
  restoration failure is reported. Shutdown restores the devices that remain.
- **Device reappears.** A device that returns after its directory disappeared is
  treated as new. Its current value becomes the original that Watt restores
  later.
- **Scan fails for a device that still exists.** If a device cannot be read, for
  example because of a permission error, Watt reports the scan error and keeps
  the recorded originals. It neither applies nor restores settings for that
  device until a later scan succeeds or the device is removed.
- **Policy membership changes.** A cpufreq policy setting stays owned while at
  least one CPU of the policy is still present. When every member CPU or the
  policy directory disappears, the policy is treated as removed. A device
  reached through several aliases, such as a Radeon card listed under two DRM
  names, stays owned while any alias remains.
