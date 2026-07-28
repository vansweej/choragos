//! Configuration resolved from environment variables.

/// Runtime configuration for choragos.
///
/// Construct via [`from_getter`] (testable) or [`from_env`] (production).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Absolute path to the `ai-coding` monorepo checkout.
    pub ai_coding_monorepo: String,
    /// Default pipeline profile name (e.g. `"default"`).
    pub default_profile: String,
    /// Maximum number of plan-cycle attempts before giving up.
    pub max_attempts: u32,
    /// Telegram bot token for run notifications, if configured.
    pub telegram_bot_token: Option<String>,
    /// Telegram chat ID for run notifications, if configured.
    pub telegram_chat_id: Option<String>,
}

/// Resolves [`Config`] from an arbitrary key→value getter.
///
/// This is the testable core; production code calls [`from_env`] which
/// delegates here with a closure over [`std::env::var`].
///
/// # Errors
///
/// Returns [`crate::CoreError::MissingEnv`] when a required variable is
/// absent, or [`crate::CoreError::Message`] when `CHORAGOS_MAX_ATTEMPTS` is
/// present but cannot be parsed as a `u32`.
pub fn from_getter<F: Fn(&str) -> Option<String>>(get: F) -> Result<Config, crate::CoreError> {
    let ai_coding_monorepo = get("AI_CODING_MONOREPO")
        .ok_or_else(|| crate::CoreError::MissingEnv("AI_CODING_MONOREPO".to_string()))?;

    let default_profile = get("CHORAGOS_DEFAULT_PROFILE")
        .ok_or_else(|| crate::CoreError::MissingEnv("CHORAGOS_DEFAULT_PROFILE".to_string()))?;

    let max_attempts = match get("CHORAGOS_MAX_ATTEMPTS") {
        None => 3,
        Some(raw) => raw.parse::<u32>().map_err(|_| {
            crate::CoreError::Message(format!("CHORAGOS_MAX_ATTEMPTS is not a valid u32: {raw:?}"))
        })?,
    };

    let telegram_bot_token = get("TELEGRAM_BOT_TOKEN");
    let telegram_chat_id = get("TELEGRAM_CHAT_ID");

    Ok(Config {
        ai_coding_monorepo,
        default_profile,
        max_attempts,
        telegram_bot_token,
        telegram_chat_id,
    })
}

/// Resolves [`Config`] from the real process environment.
///
/// Delegates to [`from_getter`] with a closure over [`std::env::var`].
/// Excluded from coverage instrumentation because it performs real I/O.
#[cfg(not(tarpaulin_include))]
pub fn from_env() -> Result<Config, crate::CoreError> {
    from_getter(|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{from_getter, Config};
    use crate::CoreError;

    fn make_getter(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |name| map.get(name).map(|v| v.to_string())
    }

    fn full_map() -> HashMap<&'static str, &'static str> {
        let mut m = HashMap::new();
        m.insert("AI_CODING_MONOREPO", "/home/user/ai-coding");
        m.insert("CHORAGOS_DEFAULT_PROFILE", "default");
        m.insert("CHORAGOS_MAX_ATTEMPTS", "5");
        m.insert("TELEGRAM_BOT_TOKEN", "tok123");
        m.insert("TELEGRAM_CHAT_ID", "chat456");
        m
    }

    #[test]
    fn all_present_success() {
        let cfg = from_getter(make_getter(full_map())).expect("should succeed");
        assert_eq!(
            cfg,
            Config {
                ai_coding_monorepo: "/home/user/ai-coding".to_string(),
                default_profile: "default".to_string(),
                max_attempts: 5,
                telegram_bot_token: Some("tok123".to_string()),
                telegram_chat_id: Some("chat456".to_string()),
            }
        );
    }

    #[test]
    fn missing_ai_coding_monorepo_returns_error() {
        let mut m = full_map();
        m.remove("AI_CODING_MONOREPO");
        let err = from_getter(make_getter(m)).unwrap_err();
        match err {
            CoreError::MissingEnv(var) => assert_eq!(var, "AI_CODING_MONOREPO"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_default_profile_returns_error() {
        let mut m = full_map();
        m.remove("CHORAGOS_DEFAULT_PROFILE");
        let err = from_getter(make_getter(m)).unwrap_err();
        match err {
            CoreError::MissingEnv(var) => assert_eq!(var, "CHORAGOS_DEFAULT_PROFILE"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn non_numeric_max_attempts_returns_error() {
        let mut m = full_map();
        m.insert("CHORAGOS_MAX_ATTEMPTS", "not-a-number");
        let err = from_getter(make_getter(m)).unwrap_err();
        match err {
            CoreError::Message(msg) => assert!(msg.contains("CHORAGOS_MAX_ATTEMPTS")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn absent_optionals_yield_none_and_default_max_attempts() {
        let mut m = HashMap::new();
        m.insert("AI_CODING_MONOREPO", "/repo");
        m.insert("CHORAGOS_DEFAULT_PROFILE", "prod");
        let cfg = from_getter(make_getter(m)).expect("should succeed");
        assert_eq!(cfg.max_attempts, 3);
        assert!(cfg.telegram_bot_token.is_none());
        assert!(cfg.telegram_chat_id.is_none());
    }
}
