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

Tests for each row:

- Governor: `watt/cpu.rs::scans_and_applies_cpu_controls_in_a_virtual_system_tree`,
  `watt/cpu.rs::shared_policy_writes_keep_one_original_value`.
- Frequency bounds:
  `watt/cpu.rs::scans_and_applies_cpu_controls_in_a_virtual_system_tree`,
  `watt/cpu.rs::frequency_transitions_preserve_valid_intermediate_bounds`,
  `watt/cpu.rs::clamped_frequency_records_readback_and_restores_original`,
  `watt/fs.rs::restoration_orders_bounds_without_inverting_the_range`.
- Legacy per-CPU cpufreq:
  `watt/cpu.rs::legacy_cpufreq_directories_remain_writable`.
- EPP, named:
  `watt/cpu.rs::scans_and_applies_cpu_controls_in_a_virtual_system_tree`.
- EPP, numeric: `watt/cpu.rs::numeric_epp_requires_a_supported_interface`,
  `watt/cpu.rs::numeric_epp_rejects_driver_mismatches_and_restores_observed_ownership`,
  `watt/config/verify/hardware.rs::numeric_epp_verification_requires_the_driver_and_control`.
- EPP, named readback:
  `watt/cpu.rs::numeric_epp_translation_requires_the_matching_intel_pstate_name`,
  `watt/cpu.rs::numeric_epp_accepts_named_driver_readback_for_restoration`,
  `watt/cpu.rs::translated_epp_does_not_restore_over_an_external_change`.
- EPB: `watt/cpu.rs::epb_remains_available_without_cpufreq`,
  `watt/cpu.rs::one_failed_cpu_setting_does_not_skip_the_others`.
- Boost, global:
  `watt/cpu/boost.rs::global_backends_share_read_write_semantics`,
  `watt/cpu/boost.rs::unreadable_boost_is_an_error_rather_than_disabled_or_unsupported`.
- Boost, per policy:
  `watt/cpu/boost.rs::policy_boost_attempts_each_target_and_reports_partial_failure`,
  `watt/cpu/boost.rs::policy_boost_reports_failure_when_every_target_fails`.
- Boost disabled outside Watt:
  `watt/system.rs::only_external_turbo_restriction_degrades_performance`,
  `watt/system.rs::boost_degradation_notifies_clients_and_recovers`.
- P-State limits:
  `watt/cpu.rs::one_failed_global_cpu_setting_does_not_skip_the_others`,
  `watt/config/verify/hardware.rs::configured_global_controls_report_missing_or_unreadable_without_writes`.
- Uncore frequency:
  `watt/system.rs::renamed_gpu_and_uncore_aliases_keep_restoration_ownership`,
  `watt/system/explain_tests.rs::shared_uncore_aliases_combine_winning_bounds_before_transition`,
  `watt/config/verify/hardware.rs::uncore_controls_inspect_selected_devices_without_writes`.
- Platform profile:
  `watt/system.rs::shutdown_restores_audio_and_platform_controls`,
  `watt/config/verify/hardware.rs::platform_verification_distinguishes_unavailable_unknown_and_unsupported`.
- Charge thresholds:
  `watt/power_supply.rs::threshold_writes_preserve_integer_percentages_and_reject_invalid_pairs`,
  `watt/system.rs::renamed_battery_alias_keeps_threshold_restoration`,
  `watt/config/verify/hardware.rs::charge_threshold_verification_respects_selectors_and_dynamic_values`.

Not covered by tests:

- `amd-pstate` without EPP, `intel_cpufreq` and other cpufreq drivers. Governor,
  frequency bound and boost handling do not depend on the driver name, but no
  test names these drivers. Numeric EPP is rejected for them.
- The vendor charge threshold files Watt also recognizes:
  `charge_control_start_percentage`/`charge_control_end_percentage` (ASUS),
  `charge_start_threshold`/`charge_stop_threshold` (ThinkPad, Huawei) and
  `charge_behaviour_start_threshold`/`charge_behaviour_end_threshold`
  (Framework). Driver-specific rules about how start and end thresholds relate
  are not modeled.
- Real kernel responses, such as which values a driver accepts or how it clamps
  them. Watt reports those through readback verification and
  `watt config errors`.

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
- **Device reappears.** A device that returns after its directory disappeared
  is treated as new. Its current value becomes the original that Watt restores
  later.
- **Scan fails for a device that still exists.** If a device cannot be read,
  for example because of a permission error, Watt reports the scan error and
  keeps the recorded originals. It neither applies nor restores settings for
  that device until a later scan succeeds or the device is removed.
- **Policy membership changes.** A cpufreq policy setting stays owned while at
  least one CPU of the policy is still present. When every member CPU or the
  policy directory disappears, the policy is treated as removed. A device
  reached through several aliases, such as a Radeon card listed under two DRM
  names, stays owned while any alias remains.

These cases are covered by
`watt/system.rs::policy_ownership_survives_one_member_but_not_policy_removal`,
`watt/system.rs::rescans_reappearing_cpus_without_stale_capabilities`,
`watt/system.rs::partial_cpu_scan_preserves_original_settings_for_restoration`,
`watt/system.rs::shared_radeon_device_retains_ownership_after_one_card_disappears`,
`watt/system/explain_tests.rs::repeated_partial_inventory_keeps_original_until_rediscovery`,
`watt/fs.rs::protected_device_disappearance_retires_originals_without_restore_failures`
and
`watt/tests/lifecycle.rs::hotplug_reload_and_device_disappearance_at_shutdown_restore_survivors`.
