use std::io;

#[derive(Debug)]
pub enum ControlError {
    Io(io::Error),
    WriteError(String),
    ReadError(String),
    InvalidValueError(String),
    NotSupported(String),
    PermissionDenied(String),
    InvalidProfile(String),
    InvalidGovernor(String),
    ParseError(String),
    PathMissing(String),
}

impl From<io::Error> for ControlError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied(err.to_string()),
            _ => Self::Io(err),
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::WriteError(s) => write!(f, "Failed to write to sysfs path: {s}"),
            Self::ReadError(s) => write!(f, "Failed to read sysfs path: {s}"),
            Self::InvalidValueError(s) => write!(f, "Invalid value for setting: {s}"),
            Self::NotSupported(s) => write!(f, "Control action not supported: {s}"),
            Self::PermissionDenied(s) => {
                write!(f, "Permission denied: {s}. Try running with sudo.")
            }
            Self::InvalidProfile(s) => {
                write!(
                    f,
                    "Invalid platform control profile {s} supplied, please provide a valid one."
                )
            }
            Self::InvalidGovernor(s) => {
                write!(f, "Invalid governor: {s}")
            }
            Self::ParseError(s) => {
                write!(f, "Failed to parse value: {s}")
            }
            Self::PathMissing(s) => {
                write!(f, "Path missing: {s}")
            }
        }
    }
}

impl std::error::Error for ControlError {}

#[derive(Debug)]
pub enum SysMonitorError {
    Io(io::Error),
    ReadError(String),
    ParseError(String),
    ProcStatParseError(String),
}

impl From<io::Error> for SysMonitorError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl std::fmt::Display for SysMonitorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::ReadError(s) => write!(f, "Failed to read sysfs path: {s}"),
            Self::ParseError(s) => write!(f, "Failed to parse value: {s}"),
            Self::ProcStatParseError(s) => {
                write!(f, "Failed to parse /proc/stat: {s}")
            }
        }
    }
}

impl std::error::Error for SysMonitorError {}

#[derive(Debug)]
pub enum EngineError {
    ControlError(ControlError),
    ConfigurationError(String),
}

impl From<ControlError> for EngineError {
    fn from(err: ControlError) -> Self {
        Self::ControlError(err)
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControlError(e) => write!(f, "CPU control error: {e}"),
            Self::ConfigurationError(s) => write!(f, "Configuration error: {s}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ControlError(e) => Some(e),
            Self::ConfigurationError(_) => None,
        }
    }
}

// A unified error type for the entire application
#[derive(Debug)]
pub enum AppError {
    Control(ControlError),
    Monitor(SysMonitorError),
    Engine(EngineError),
    Config(crate::config::ConfigError),
    Generic(String),
    Io(io::Error),
}

impl From<ControlError> for AppError {
    fn from(err: ControlError) -> Self {
        Self::Control(err)
    }
}

impl From<SysMonitorError> for AppError {
    fn from(err: SysMonitorError) -> Self {
        Self::Monitor(err)
    }
}

impl From<EngineError> for AppError {
    fn from(err: EngineError) -> Self {
        Self::Engine(err)
    }
}

impl From<crate::config::ConfigError> for AppError {
    fn from(err: crate::config::ConfigError) -> Self {
        Self::Config(err)
    }
}

impl From<io::Error> for AppError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<String> for AppError {
    fn from(err: String) -> Self {
        Self::Generic(err)
    }
}

impl From<&str> for AppError {
    fn from(err: &str) -> Self {
        Self::Generic(err.to_string())
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Control(e) => write!(f, "{e}"),
            Self::Monitor(e) => write!(f, "{e}"),
            Self::Engine(e) => write!(f, "{e}"),
            Self::Config(e) => write!(f, "{e}"),
            Self::Generic(s) => write!(f, "{s}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Control(e) => Some(e),
            Self::Monitor(e) => Some(e),
            Self::Engine(e) => Some(e),
            Self::Config(e) => Some(e),
            Self::Generic(_) => None,
            Self::Io(e) => Some(e),
        }
    }
}
