//! Quoting helpers for the command string typed into a Herdr pane shell.
//! The string is executed by the user's interactive shell, so it must be valid
//! for bash, zsh and fish alike. `env KEY=VALUE cmd` is used instead of the
//! `KEY=VALUE cmd` prefix form because fish does not support the latter.

use crate::errors::{ErrorKind, PluginError, Result};

/// A bare word may not start with `=` (zsh expands `=cmd`) or `%` (fish expands `%job`);
/// `~` is never in the set, so home-directory expansion cannot happen either.
fn is_safe_word(text: &str) -> bool {
    let mut chars = text.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || "_/.:@+,-".contains(c));
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || "_/.:=@%+,-".contains(c))
}

/// Quotes one word for a POSIX or fish shell.
pub fn shell_quote(text: &str) -> Result<String> {
    for c in text.chars() {
        let code = c as u32;
        if code < 32 || code == 127 {
            return Err(PluginError::new(
                ErrorKind::Target,
                "A path or argument contains control characters and cannot be typed into the pane shell.",
            ));
        }
        if code == 92 {
            return Err(PluginError::new(
                ErrorKind::Target,
                format!(
                    "A path or argument contains a backslash ({}), which bash and fish quote differently. Rename it before using it with this plugin.",
                    serde_json::Value::String(text.to_string())
                ),
            ));
        }
    }
    if text.is_empty() {
        return Ok("''".to_string());
    }
    if is_safe_word(text) {
        return Ok(text.to_string());
    }
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Builds the single-line command Herdr submits into a pane.
pub fn build_pane_command(argv: &[String], env: &[(String, String)]) -> Result<String> {
    if argv.is_empty() {
        return Err(PluginError::new(
            ErrorKind::Startup,
            "buildPaneCommand needs a non-empty argv.",
        ));
    }
    let mut words = Vec::new();
    if !env.is_empty() {
        words.push("env".to_string());
        for (key, value) in env {
            if !is_env_name(key) {
                return Err(PluginError::new(
                    ErrorKind::Startup,
                    format!("Invalid environment variable name for the pane command: {key}"),
                ));
            }
            words.push(format!("{key}={}", shell_quote(value)?));
        }
    }
    for word in argv {
        words.push(shell_quote(word)?);
    }
    Ok(words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_cases() {
        for (input, expected) in [
            ("simple", "simple"),
            ("/abs/path-1.2:x", "/abs/path-1.2:x"),
            ("has space", "'has space'"),
            ("it's", "'it'\\''s'"),
            ("", "''"),
            ("=ls", "'=ls'"),
            ("%1", "'%1'"),
            ("a=b", "a=b"),
            ("50%", "50%"),
            ("~", "'~'"),
        ] {
            assert_eq!(shell_quote(input).unwrap(), expected, "{input:?}");
        }
    }

    #[test]
    fn shell_quote_refuses_control_characters_and_backslashes() {
        let error = shell_quote("a\nb").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Target);
        let error = shell_quote("a\\b").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Target);
        assert!(
            error.message.contains("backslash") && error.message.contains(r#""a\\b""#),
            "{}",
            error.message
        );
        assert!(shell_quote("a\u{7f}").is_err());
    }

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn build_pane_command_uses_env_for_fish_compatibility_and_quotes_words() {
        let command = build_pane_command(
            &owned(&["/usr/bin/node", "/plugin root/src/bridge.mjs", "start"]),
            &[("HERDR_AGENT".into(), "opencode".into())],
        )
        .unwrap();
        assert_eq!(
            command,
            "env HERDR_AGENT=opencode /usr/bin/node '/plugin root/src/bridge.mjs' start"
        );
        assert_eq!(build_pane_command(&owned(&["ls"]), &[]).unwrap(), "ls");
        let bad_name =
            build_pane_command(&owned(&["ls"]), &[("bad-name".into(), "x".into())]).unwrap_err();
        assert!(bad_name
            .message
            .contains("Invalid environment variable name"));
        let empty = build_pane_command(&[], &[]).unwrap_err();
        assert!(empty.message.contains("non-empty argv"));
    }
}
