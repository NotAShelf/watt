<h1 id="header" align="center">
  Superfreq
</h1>

<div align="center">
  Modern, transparent and intelligent utility for CPU management on Linux.
</div>

<div align="center">
  <br/>
  <a href="#what-is-superfreq">Synopsis</a><br/>
  <a href="#features">Features</a> | <a href="#usage">Usage</a><br/>
  <a href="#contributing">Contributing</a>
  <br/>
</div>

## What is Superfreq

Superfreq is a modern CPU frequency and power management utility for Linux
systems. It provides intelligent control of CPU governors, frequencies, and
power-saving features, helping optimize both performance and battery life.

It is greatly inspired by auto-cpufreq, but rewritten from ground up to provide
a smoother experience with a more efficient and more correct codebase. Some
features are omitted, and it is _not_ a drop-in replacement for auto-cpufreq,
but most common usecases are already implemented.

## Features

- **Real-time CPU Management**: Monitor and control CPU governors, frequencies,
  and turbo boost
- **Intelligent Power Management**: Different profiles for AC and battery
  operation
- **Dynamic Turbo Boost Control**: Automatically enables/disables turbo based on
  CPU load and temperature
- **Fine-tuned Controls**: Adjust energy performance preferences, biases, and
  frequency limits
- **Per-core Control**: Apply settings globally or to specific CPU cores
- **Battery Management**: Monitor battery status and power consumption
- **System Load Tracking**: Track system load and make intelligent decisions
- **Daemon Mode**: Run in background with adaptive polling to minimize overhead
- **Conflict Detection**: Identifies and warns about conflicts with other power
  management tools

## Usage

### Basic Commands

```bash
# Show current system information
superfreq info

# Run as a daemon in the background
sudo superfreq daemon

# Run with verbose logging
sudo superfreq daemon --verbose

# Display comprehensive debug information
superfreq debug
```

### CPU Governor Control

```bash
# Set CPU governor for all cores
sudo superfreq set-governor performance

# Set CPU governor for a specific core
sudo superfreq set-governor powersave --core-id 0

# Force a specific governor mode persistently
sudo superfreq force-governor performance
```

### Turbo Boost Management

```bash
# Always enable turbo boost
sudo superfreq set-turbo always

# Disable turbo boost
sudo superfreq set-turbo never

# Let Superfreq manage turbo boost based on conditions
sudo superfreq set-turbo auto
```

### Power and Performance Settings

```bash
# Set Energy Performance Preference (EPP)
sudo superfreq set-epp performance

# Set Energy Performance Bias (EPB)
sudo superfreq set-epb 4

# Set ACPI platform profile
sudo superfreq set-platform-profile balanced
```

### Frequency Control

```bash
# Set minimum CPU frequency (in MHz)
sudo superfreq set-min-freq 800

# Set maximum CPU frequency (in MHz)
sudo superfreq set-max-freq 3000

# Set per-core frequency limits
sudo superfreq set-min-freq 1200 --core-id 0
sudo superfreq set-max-freq 2800 --core-id 1
```

### Battery Management

```bash
# Set battery charging thresholds to extend battery lifespan
sudo superfreq set-battery-thresholds 40 80  # Start charging at 40%, stop at 80%
```

Battery charging thresholds help extend battery longevity by preventing constant
charging to 100%. Different laptop vendors implement this feature differently,
but Superfreq attempts to support multiple vendor implementations including:

- Lenovo ThinkPad/IdeaPad (Standard implementation)
- ASUS laptops
- Huawei laptops
- Other devices using the standard Linux power_supply API

Note that battery management is sensitive, and that your mileage may vary.
Please open an issue if your vendor is not supported, but patches would help
more than issue reports, as supporting hardware _needs_ hardware.

## Configuration

Superfreq uses TOML configuration files. Default locations:

- `/etc/xdg/superfreq/config.toml`
- `/etc/superfreq.toml`

You can also specify a custom path by setting the `SUPERFREQ_CONFIG` environment
variable.

### Sample Configuration

```toml
# Settings for when connected to a power source
[charger]
# CPU governor to use
governor = "performance"
# Turbo boost setting: "always", "auto", or "never"
turbo = "auto"
# Enable or disable automatic turbo management (when turbo = "auto")
enable_auto_turbo = true
# Custom thresholds for auto turbo management
turbo_auto_settings = {
    load_threshold_high = 70.0,
    load_threshold_low = 30.0,
    temp_threshold_high = 75.0,
    initial_turbo_state = false,  # whether turbo should be initially enabled (false = disabled)
}
# Energy Performance Preference
epp = "performance"
# Energy Performance Bias (0-15 scale or named value)
epb = "balance_performance"
# Platform profile (if supported)
platform_profile = "performance"
# Min/max frequency in MHz (optional)
min_freq_mhz = 800
max_freq_mhz = 3500
# Optional: Profile-specific battery charge thresholds (overrides global setting)
# battery_charge_thresholds = [40, 80]  # Start at 40%, stop at 80%

# Settings for when on battery power
[battery]
governor = "powersave"
turbo = "auto"
# More conservative auto turbo settings on battery
enable_auto_turbo = true
turbo_auto_settings = {
    load_threshold_high = 80.0,
    load_threshold_low = 40.0,
    temp_threshold_high = 70.0,
    initial_turbo_state = false,  # start with turbo disabled on battery for power savings
}
epp = "power"
epb = "balance_power"
platform_profile = "low-power"
min_freq_mhz = 800
max_freq_mhz = 2500
# Optional: Profile-specific battery charge thresholds (overrides global setting)
# battery_charge_thresholds = [60, 80]  # Start at 60%, stop at 80% (more conservative)

# Global battery charging thresholds (applied to both profiles unless overridden)
# Start charging at 40%, stop at 80% - extends battery lifespan
# NOTE: Profile-specific thresholds (in [charger] or [battery] sections)
# take precedence over this global setting
battery_charge_thresholds = [40, 80]

# Daemon configuration
[daemon]
# Base polling interval in seconds
poll_interval_sec = 5
# Enable adaptive polling that changes with system state
adaptive_interval = true
# Minimum polling interval for adaptive polling (seconds)
min_poll_interval_sec = 1
# Maximum polling interval for adaptive polling (seconds)
max_poll_interval_sec = 30
# Double the polling interval when on battery to save power
throttle_on_battery = true
# Logging level: Error, Warning, Info, Debug
log_level = "Info"
# Optional stats file path
stats_file_path = "/var/run/superfreq-stats"

# Optional: List of power supplies to ignore
[power_supply_ignore_list]
mouse_battery = "hid-12:34:56:78:90:ab-battery"
# Add other devices to ignore here
```

## Advanced Features

Those are the more advanced features of Superfreq that some users might be more
inclined to use than others. If you have a use-case that is not covered, please
create an issue.

### Dynamic Turbo Boost Management

When using `turbo = "auto"` with `enable_auto_turbo = true`, Superfreq
dynamically controls CPU turbo boost based on:

- **CPU Load Thresholds**: Enables turbo when load exceeds `load_threshold_high`
  (default 70%), disables when below `load_threshold_low` (default 30%)
- **Temperature Protection**: Automatically disables turbo when CPU temperature
  exceeds `temp_threshold_high` (default 75°C)
- **Hysteresis Control**: Prevents rapid toggling by maintaining previous state
  when load is between thresholds
- **Configurable Initial State**: Sets the initial turbo state via
  `initial_turbo_state` (default: disabled) before system load data is available
- **Profile-Specific Settings**: Configure different thresholds for battery vs.
  AC power

This feature optimizes performance and power consumption by providing maximum
performance for demanding tasks while conserving energy during light workloads.

> [!TIP]
> You can disable this logic with `enable_auto_turbo = false` to let the system
> handle turbo boost natively when `turbo = "auto"`.

#### Turbo Boost Behavior Table

The table below explains how different combinations of `turbo` and
`enable_auto_turbo` settings affect CPU turbo behavior:

| Setting            | `enable_auto_turbo = true`                                                                               | `enable_auto_turbo = false`                                                                                  |
| ------------------ | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `turbo = "always"` | **Always enabled**<br>Turbo is always active regardless of CPU load or temperature                       | **Always enabled**<br>Turbo is always active regardless of CPU load or temperature                           |
| `turbo = "never"`  | **Always disabled**<br>Turbo is always disabled regardless of CPU load or temperature                    | **Always disabled**<br>Turbo is always disabled regardless of CPU load or temperature                        |
| `turbo = "auto"`   | **Dynamically managed**<br>Superfreq enables/disables turbo based on CPU load and temperature thresholds | **System default**<br>Turbo is reset to system's default enabled state and is managed by the hardware/kernel |

> [!NOTE]
> When `turbo = "auto"` and `enable_auto_turbo = false`, Superfreq ensures that
> any previous turbo state restrictions are removed, allowing the
> hardware/kernel to manage turbo behavior according to its default algorithms.

### Adaptive Polling

Superfreq includes a "sophisticated" (euphemism for complicated) adaptive
polling system to try and maximize power efficiency

- **Battery Discharge Analysis** - Automatically adjusts polling frequency based
  on the battery discharge rate, reducing system activity when battery is
  draining quickly
- **System Activity Pattern Recognition** - Monitors CPU usage and temperature
  patterns to identify system stability
- **Dynamic Interval Calculation** - Uses multiple factors to determine optimal
  polling intervals - up to 3x longer on battery with minimal user impact
- **Idle Detection** - Significantly reduces polling frequency during extended
  idle periods to minimize power consumption
- **Gradual Transition** - Smooth transitions between polling rates to avoid
  performance spikes
- **Progressive Back-off** - Implements logarithmic back-off during idle periods
  (1min -> 1.5x, 2min -> 2x, 4min -> 3x, 8min -> 4x, 16min -> 5x)
- **Battery Discharge Protection** - Includes safeguards against measurement
  noise to prevent erratic polling behavior

When enabled, this intelligent polling system provides substantial power savings
over conventional fixed-interval approaches, especially during low-activity or
idle periods, while maintaining responsiveness when needed.

### Power Supply Filtering

Configure Superfreq to ignore certain power supplies (like peripheral batteries)
that might interfere with power state detection.

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

While reporting issues, please attach the results from `superfreq debug`.

## Contributing

Contributions to Superfreq are always welcome! Whether it's bug reports, feature
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

```bash
nix develop
```

Non-Nix users may get the appropriate Cargo and Rust versions from their package
manager, or using something like Rustup.

### Formatting & Lints

Please make sure to run _at least_ `cargo fmt` inside the repository to make
sure all of your code is properly formatted. For Nix code, please use Alejandra.

Clippy lints are not _required_ as of now, but a good rule of thumb to run them
before committing to catch possible code smell early.

## License

Superfreq is available under [Mozilla Public License v2.0](LICENSE) for your
convenience, and at our expense. Please see the license file for more details.
