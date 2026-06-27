use std::path::PathBuf;

use super::blueprint::ValidationError;
use super::resolve::InterpolationError;

/// Top-level config-pipeline error. The accumulating variants (`Interpolation`,
/// `Validation`) carry *every* problem found in that stage, rendered together.
#[derive(Debug)]
pub enum ConfigError {
    NoConfig,
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    EnvFile {
        path: PathBuf,
        source: dotenvy::Error,
    },
    Parse {
        path: Option<PathBuf>,
        source: serde_yaml_ng::Error,
    },
    Interpolation(Vec<InterpolationError>),
    Validation(Vec<ValidationError>),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoConfig => write!(f, "no config files provided"),
            Self::Read { path, source } => {
                write!(f, "reading config {}: {}", path.display(), source)
            }
            Self::EnvFile { path, source } => {
                write!(f, "reading env file {}: {}", path.display(), source)
            }
            Self::Parse { path, source } => match path {
                Some(p) => write!(f, "parsing config {}: {}", p.display(), source),
                None => write!(f, "parsing merged config: {}", source),
            },
            Self::Interpolation(errors) => {
                writeln!(f, "{} interpolation error(s):", errors.len())?;
                for e in errors {
                    writeln!(f, "  - {} ({}): {}", e.var, e.file, e.message)?;
                }
                Ok(())
            }
            Self::Validation(errors) => {
                writeln!(f, "{} config validation error(s):", errors.len())?;
                for e in errors {
                    writeln!(f, "  - {}: {}", e.path, e.message)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::EnvFile { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}
