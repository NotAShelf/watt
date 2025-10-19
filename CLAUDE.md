# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Watt is a modern CPU frequency and power management utility for Linux systems. It provides intelligent control of CPU governors, frequencies, and power-saving features, optimizing both performance and battery life. Inspired by auto-cpufreq but rewritten from the ground up with a focus on efficiency and correctness.

**Language:** Rust (edition 2024, requires Rust 1.85+)

## Build, Test, and Development Commands

### Building
```bash
# Build the project
cargo build

# Build with optimizations
cargo build --release
```

### Running
```bash
# Run directly with cargo (daemon mode requires sudo)
cargo run -- info

# Run daemon (requires sudo for most functionality)
sudo cargo run -- daemon

# Run with verbose logging
sudo cargo run -- daemon --verbose
```

### Code Quality
```bash
# Format code (REQUIRED before commits)
cargo fmt

# Run clippy lints (recommended but not required)
cargo clippy

# For Nix files, use Alejandra
nix fmt
```

### Nix Development
```bash
# Enter development shell
nix develop

# Build the package
nix build

# Run formatter on all Nix files
nix fmt
```

## Architecture

### Module Structure

The codebase is organized into focused modules:

- **`main.rs`**: CLI entry point, command parsing, and execution flow
- **`core.rs`**: Core data structures representing system state (`SystemReport`, `SystemInfo`, `CpuCoreInfo`, `BatteryInfo`, etc.)
- **`engine.rs`**: Decision engine that determines and applies CPU/power settings based on system state and configuration
- **`daemon.rs`**: Daemon loop with adaptive polling system
- **`monitor.rs`**: Collects system information and generates `SystemReport`
- **`cpu.rs`**: Low-level CPU control (governors, frequencies, turbo, EPP/EPB, platform profiles)
- **`battery.rs`**: Battery management and charge threshold control
- **`config/`**: Configuration loading and types
- **`util/`**: Utilities (sysfs interaction, error types)
- **`cli/`**: CLI-specific commands (e.g., debug output)

### Key Architectural Patterns

**Profile-Based Configuration**: Watt uses two profiles (charger/battery) that define CPU settings for different power states. The engine module selects the appropriate profile based on AC/battery status.

**Adaptive Polling**: The daemon implements sophisticated adaptive polling that adjusts the polling interval based on:
- Battery discharge rate
- System activity patterns (CPU usage, temperature volatility)
- Idle detection with progressive backoff
- User activity detection

**Dynamic Turbo Management**: When `turbo = "auto"` and `enable_auto_turbo = true`, Watt dynamically controls turbo boost using hysteresis to prevent rapid toggling:
- Enables turbo when CPU load exceeds `load_threshold_high`
- Disables turbo when load drops below `load_threshold_low`
- Maintains previous state between thresholds (hysteresis)
- Automatically disables turbo if temperature exceeds `temp_threshold_high`
- Maintains separate hysteresis state for AC vs battery power

**Error Handling**: The codebase uses custom error types (`AppError`, `ControlError`, `EngineError`) with thiserror for structured error handling. The `try_apply_feature` helper in engine.rs centralizes feature application and gracefully handles unsupported features.

### Configuration Flow

1. Configuration is loaded from TOML files (`/etc/xdg/watt/config.toml`, `/etc/watt.toml`, or `WATT_CONFIG` env var)
2. The `engine` module receives a `SystemReport` from `monitor` and `AppConfig`
3. Engine selects appropriate profile (charger/battery) based on power status
4. Engine applies settings via `cpu` and `battery` modules
5. Settings are written to sysfs (via `util/sysfs.rs`)

### State Management

- **Governor Override**: The `cpu` module maintains a governor override flag that can force a specific governor mode persistently
- **Turbo Hysteresis**: The `engine` module uses `TurboHysteresisStates` with separate state for AC and battery power to prevent rapid turbo toggling
- **System History**: The `daemon` module maintains `SystemHistory` tracking CPU usage, temperature, battery discharge rate, and user activity for adaptive polling

## Important Implementation Notes

### Sysfs Interaction
All hardware interaction happens through the Linux sysfs interface in `/sys/devices/system/cpu/` and `/sys/class/power_supply/`. The `util/sysfs` module provides safe read/write helpers.

### Permission Requirements
Most CPU management operations require root privileges. Commands like `set-governor`, `set-turbo`, etc. will fail with `PermissionDenied` if not run with sudo.

### Vendor-Specific Battery Support
Battery charge thresholds use vendor-specific sysfs paths. Currently supports:
- Standard Linux power_supply API
- Lenovo ThinkPad/IdeaPad
- ASUS laptops
- Huawei laptops

Adding new vendor support requires identifying the correct sysfs paths for that hardware.

### Platform Profile Support
Platform profiles (`set-platform-profile`) require ACPI platform profile support in the hardware/kernel. Not all systems support this feature.

## Common Gotchas

- The daemon enforces a minimum poll interval of 1 second to prevent busy loops, even if config specifies 0
- Turbo auto management maintains separate state for AC and battery power modes
- Battery discharge rate calculations require at least 30 seconds between measurements to avoid noise
- Adaptive polling uses weighted averaging (70% previous, 30% new) to smooth out interval changes
- Configuration validation happens at runtime, not parse time, for some settings (like turbo thresholds)
