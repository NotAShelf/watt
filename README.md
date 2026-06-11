<!--markdownlint-disable MD013 MD033 MD041-->

<div align="center">
  <h1 id="header">
    <pre>Watt</pre>
  </h1>
  <div>
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
  <div>
    Modern, transparent and intelligent utility for CPU management on Linux.
  </div>
  <div>
    <br/>
    <a href="#watt-is-it">Synopsis</a><br/>
    <a href="#features">Features</a> | <a href="#usage">Usage</a> | <a href="#configuration">Configuration</a><br/>
    <a href="#contributing">Contributing</a>
    <br/>
  </div>
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
- **Rule-Based Power Policy**: Evaluate declarative TOML rules against live
  system state, then apply the highest-priority matching settings.
- **CPU Management**: Monitor and control CPU governors, frequencies, turbo
  boost, EPP/EPB, Intel P-State limits and PM QoS latency controls.
- **Device Power Controls**: Tune power-related settings for batteries, ACPI
  platform profiles, disks, USB devices, audio codecs, GPUs, Intel uncore
  frequency and VM memory policy.
- **Conflict Detection**: Identifies and warns about conflicts with other power
  management tools.
- **Powerful Config DSL**: Built on TOML, Watt features a robust configuration
  DSL for conditions, arithmetic, availability checks, target selection and
  fallback values.

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

## Configuration

Watt uses a rule-based TOML configuration system that allows you to define power
management policies. Each `[[rule]]` has an optional condition, a unique
priority and one or more action sections. Watt evaluates matching rules from
highest to lowest priority; higher-priority values fill each setting first, and
lower-priority rules only fill settings that remain unset.

### Configuration Locations

Configuration sources, in order of precedence:

- Custom path via `--config` flag.
- Custom path via `WATT_CONFIG` environment variable (if no flag is specified).
- Built-in default configuration (if no custom path is specified).

<!--markdownlint-disable MD059 -->

[watt crate]: ./watt/config.toml

If no custom config path is provided, Watt uses a built-in default
configuration. You can find the default configuration in the [watt crate].

<!--markdownlint-enable MD059 -->

Rules can target CPU cores, power supplies, Intel uncore devices, disks, USB
devices and GPUs with `*.for` selectors where the subsystem has named devices.
Global sections such as `vm`, `audio`, `disk.alpm` and `power.platform-profile`
apply once per polling cycle.

For the full rule syntax, expression reference and every supported action, see
[Configuring Watt](docs/configuring.md).

### Example

```toml
[[rule]]
if.all = [ "?discharging", { is-less-than = 0.3, value = "%power-supply-charge" } ]
name     = "battery-preservation"
priority = 90

cpu.governor = { first-available-governor = ["powersave", "schedutil"] }
cpu.energy-performance-preference = { first-available-energy-performance-preference = ["power", "balance_power"] }
cpu.turbo = { if = "?turbo-available", then = false }

power.platform-profile = { first-available-platform-profile = ["low-power", "quiet"] }
power.charge-threshold-start = 40
power.charge-threshold-end = 80

usb.autosuspend = true
audio.timeout-seconds = 10
gpu.panel-power-savings = { if.is-chassis-type = "laptop", then = 3 }
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
- Disk APM and spindown settings require `hdparm` and compatible drives
- GPU panel power savings require compatible AMDGPU panel sysfs support

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

You will need Cargo and Rust installed on your system. Rust 1.88 or later is
required. For [direnv](https://direnv.net) users, a `.envrc` is provided, and
it's usage is encouraged. Especially if you are a Nix user. Alternatively, you
may use Nix directly for a reproducible developer environment

```sh
# Enter a dev shell with necessary dependencies.
$ nix develop
```

Non-Nix users may get the appropriate Cargo and Rust versions from their package
manager, or using something like Rustup.

### Formatting & Lints

Please make sure to run _at least_ `cargo fmt` (and `taplo format` if you have
modified any TOML) inside the repository to make sure all of your code is
properly formatted. For Nix code, use Alejandra.

Clippy lints are not _required_ as of now, but a good rule of thumb to run them
before committing to catch possible code smell early.

## License

Watt is available under [Mozilla Public License v2.0](LICENSE) for your
convenience, and at our expense. Please see the license file for more details.
