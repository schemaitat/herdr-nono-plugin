//! Session naming. Every agent pane gets a stable name such as
//! `herdr-opencode-3f9a1c0b2d4e` that is passed to `nono run --name`, so
//! `nono ps` shows which pane a sandboxed process belongs to. Each launch in
//! the pane is its own nono session, but they all carry this name.

use crate::constants::SERVER_SESSION_SUFFIX;
use crate::errors::{ErrorKind, PluginError, Result};
use crate::util::{random_hex, sha256_hex};

/// Whether `name` matches `^[A-Za-z0-9][A-Za-z0-9.-]+$`, the pattern every
/// session name produced or accepted by the plugin must match.
pub fn is_session_name(name: &str) -> bool {
    let mut chars = name.chars();
    let starts_well = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric());
    let rest: Vec<char> = chars.collect();
    starts_well
        && !rest.is_empty()
        && rest
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
}

/// Lower-cases a label and replaces everything outside [a-z0-9] with hyphens.
pub fn slugify(value: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut pending_hyphen = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            pending_hyphen = false;
            slug.push(c);
        } else {
            pending_hyphen = true;
        }
    }
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

/// Fails when a name is not a plain session name.
pub fn assert_session_name(name: &str) -> Result<()> {
    if is_session_name(name) {
        Ok(())
    } else {
        Err(PluginError::new(
            ErrorKind::Config,
            format!("Invalid session name \"{name}\": use at least two characters, start with a letter or digit, and only use letters, digits, hyphens and periods."),
        ))
    }
}

/// The 12 hex characters that make a session name unique.
pub fn session_digest(local_path: &str, pane_id: Option<&str>, nonce_hex: &str) -> String {
    sha256_hex(&format!(
        "{local_path}\n{}\n{nonce_hex}",
        pane_id.unwrap_or("")
    ))[..12]
        .to_string()
}

/// Produces a fresh unique session name such as `herdr-opencode-3f9a1c0b2d4e`.
pub fn session_name_for(
    prefix: &str,
    agent_kind: &str,
    local_path: &str,
    pane_id: Option<&str>,
) -> Result<String> {
    let digest = session_digest(local_path, pane_id, &random_hex(8));
    let name = format!(
        "{}-{}-{digest}",
        slugify(prefix, "herdr"),
        slugify(agent_kind, "agent")
    );
    assert_session_name(&name)?;
    Ok(name)
}

/// Name of the nono session an `open-shell` pane runs under.
pub fn shell_session_name(session_name: &str) -> String {
    format!("{session_name}-shell")
}

/// Name of the nono session an agent's private server runs under.
pub fn server_session_name(session_name: &str) -> String {
    format!("{session_name}{SERVER_SESSION_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn is_digest(text: &str) -> bool {
        text.len() == 12
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn session_name_for_produces_valid_unique_names() {
        let first = session_name_for("herdr", "opencode", "/tmp/repo", Some("pane-1")).unwrap();
        let second = session_name_for("herdr", "opencode", "/tmp/repo", Some("pane-1")).unwrap();
        let digest = first
            .strip_prefix("herdr-opencode-")
            .expect("prefix and kind");
        assert!(is_digest(digest), "{first}");
        assert!(is_session_name(&first));
        assert_ne!(first, second);
    }

    #[test]
    fn session_name_for_honors_the_prefix_and_slugs_odd_agent_kinds() {
        let name = session_name_for("Team", "My Agent!!", "/x", None).unwrap();
        assert!(
            name.starts_with("team-my-agent-") && is_digest(&name["team-my-agent-".len()..]),
            "{name}"
        );
    }

    #[test]
    fn shell_and_server_sessions_carry_the_agent_name() {
        assert_eq!(
            shell_session_name("herdr-opencode-abc"),
            "herdr-opencode-abc-shell"
        );
        assert_eq!(
            server_session_name("herdr-opencode-abc"),
            "herdr-opencode-abc-server"
        );
    }

    #[test]
    fn slugify_cases() {
        for (input, expected) in [
            ("Claude Code", "claude-code"),
            ("--weird--", "weird"),
            ("", "agent"),
            ("UPPER_case.mixed", "upper-case-mixed"),
            ("ünï", "n"),
            ("a💥b", "a-b"),
        ] {
            assert_eq!(slugify(input, "agent"), expected, "{input:?}");
        }
        assert_eq!(slugify("!!", "herdr"), "herdr");
    }

    #[test]
    fn assert_session_name_rejects_names_that_are_not_plain_words() {
        for bad in ["a", "-abc", "has space", "under_score", ""] {
            let error = assert_session_name(bad).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Config);
            assert!(error.message.contains("Invalid session name"));
        }
        assert!(assert_session_name("ok-name.1").is_ok());
    }

    #[test]
    fn digests_equal_the_js_golden_vectors() {
        let golden: Value =
            serde_json::from_str(include_str!("../tests/fixtures/golden.json")).unwrap();
        for vector in golden["sessionDigest"].as_array().unwrap() {
            assert_eq!(
                session_digest(
                    vector["localPath"].as_str().unwrap(),
                    vector["paneId"].as_str(),
                    vector["nonce"].as_str().unwrap()
                ),
                vector["digest"].as_str().unwrap()
            );
        }
    }
}
