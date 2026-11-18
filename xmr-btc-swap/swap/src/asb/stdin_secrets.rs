//! Secrets the GUI hands the bundled ASB over stdin instead of the command line or the
//! environment, where other processes of the same user can read them (audit 2026-11-10, M-6).
//!
//! With `--seed-stdin` the payload is the base64 seed on the first line and, optionally, the
//! Dojo API key on the second. A seed-only payload (older GUIs) still works.

use std::sync::OnceLock;

/// The seed (base64) and the Dojo API key, if the payload had one.
#[derive(Debug, PartialEq, Eq)]
pub struct StdinSecrets {
    pub seed_base64: String,
    pub dojo_api_key: Option<String>,
}

pub fn parse(payload: &str) -> StdinSecrets {
    let mut lines = payload.lines().map(str::trim);
    let seed_base64 = lines.next().unwrap_or("").to_string();
    let dojo_api_key = lines.next().filter(|key| !key.is_empty()).map(str::to_string);
    StdinSecrets { seed_base64, dojo_api_key }
}

static DOJO_API_KEY: OnceLock<String> = OnceLock::new();

/// Remember the Dojo API key read from stdin for the Dojo set-up later in this process.
pub fn remember_dojo_api_key(key: Option<String>) {
    if let Some(key) = key {
        let _ = DOJO_API_KEY.set(key);
    }
}

pub fn dojo_api_key() -> Option<String> {
    DOJO_API_KEY.get().cloned()
}

/// The key from stdin wins; the environment variable is the fallback for an ASB started by hand.
pub fn choose_dojo_api_key(from_stdin: Option<String>, from_env: Option<String>) -> Option<String> {
    from_stdin
        .filter(|key| !key.trim().is_empty())
        .or_else(|| from_env.filter(|key| !key.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_and_key_are_read_from_two_lines() {
        assert_eq!(
            parse("c2VlZA==\ndojo-key\n"),
            StdinSecrets { seed_base64: "c2VlZA==".into(), dojo_api_key: Some("dojo-key".into()) }
        );
        assert_eq!(parse("c2VlZA==\r\ndojo-key").dojo_api_key.as_deref(), Some("dojo-key"));
    }

    #[test]
    fn a_seed_only_payload_still_works() {
        assert_eq!(
            parse("c2VlZA=="),
            StdinSecrets { seed_base64: "c2VlZA==".into(), dojo_api_key: None }
        );
        assert_eq!(parse("c2VlZA==\n\n").dojo_api_key, None);
    }

    #[test]
    fn the_key_from_stdin_wins_over_the_environment() {
        assert_eq!(
            choose_dojo_api_key(Some("stdin".into()), Some("env".into())).as_deref(),
            Some("stdin")
        );
        assert_eq!(choose_dojo_api_key(None, Some("env".into())).as_deref(), Some("env"));
        assert_eq!(choose_dojo_api_key(Some(" ".into()), None), None);
        assert_eq!(choose_dojo_api_key(None, None), None);
    }
}
