use std::collections::HashMap;

/// Variable source for interpolation. `.env` values take precedence over the
/// process environment (D29 — a synced config dir behaves identically on every
/// host regardless of ambient shell state).
pub(crate) struct Env {
    dotenv: HashMap<String, String>,
}

impl Env {
    pub(crate) fn new(dotenv: HashMap<String, String>) -> Self {
        Self { dotenv }
    }

    pub(crate) fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.dotenv.get(key) {
            return Some(v.clone());
        }
        std::env::var(key).ok()
    }
}

/// One unresolved `${...}` placeholder, surfaced when no value is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpolationError {
    pub file: String,
    pub var: String,
    pub message: String,
}

/// Substitute `${VAR}`, `${VAR:-default}`, `${VAR:?message}` over raw text.
/// `$$` escapes a literal `$`. Errors (undefined `${VAR}`, or a triggered
/// `${VAR:?msg}`) are accumulated into `errors` rather than failing fast.
pub(crate) fn interpolate(
    text: &str,
    env: &Env,
    file: &str,
    errors: &mut Vec<InterpolationError>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // `$$` → literal `$`
            Some('$') => {
                chars.next();
                out.push('$');
            }
            // `${...}`
            Some('{') => {
                chars.next(); // consume '{'
                let mut inner = String::new();
                let mut closed = false;
                while let Some(nc) = chars.next() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    inner.push(nc);
                }
                if closed {
                    out.push_str(&resolve_placeholder(&inner, env, file, errors));
                } else {
                    // unterminated — emit literally so it's visible, not silently dropped
                    out.push_str("${");
                    out.push_str(&inner);
                }
            }
            // bare `$` — literal
            _ => out.push('$'),
        }
    }
    out
}

fn resolve_placeholder(
    inner: &str,
    env: &Env,
    file: &str,
    errors: &mut Vec<InterpolationError>,
) -> String {
    if let Some(idx) = inner.find(":-") {
        // ${VAR:-default} — default when unset or empty
        let name = inner[..idx].trim();
        let default = &inner[idx + 2..];
        match env.get(name) {
            Some(v) if !v.is_empty() => v,
            _ => default.to_string(),
        }
    } else if let Some(idx) = inner.find(":?") {
        // ${VAR:?message} — error with message when unset or empty
        let name = inner[..idx].trim();
        let msg = inner[idx + 2..].trim();
        match env.get(name) {
            Some(v) if !v.is_empty() => v,
            _ => {
                let message = if msg.is_empty() {
                    "required variable is unset".to_string()
                } else {
                    msg.to_string()
                };
                errors.push(InterpolationError {
                    file: file.to_string(),
                    var: name.to_string(),
                    message,
                });
                String::new()
            }
        }
    } else {
        // ${VAR} — error when unset
        let name = inner.trim();
        match env.get(name) {
            Some(v) => v,
            None => {
                errors.push(InterpolationError {
                    file: file.to_string(),
                    var: name.to_string(),
                    message: "undefined variable".to_string(),
                });
                String::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn substitutes_defined_var() {
        let mut errs = vec![];
        let out = interpolate(
            "listen: ${HOST}:8080",
            &env(&[("HOST", "1.2.3.4")]),
            "f",
            &mut errs,
        );
        assert_eq!(out, "listen: 1.2.3.4:8080");
        assert!(errs.is_empty());
    }

    #[test]
    fn uses_default_when_unset() {
        let mut errs = vec![];
        let out = interpolate("${MISSING:-fallback}", &env(&[]), "f", &mut errs);
        assert_eq!(out, "fallback");
        assert!(errs.is_empty());
    }

    #[test]
    fn dotenv_beats_process_env_via_map() {
        // Env::get checks the dotenv map first; here the map supplies the value.
        let mut errs = vec![];
        let out = interpolate("${K}", &env(&[("K", "from-dotenv")]), "f", &mut errs);
        assert_eq!(out, "from-dotenv");
    }

    #[test]
    fn undefined_var_accumulates_error() {
        let mut errs = vec![];
        let out = interpolate("a ${NOPE} b ${ALSO_NOPE}", &env(&[]), "site.yml", &mut errs);
        assert_eq!(out, "a  b ");
        assert_eq!(errs.len(), 2);
        assert_eq!(errs[0].var, "NOPE");
        assert_eq!(errs[1].var, "ALSO_NOPE");
        assert_eq!(errs[0].file, "site.yml");
    }

    #[test]
    fn required_var_message() {
        let mut errs = vec![];
        interpolate("${TOKEN:?set the token}", &env(&[]), "f", &mut errs);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].message, "set the token");
    }

    #[test]
    fn double_dollar_escapes() {
        let mut errs = vec![];
        let out = interpolate("cost is $$5 not ${X:-0}", &env(&[]), "f", &mut errs);
        assert_eq!(out, "cost is $5 not 0");
    }
}
