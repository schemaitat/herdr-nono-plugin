//! The machine-readable result contract: the first stdout line of every action
//! is `HERDR_SANDBOX_RESULT: {json}` so orchestrators polling
//! `herdr plugin log list` can parse it without scraping human text.

use std::io::Write;

use serde_json::{json, Map, Value};

use crate::constants::{PLUGIN_ID, RESULT_MARKER, RESULT_SCHEMA_VERSION};
use crate::errors::PluginError;

/// Formats a result payload as the marker line. `schemaVersion` and `plugin`
/// come first; a payload field of the same name replaces the value in place.
pub fn format_result_line(payload: &Map<String, Value>) -> String {
    let mut object = Map::new();
    object.insert("schemaVersion".into(), json!(RESULT_SCHEMA_VERSION));
    object.insert("plugin".into(), json!(PLUGIN_ID));
    for (key, value) in payload {
        object.insert(key.clone(), value.clone());
    }
    format!("{RESULT_MARKER} {}", Value::Object(object))
}

/// Writes the marker line followed by optional human-readable lines.
pub fn emit_result(payload: &Map<String, Value>, extra_lines: &[String]) {
    let mut out = std::io::stdout().lock();
    // A closed stdout is not something the action can report anywhere.
    let _ = writeln!(out, "{}", format_result_line(payload));
    for line in extra_lines {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();
}

/// Builds the failure payload for an error. The error's own payload fields come
/// first, and `action`, `ok`, `errorKind` and `message` cannot be overridden.
pub fn failure_payload(action: Option<&str>, error: &PluginError) -> Map<String, Value> {
    let mut payload = error.payload.clone().unwrap_or_default();
    payload.insert(
        "action".into(),
        action.map_or(Value::Null, |action| json!(action)),
    );
    payload.insert("ok".into(), json!(false));
    payload.insert("errorKind".into(), json!(error.kind_str()));
    payload.insert("message".into(), json!(error.message));
    let output = error.output.trim();
    if !output.is_empty() {
        payload.insert("output".into(), json!(truncate_utf16(output, 4000)));
    }
    payload
}

/// `String#slice(0, units)`: at most `units` UTF-16 code units.
fn truncate_utf16(text: &str, units: usize) -> String {
    let encoded: Vec<u16> = text.encode_utf16().take(units).collect();
    String::from_utf16_lossy(&encoded)
}

/// Extracts the parsed result payload from captured stdout, or `None` when absent.
#[cfg(test)]
pub fn parse_result_line(text: &str) -> Result<Option<Value>, serde_json::Error> {
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix(RESULT_MARKER) {
            return serde_json::from_str(rest.trim()).map(Some);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn format_and_parse_round_trip() {
        let line = format_result_line(&object(
            json!({"action": "info", "ok": true, "sessionName": "s"}),
        ));
        assert!(line.starts_with(&format!("{RESULT_MARKER} {{")));
        assert_eq!(
            line,
            r#"HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"info","ok":true,"sessionName":"s"}"#
        );
        let parsed = parse_result_line(&format!("noise\n{line}\nmore"))
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed,
            json!({"schemaVersion": 1, "plugin": "nono.sandbox", "action": "info", "ok": true, "sessionName": "s"})
        );
        assert_eq!(parse_result_line("nothing here").unwrap(), None);
        assert!(parse_result_line(&format!("{RESULT_MARKER} {{broken")).is_err());
    }

    #[test]
    fn failure_payload_carries_the_error_kind_trimmed_output_and_extra_fields() {
        let error = PluginError::new(ErrorKind::NotFound, "gone")
            .with_output("  nono: Session not found  ");
        let payload = failure_payload(Some("stop"), &error);
        assert_eq!(
            Value::Object(payload),
            json!({"action": "stop", "ok": false, "errorKind": "not-found", "message": "gone", "output": "nono: Session not found"})
        );
        let unknown = PluginError::new(ErrorKind::Unknown, "boom");
        assert_eq!(
            Value::Object(failure_payload(None, &unknown)),
            json!({"action": null, "ok": false, "errorKind": "unknown", "message": "boom"})
        );
        let with_report = PluginError::new(ErrorKind::Unconfined, "bad")
            .with_payload(object(json!({"verified": false, "ok": true})));
        let payload = failure_payload(Some("verify-sandbox"), &with_report);
        assert_eq!(payload["verified"], json!(false));
        assert_eq!(
            payload["ok"],
            json!(false),
            "the payload cannot override ok"
        );
        assert_eq!(payload["errorKind"], json!("unconfined"));
        // Field order follows the JS spread: payload fields keep their place.
        assert_eq!(
            payload.keys().collect::<Vec<_>>(),
            ["verified", "ok", "action", "errorKind", "message"]
        );
    }

    #[test]
    fn output_is_capped_at_4000_utf16_units() {
        let error = PluginError::new(ErrorKind::Unknown, "x").with_output("a".repeat(5000));
        assert_eq!(
            failure_payload(None, &error)["output"]
                .as_str()
                .unwrap()
                .len(),
            4000
        );
    }
}
