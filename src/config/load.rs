use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_yaml_ng::Value;

use super::blueprint::Blueprint;
use super::error::ConfigError;
use super::merge::merge_values;
use super::model::Config;
use super::resolve::{Env, InterpolationError, interpolate};

/// Load, resolve, merge, and validate one or more config files into a `Blueprint`.
///
/// `.env` (next to the first file) is auto-loaded and takes precedence over the
/// process environment. Files are merged left-to-right (later wins; lists append).
pub fn load(paths: &[PathBuf]) -> Result<Blueprint, ConfigError> {
    let first = paths.first().ok_or(ConfigError::NoConfig)?;
    let env = load_env(first)?;

    // Pass 1: read + interpolate every file; accumulate all interpolation errors.
    let mut interp_errors: Vec<InterpolationError> = Vec::new();
    let mut resolved: Vec<(PathBuf, String)> = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let label = path.display().to_string();
        let text = interpolate(&raw, &env, &label, &mut interp_errors);
        resolved.push((path.clone(), text));
    }
    if !interp_errors.is_empty() {
        return Err(ConfigError::Interpolation(interp_errors));
    }

    // Pass 2: parse each file to a YAML value and merge left-to-right.
    let mut merged: Option<Value> = None;
    for (path, text) in resolved {
        let value: Value = serde_yaml_ng::from_str(&text).map_err(|source| ConfigError::Parse {
            path: Some(path),
            source,
        })?;
        merged = Some(match merged {
            None => value,
            Some(acc) => merge_values(acc, value),
        });
    }
    let value = merged.ok_or(ConfigError::NoConfig)?;

    // Deserialize the merged document, then validate-by-conversion.
    let config: Config = serde_yaml_ng::from_value(value)
        .map_err(|source| ConfigError::Parse { path: None, source })?;
    Blueprint::try_from(config).map_err(ConfigError::Validation)
}

/// Load `.env` sitting next to the first config file (if any). Parsed with
/// `dotenvy` purely as a parser — it never touches `std::env`.
fn load_env(first: &Path) -> Result<Env, ConfigError> {
    let dir = first.parent().filter(|p| !p.as_os_str().is_empty());
    let env_path = match dir {
        Some(d) => d.join(".env"),
        None => PathBuf::from(".env"),
    };

    let mut map = HashMap::new();
    if env_path.exists() {
        let iter = dotenvy::from_path_iter(&env_path).map_err(|source| ConfigError::EnvFile {
            path: env_path.clone(),
            source,
        })?;
        for item in iter {
            let (k, v) = item.map_err(|source| ConfigError::EnvFile {
                path: env_path.clone(),
                source,
            })?;
            map.insert(k, v);
        }
    }
    Ok(Env::new(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LogFormat;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from("fixtures/config").join(name)
    }

    #[test]
    fn loads_merges_and_interpolates() {
        let bp = load(&[fixture("base.yml"), fixture("override.yml")]).unwrap();

        // listen comes from ${LISTEN} in base.yml, resolved via fixtures/config/.env
        assert_eq!(bp.listen.to_string(), "127.0.0.1:9999");
        // log_format only set in override.yml
        assert_eq!(bp.observability.log_format, LogFormat::Pretty);
        // backends append across files
        assert_eq!(bp.backends.len(), 2);

        let api = bp.backends.iter().find(|b| b.name == "api").unwrap();
        assert!(api.cache.enabled);
        assert_eq!(api.rate_limit.rps, 100);

        // web backend exists only in override.yml
        assert!(bp.backends.iter().any(|b| b.name == "web"));
    }

    #[test]
    fn log_level_resolves_from_env_override() {
        // loglevel.yml uses ${EDGEPROXY_TEST_LOG:-info}; the value comes from
        // fixtures/config/.env, proving the YAML-default + env-override path.
        let bp = load(&[fixture("loglevel.yml")]).unwrap();
        assert_eq!(bp.observability.log_level, "trace");
    }

    #[test]
    fn missing_file_is_read_error() {
        let err = load(&[fixture("does-not-exist.yml")]).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    }

    #[test]
    fn undefined_var_is_interpolation_error() {
        let err = load(&[fixture("undefined-var.yml")]).unwrap_err();
        match err {
            ConfigError::Interpolation(errs) => {
                assert!(errs.iter().any(|e| e.var == "EDGEPROXY_TEST_UNDEFINED_XYZ"));
            }
            other => panic!("expected interpolation error, got {other}"),
        }
    }

    #[test]
    fn no_paths_is_no_config() {
        let err = load(&[]).unwrap_err();
        assert!(matches!(err, ConfigError::NoConfig));
    }
}
