<!-- markdownlint-disable MD033-->

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
    <a alt="Docs" href="https://notashelf.github.io/watt/watt/index.html">
        <img
          src="https://github.com/NotAShelf/watt/actions/workflows/docs.yml/badge.svg"
          alt="Documentation Status"
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

It is greatly inspired by auto-cpufreq, but rewritten from ground up to provide
a smoother experience with a more efficient and more correct codebase. Some
features are omitted, and it is _not_ a drop-in replacement for auto-cpufreq,
but most common usecases are already implemented.

Watt is written in Rust, because "languages such as JS and Python should never
be used for system level daemons" [^1] Overhead is little to none, and
performance is as high as it can be, which should be a standard for all system
daemons.

[^1]: [Wise words](https://github.com/flukejones/asusctl)

## Features

- **Real-time CPU Management**: Monitor and control CPU governors, frequencies,
  and turbo boost.
- **Intelligent Power Management**: Different profiles for AC and battery
  operation.
- **Dynamic Turbo Boost Control**: Automatically enables/disables turbo based on
  CPU load and temperature.
- **Fine-tuned Controls**: Adjust energy performance preferences, biases, and
  frequency limits.
- **Per-core Control**: Apply settings globally or to specific CPU cores.
- **Battery Management**: Monitor battery status and power consumption.
- **System Load Tracking**: Track system load and make intelligent decisions.
- **Daemon Mode**: Run in background with adaptive polling to minimize overhead.
- **Conflict Detection**: Identifies and warns about conflicts with other power
  management tools.

## Usage

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

Watt operates primarily as a daemon that automatically manages CPU and power
settings based on the configuration rules. For manual CPU management, you can
use the `cpu` command (requires the `cpu` binary to be available):

```sh
# Set CPU governor for all cores
sudo cpu set --governor performance

# Set CPU governor for specific cores
sudo cpu set --governor powersave --for 0,1,2

# Set Energy Performance Preference (EPP)
sudo cpu set --energy-performance-preference performance

# Set Energy Performance Bias (EPB)
sudo cpu set --energy-performance-bias balance_performance

# Set frequency limits
sudo cpu set --frequency-mhz-minimum 800 --frequency-mhz-maximum 3000

# Enable/disable turbo boost
sudo cpu set --turbo true
sudo cpu set --turbo false

# Apply multiple settings at once
sudo cpu set --governor schedutil --energy-performance-preference balance_performance --turbo true
```

> [!NOTE]
> The `cpu` and `power` commands require the watt binary to be installed and
> available as symlinks or copies with those names. Package managers typically
> handle this setup automatically.

### Power Management

For manual power management, you can use the `power` command:

```sh
# Set battery charging thresholds to extend battery lifespan
sudo power set --charge-threshold-start 40 --charge-threshold-end 80

# Set ACPI platform profile
sudo power set --platform-profile low-power

# Apply power settings to specific power supplies
sudo power set --for BAT0 --charge-threshold-start 30 --charge-threshold-end 70
```

Battery charging thresholds help extend battery longevity by preventing constant
charging to 100%. Different laptop vendors implement this feature differently,
but Watt attempts to support multiple vendor implementations including:

- Lenovo ThinkPad/IdeaPad (Standard implementation)
- ASUS laptops
- Huawei laptops
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
  { value = "%cpu-usage", is-more-than = 0.8 },
  { value = "$cpu-idle-seconds", is-less-than = 30.0 },
  { value = "$cpu-temperature", is-less-than = 75.0 },
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
  { value = "%power-supply-charge", is-less-than = 0.3 }
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
  { value = "%power-supply-charge", is-less-than = 0.5 }
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
if = {
  value = { value = "$cpu-temperature", minus = 50.0 },
  is-more-than = 10.0
}
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

- `"%cpu-usage"` - Current CPU usage percentage (0.0-1.0)
- `"$cpu-usage-volatility"` - CPU usage volatility measurement
- `"$cpu-temperature"` - CPU temperature in Celsius
- `"$cpu-temperature-volatility"` - CPU temperature volatility
- `"$cpu-idle-seconds"` - Seconds since last significant CPU activity
- `"%power-supply-charge"` - Battery charge percentage (0.0-1.0)
- `"%power-supply-discharge-rate"` - Current discharge rate
- `"?discharging"` - Boolean indicating if system is on battery power

The expression language only has boolean and 64-bit float values.

#### Operators

- **Comparison**: `is-less-than`, `is-more-than`, `is-equal` (with `leeway`
  parameter)
- **Logical**: `and`, `or`, `not`, `all`, `any`
- **Arithmetic**: `plus`, `minus`, `multiply`, `divide`, `power`

You can use operators with TOML attribute sets:

```toml
[[rule]]
if = { value = <expression>, <operator> = <parameter> }
```

However, `all` and `any` do not take a `value` argument, but instead take a list
of expressions as the parameter:

```toml
[[rule]]
if = { all = [ <expression>, <expression2> ] }
```

### Basic Configuration Example

```toml
# Emergency thermal protection (highest priority)
[[rule]]
if = { value = "$cpu-temperature", is-more-than = 85.0 }
priority = 100

cpu.energy-performance-preference = "power"
cpu.frequency-mhz-maximum = 2000
cpu.governor = "powersave"
cpu.turbo = false

# Critical battery preservation
[[rule]]
if.all = [ "?discharging", { value = "%power-supply-charge", is-less-than = 0.3 } ]
priority = 90

cpu.energy-performance-preference = "power"
cpu.frequency-mhz-maximum = 800
cpu.governor = "powersave"
cpu.turbo = false
power.platform-profile = "low-power"

# High performance mode for sustained high load
[[rule]]
if.all = [
  { value = "%cpu-usage", is-more-than = 0.8 },
  { value = "$cpu-idle-seconds", is-less-than = 30.0 },
  { value = "$cpu-temperature", is-less-than = 75.0 },
]
priority = 80

cpu.energy-performance-preference = "performance"
cpu.governor = "performance"
cpu.turbo = true

# Performance mode when not discharging
[[rule]]
if.all = [
  { not = "?discharging" },
  { value = "%cpu-usage", is-more-than = 0.1 },
  { value = "$cpu-temperature", is-less-than = 80.0 },
]
priority = 70

cpu.energy-performance-bias = "balance_performance"
cpu.energy-performance-preference = "performance"
cpu.governor = "performance"
cpu.turbo = true

# Moderate performance for medium load
[[rule]]
if.all = [
  { value = "%cpu-usage", is-more-than = 0.4 },
  { value = "%cpu-usage", is-less-than = 0.8 },
]
priority = 60

cpu.energy-performance-preference = "balance_performance"
cpu.governor = "schedutil"

# Power saving during low activity
[[rule]]
if.all = [
  { value = "%cpu-usage", is-less-than = 0.2 },
  { value = "$cpu-idle-seconds", is-more-than = 60.0 },
]
priority = 50

cpu.energy-performance-preference = "power"
cpu.governor = "powersave"
cpu.turbo = false

# Extended idle power optimization
[[rule]]
if = { value = "$cpu-idle-seconds", is-more-than = 300.0 }
priority = 40

cpu.energy-performance-preference = "power"
cpu.frequency-mhz-maximum = 1600
cpu.governor = "powersave"
cpu.turbo = false

# Battery conservation when discharging
[[rule]]
if.all = [ "?discharging", { value = "%power-supply-charge", is-less-than = 0.5 } ]
priority = 30

cpu.energy-performance-preference = "power"
cpu.frequency-mhz-maximum = 2000
cpu.governor = "powersave"
cpu.turbo = false
power.platform-profile = "low-power"

# General battery mode
[[rule]]
if = "?discharging"
priority = 20

cpu.energy-performance-bias = "balance_power"
cpu.energy-performance-preference = "power"
cpu.frequency-mhz-maximum = 1800
cpu.frequency-mhz-minimum = 200
cpu.governor = "powersave"
cpu.turbo = false

# Balanced performance for general use - Default fallback rule
[[rule]]
priority = 0
cpu.energy-performance-preference = "balance_performance"
cpu.governor = "schedutil"
```

### CPU Settings

Available CPU configuration options:

- `governor` - CPU frequency governor (`performance`, `powersave`, `schedutil`,
  etc.)
- `energy-performance-preference` - EPP setting (`performance`,
  `balance_performance`, `balance_power`, `power`)
- `energy-performance-bias` - EPB setting (`performance`, `balance_performance`,
  `balance_power`, `power`)
- `frequency-mhz-minimum` - Minimum CPU frequency in MHz
- `frequency-mhz-maximum` - Maximum CPU frequency in MHz
- `turbo` - Enable/disable turbo boost (boolean)
- `for` - Apply settings to specific CPU cores (list of core IDs)

### Power Settings

Available power management options:

- `platform-profile` - ACPI platform profile (`performance`, `balanced`,
  `low-power`)
- `charge-threshold-start` - Battery charge level to start charging (0-100%)
- `charge-threshold-end` - Battery charge level to stop charging (0-100%)
- `for` - Apply settings to specific power supplies (list of supply names)

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
