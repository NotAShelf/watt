# Rules and expressions

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

Watt evaluates expressions in rule conditions and action values. If an action
returns no value, the setting stays unset so a lower-priority rule can set it.
If a condition or action fails to evaluate, including arithmetic that produces
NaN or infinity, Watt records the error and skips the whole rule.

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

`cpu-usage-since` and `load-average-since` average the samples recorded within
the requested duration: CPU utilization between polls, and the kernel's
one-minute load average respectively. A single recent sample is enough; a
zero-length window has no samples and is unavailable.

Watt keeps the last 100 samples. At the slowest adaptive cadence of 30 seconds
that covers 49 minutes 30 seconds, so longer windows average whatever history
remains. `watt config polling` shows the current cadence and how many samples
each configured window has. See
[daemon administration](administration.md#d-bus-api) for the field names.

The volatility values are the mean absolute change between consecutive samples
over the last five minutes, as a fraction for usage and in degrees Celsius for
temperature.

`$cpu-idle-seconds` tracks inferred CPU inactivity. It does not measure
keyboard, mouse, or session idle time.

`%power-supply-discharge-rate` is the fraction of battery capacity discharged
per hour: `0.2` means 20% per hour. It is measured since the charge last
increased, and needs two samples in that period.

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
- Energy-performance preference fallback:

  ```toml
  { first-available-energy-performance-preference = ["balance_power", "power"] }
  ```

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
- Combined conditions:

<!--markdownlint-disable MD013-->

```toml
{ all = ["?discharging", { is-less-than = 0.5, value = "%power-supply-charge" }] }
```

<!--markdownlint-enable MD013-->

- `{ any = ["?virtual-machine", { is-chassis-type = "desktop" }] }`
- `{ not = "?discharging" }`
- `{ minimum = ["$cpu-temperature", 80.0] }`
- `{ maximum = ["$cpu-frequency-minimum", 1000.0] }`

Conditional values use `if`, `then` and optional `else`:

<!--markdownlint-disable MD013-->

```toml
cpu.turbo = { if = "?turbo-available", then = false }
cpu.governor = { if.is-governor-available = "schedutil", then = "schedutil", else = "powersave" }
```

<!--markdownlint-enable MD013-->
