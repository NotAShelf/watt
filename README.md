<!-- markdownlint-disable MD013 MD033-->

<h1 id="header" align="center">
    <pre>Watt</pre>
</h1>

<div align="center">
    <a alt="CI" href="https://github.com/NotAShelf/watt/actions">
        <img
          src="https://github.com/NotAShelf/watt/actions/workflows/build.yml/badge.svg"
          alt="Build Status"
        />
    </a>
    <a alt="License" href="https://github.com/notashelf/watt/blob/master/LICENSE">
        <img
          src="https://img.shields.io/github/license/notashelf/watt?label=License"
          alt="License"
        />
    </a>
</div>

<div align="center">
  Modern, transparent and intelligent utility for CPU management on Linux.
</div>

<div align="center">
  <br/>
  <a href="#watt-is-it">Synopsis</a><br/>
  <a href="#features">Features</a> | <a href="#usage">Usage</a><br/>
  <a href="#contributing">Contributing</a>
  <br/>
</div>

## Watt Is It?

Watt is a modern, low-overhead CPU frequency and power management utility for
Linux systems. It provides intelligent control of your CPU, governors,
frequencies and power-saving features to help optimize performance and battery
life overall.

It is _greatly_ inspired by auto-cpufreq and similar tools, but is rewritten
from ground up to provide a smoother experience with the fraction of its runtime
cost with an emphasis on efficiency, correctness and future-proofing. Some
features are omitted, and it is _not_ a drop-in replacement for auto-cpufreq,
but most common usecases are already implemented or planned to be implemented.
In addition, Watt features a very powerful configuration DSL to allow cooking up
intricate power management rules to get the _best possible performance_ out of
your system.

Watt is written in Rust, because "languages such as JS and Python should never
be used for system level daemons" [^1] Overhead is little to none, and
performance is as high as it can be, which should be a standard for all system
daemons.

[^1]: Wise words from
    [the author of asusctl](https://github.com/flukejones/asusctl)

## Features

Watt is a tiny binary with a small footprint, but it packs a bunch. Namely:

- **Daemon Mode**: Run in background with adaptive polling to minimize overhead.
- **Real-time CPU Management**: Monitor and control CPU governors, frequencies,
  and turbo boost.
- **Conflict Detection**: Identifies and warns about conflicts with other power
  management tools.
- **Powerful Config DSL**: Built on TOML, Watt features a robust configuration
  DSL to allow configuring the daemon _exactly_ to your needs.
  - **Intelligent Power Management**: Different profiles for AC and battery
    operation.
  - **Dynamic Turbo Boost Control**: Automatically enables/disables turbo based
    on CPU load and temperature.
  - **Fine-tuned Controls**: Adjust energy performance preferences, biases, and
    frequency limits.
  - **Per-core Control**: Apply settings globally or to specific CPU cores.
  - **Battery Management**: Monitor battery status and power consumption.
  - **System Load Tracking**: Track system load and make intelligent decisions.

## Usage

Using Watt is quite simple. It comes with a powerful default configuration
tested on a variety of systems to provide better performance out of the box.
Unless the user wishes to configure Watt from ground up, it's enough to simply
start the Watt daemon and leave it at that.

### Basic Commands

```sh
# Run as a daemon in the background with default configuration
sudo watt

# Run with adjusted logging
#
# You can also use the logform options
# --quiet and --verbose, and repeat them.
sudo watt -qqq # Log level: OFF   (won't log)
sudo watt -qq  # Log level: ERROR (will log only ERROR)
sudo watt -q   # Log level: WARN  (will log ERROR and up)
sudo watt      # Log level: INFO  (will log INFO and up)
sudo watt -v   # Log level: DEBUG (will log DEBUG and up)
sudo watt -vv  # Log level: TRACE (will log everything)

# Run with a custom configuration file
sudo watt --config /path/to/config.toml
```

### CPU Management

Watt operates primarily as a daemon that runs in the background and
automatically manages CPU and power settings based on set configuration rules,
using the various kernel power management APIs. All CPU settings are specified
in the configuration file:

```toml
[[rule]]
if = "?discharging"
priority = 10

cpu.governor = "powersave"
cpu.energy-performance-preference = "power"
cpu.turbo = false
```

You can apply settings to specific CPU cores using the `cpu.for` option:

```toml
[[rule]]
cpu.for = [0, 1]  # Apply to first two cores
cpu.governor = "performance"
```

### Power Management

Power settings are also specified in the configuration file:

```toml
[[rule]]
if = "?discharging"
priority = 10

# Set battery charging thresholds (as percentages, 0.0-1.0)
power.charge-threshold-start = 0.4
power.charge-threshold-end = 0.8

# Set ACPI platform profile
power.platform-profile = "low-power"

# Apply to specific power supplies
power.for = ["BAT0"]
```

Battery charging thresholds help extend battery longevity by preventing constant
charging to 100%. Different laptop vendors implement this feature differently,
but Watt attempts to support multiple vendor implementations including:

- Lenovo ThinkPad/IdeaPad
- ASUS laptops
- Huawei laptops
- Framework laptops
- Other devices using the standard Linux power_supply API

> [!NOTE]
> Battery management is sensitive, and your mileage may vary. Please open an
> issue if your vendor is not supported, but patches would help more than issue
> reports, as supporting hardware _needs_ hardware.

### Advanced Configuration Examples

#### Complex Condition Logic

```toml
# High performance for sustained workloads with thermal protection
[[rule]]
if.all = [
  { is-more-than = 0.8, value = { cpu-usage-since = "2sec" } },
  { is-less-than = 30.0, value = "$cpu-idle-seconds" },
  { is-less-than = 75.0, value = "$cpu-temperature" },
]
priority = 80

cpu.governor = "performance"
cpu.energy-performance-preference = "performance"
cpu.turbo = true
```

#### Battery Conservation with Multiple Thresholds

```toml
# Critical battery preservation
[[rule]]
if.all = [
  "?discharging",
  { is-less-than = 0.3, value = "%power-supply-charge" }
]
priority = 90

cpu.governor = "powersave"
cpu.energy-performance-preference = "power"
cpu.frequency-mhz-maximum = 800
cpu.turbo = false
power.platform-profile = "low-power"

# Moderate battery conservation
[[rule]]
if.all = [
  "?discharging",
  { is-less-than = 0.5, value = "%power-supply-charge" }
]
priority = 30

cpu.governor = "powersave"
cpu.frequency-mhz-maximum = 2000
cpu.turbo = false
```

#### Using Arithmetic Expressions

```toml
# Adaptive frequency scaling based on temperature
[[rule]]
if = { is-more-than = 10.0, value = { value = "$cpu-temperature", minus = 50.0 } }
priority = 60

cpu.frequency-mhz-maximum = 2400
cpu.governor = "schedutil"
```

## Configuration

Watt uses a rule-based TOML configuration system that allows you to define
intelligent power management policies. The daemon evaluates rules by priority
and applies the all matching rules. Rule settings with higher priority shadow
ones with lower priority.

### Configuration Locations

Configuration locations (in order of precedence):

- Custom path via `--config` flag.
- Custom path via `WATT_CONFIG` environment variable (if no flag is specified).
- Built-in default configuration (if no custom path is specified).

If no configuration file is found, Watt uses a built-in default configuration.

### Rule-Based Configuration

Rules are the core of Watt's configuration system. Each rule can specify:

- **Conditions**: When the rule should apply (using the expression DSL). If not
  specified, always applies (defaults to `true`).
- **Priority**: Higher numbers take precedence (0-65535).
- **Actions**: CPU and power management settings to apply.

### Expression DSL

Watt includes a powerful expression language for defining conditions:

#### Constants

- `true`, `false` - Booleans
- `123.456`, `42` - 64-bit Floats

#### System Variables

- `{ cpu-usage-since = "<duration>" }` - CPU usage percentage over a duration
  (e.g., `"1sec"`, `"5sec"`)
- `"$cpu-usage-volatility"` - CPU usage volatility measurement
- `"$cpu-temperature"` - CPU temperature in Celsius
- `"$cpu-temperature-volatility"` - CPU temperature volatility
- `"$cpu-idle-seconds"` - Seconds since last significant CPU activity
- `"$cpu-frequency-maximum"` - CPU hardware maximum frequency in MHz
- `"$cpu-frequency-minimum"` - CPU hardware minimum frequency in MHz
- `"$cpu-scaling-maximum"` - Current CPU scaling maximum frequency in MHz (from
  `scaling_max_freq`)
- `"$load-average-1m"` - System load average over the last 1 minute
- `"$load-average-5m"` - System load average over the last 5 minutes
- `"$load-average-15m"` - System load average over the last 15 minutes
- `"%power-supply-charge"` - Battery charge percentage (0.0-1.0)
- `"%power-supply-discharge-rate"` - Current discharge rate
- `"?discharging"` - Boolean indicating if system is on battery power
- `"?frequency-available"` - Boolean indicating if CPU frequency control is
  available
- `"?turbo-available"` - Boolean indicating if turbo boost control is available

The expression language only has boolean and 64-bit float values.

#### Predicates

Predicates check if a specific value is available on the system:

```toml
if.is-governor-available = "powersave"
if.is-energy-performance-preference-available = "balance_performance"
if.is-energy-perf-bias-available = "5"
if.is-platform-profile-available = "low-power"
if.is-driver-loaded = "intel_pstate"
```

Each will be `true` only if the named value is available on your system. If the
argument is not a string, Watt will fail with a configuration error.

#### Operators

- **Comparison**: `is-less-than`, `is-more-than`, `is-equal` (with `leeway`
  parameter)
- **Logical**: `and`, `or`, `not`, `all`, `any`
- **Arithmetic**: `plus`, `minus`, `multiply`, `divide`, `power`
- **Aggregation**: `minimum`, `maximum` - take a list of expressions

You can use operators with TOML attribute sets:

```toml
[[rule]]
if = { is-more-than = { cpu-usage-since = "1sec" }, value = 0.8 }
```

However, `all` and `any` do not take a `value` argument, but instead take a list
of expressions as the parameter:

```toml
[[rule]]
if = { all = [ <expression>, <expression2> ] }
```

#### Conditional Values

Settings can be conditionally applied based on availability:

```toml
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "balance_performance", then = "balance_performance" }
cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "5", then = "5" }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 2000 }
cpu.turbo = { if = "?turbo-available", then = true }
```

If the condition is not met, the setting will not be applied.

### Basic Configuration Example

```toml
# Emergency thermal protection (highest priority)
[[rule]]
if = { is-more-than = 85.0, value = "$cpu-temperature" }
priority = 100

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "power", then = "power" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "power", then = "power" }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 2000 }
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.turbo = { if = "?turbo-available", then = false }

# Critical battery preservation
[[rule]]
if.all = [ "?discharging", { is-less-than = 0.3, value = "%power-supply-charge" } ]
priority = 90

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "power", then = "power" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "power", then = "power" }
cpu.frequency-mhz-maximum = 800
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.turbo = { if = "?turbo-available", then = false }
power.platform-profile = { if.is-platform-profile-available = "low-power", then = "low-power" }

# High performance mode for sustained high load
[[rule]]
if.all = [
  { is-more-than = 0.8, value = { cpu-usage-since = "2sec" } },
  { is-less-than = 30.0, value = "$cpu-idle-seconds" },
  { is-less-than = 75.0, value = "$cpu-temperature" },
]
priority = 80

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "performance", then = "performance" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "performance", then = "performance" }
cpu.governor = { if.is-governor-available = "performance", then = "performance" }
cpu.turbo = { if = "?turbo-available", then = true }

# Performance mode when not discharging
[[rule]]
if.all = [
  { not = "?discharging" },
  { is-more-than = 0.1, value = { cpu-usage-since = "1sec" } },
  { is-less-than = 80.0, value = "$cpu-temperature" },
]
priority = 70

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "balance-performance", then = "balance-performance" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "performance", then = "performance" }
cpu.governor = { if.is-governor-available = "performance", then = "performance" }
cpu.turbo = { if = "?turbo-available", then = true }

# Moderate performance for medium load
[[rule]]
if.all = [
  { is-more-than = 0.4, value = { cpu-usage-since = "5sec" } },
  { is-less-than = 0.8, value = { cpu-usage-since = "5sec" } },
]
priority = 60

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "balance-performance", then = "balance-performance" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "balance_performance", then = "balance_performance" }
cpu.governor = { if.is-governor-available = "schedutil", then = "schedutil" }

# Power saving during low activity
[[rule]]
if.all = [
  { is-less-than = 0.2, value = { cpu-usage-since = "10sec" } },
  { is-more-than = 60.0, value = "$cpu-idle-seconds" },
]
priority = 50

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "power", then = "power" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "power", then = "power" }
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.turbo = { if = "?turbo-available", then = false }

# Extended idle power optimization
[[rule]]
if = { is-more-than = 300.0, value = "$cpu-idle-seconds" }
priority = 40

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "power", then = "power" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "power", then = "power" }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 1600 }
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.turbo = { if = "?turbo-available", then = false }

# Battery conservation when discharging
[[rule]]
if.all = [ "?discharging", { is-less-than = 0.5, value = "%power-supply-charge" } ]
priority = 30

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "power", then = "power" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "power", then = "power" }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 2000 }
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.turbo = { if = "?turbo-available", then = false }
power.platform-profile = { if.is-platform-profile-available = "low-power", then = "low-power" }

# General battery mode
[[rule]]
if = "?discharging"
priority = 20

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "balance-power", then = "balance-power" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "power", then = "power" }
cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 1800 }
cpu.frequency-mhz-minimum = { if = "?frequency-available", then = 200 }
cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
cpu.turbo = { if = "?turbo-available", then = false }

# Balanced performance for general use - Default fallback rule
[[rule]]
priority = 0

cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "balance-performance", then = "balance-performance" }
cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "balance_performance", then = "balance_performance" }
cpu.governor = { if.is-governor-available = "schedutil", then = "schedutil" }
```

### CPU Settings

Available CPU configuration options:

- `governor` - CPU frequency governor (`performance`, `powersave`, `schedutil`,
  etc.). You can conditionally set a governor only if it is available using:

  ```toml
  cpu.governor = { if.is-governor-available = "powersave", then = "powersave" }
  ```

  If the governor name is not a string, Watt will fail with a configuration
  error.

- `energy-performance-preference` - EPP setting (`performance`,
  `balance_performance`, `balance_power`, `power`, etc.). You can conditionally
  set an EPP only if it is available using:

  ```toml
  cpu.energy-performance-preference = { if.is-energy-performance-preference-available = "balance_performance", then = "balance_performance" }
  ```

- `energy-perf-bias` - EPB setting (`performance`, `balance-performance`,
  `balance-power`, `power`, etc.). You can conditionally set an EPB only if it
  is available using:

  ```toml
  cpu.energy-perf-bias = { if.is-energy-perf-bias-available = "5", then = "5" }
  ```

- `frequency-mhz-minimum` - Minimum CPU frequency in MHz. Can use conditional
  expression:

  ```toml
  cpu.frequency-mhz-minimum = { if = "?frequency-available", then = 200 }
  ```

- `frequency-mhz-maximum` - Maximum CPU frequency in MHz. Can use conditional
  expression:

  ```toml
  cpu.frequency-mhz-maximum = { if = "?frequency-available", then = 3000 }
  ```

- `turbo` - Enable/disable turbo boost (boolean). Can use conditional
  expression:

  ```toml
  cpu.turbo = { if = "?turbo-available", then = true }
  ```

- `for` - Apply settings to specific CPU cores (list of core IDs):

  ```toml
  cpu.for = [0, 1, 2]
  ```

### Power Settings

Available power management options:

- `platform-profile` - ACPI platform profile (`performance`, `balanced`,
  `low-power`). Can use conditional expression:

  ```toml
  power.platform-profile = { if.is-platform-profile-available = "low-power", then = "low-power" }
  ```

- `charge-threshold-start` - Battery charge level to start charging (0.0-1.0)
- `charge-threshold-end` - Battery charge level to stop charging (0.0-1.0)
- `for` - Apply settings to specific power supplies (list of supply names):

  ```toml
  power.for = ["BAT0"]
  ```

## Troubleshooting

### Permission Issues

Most CPU management commands require root privileges. If you see permission
errors, try running with `sudo`.

### Feature Compatibility

Not all features are available on all hardware:

- Turbo boost control requires CPU support for Intel/AMD boost features
- EPP/EPB settings require CPU driver support
- Platform profiles require ACPI platform profile support in your hardware

### Common Problems

1. **Settings not applying**: Check for conflicts with other power management
   tools
2. **CPU frequencies fluctuating**: May be due to thermal throttling
3. **Missing CPU information**: Verify kernel module support for your CPU

While reporting issues, please include your system information and any relevant
logs.

## Contributing

Contributions to Watt are always welcome! Whether it's bug reports, feature
requests, or code contributions, please feel free to contribute.

> [!NOTE]
> If you are looking to reimplement features from auto-cpufreq, please consider
> opening an issue first and let us know what you have in mind. Certain features
> (such as the system tray) are deliberately ignored, and might not be desired
> in the codebase as they stand. Please discuss those features with us first :)

### Setup

You will need Cargo and Rust installed on your system. Rust 1.85 or later is
required.

A `.envrc` is provided, and it's usage is encouraged for Nix users.
Alternatively, you may use Nix for a reproducible developer environment

```sh
nix develop
```

Non-Nix users may get the appropriate Cargo and Rust versions from their package
manager, or using something like Rustup.

### Formatting & Lints

Please make sure to run _at least_ `cargo fmt` (and `taplo format` if you have
modified any TOML) inside the repository to make sure all of your code is
properly formatted. For Nix code, please use Alejandra.

Clippy lints are not _required_ as of now, but a good rule of thumb to run them
before committing to catch possible code smell early.

## License

Watt is available under [Mozilla Public License v2.0](LICENSE) for your
convenience, and at our expense. Please see the license file for more details.
