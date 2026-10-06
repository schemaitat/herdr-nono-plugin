//! `herdr-nono describe`: the facts the docs test checks (action ids, config
//! keys, error kinds, constants) as JSON, so tests need no second copy of them.

use serde_json::{json, Value};

use crate::config::CONFIG_KEYS;
use crate::constants::{
    ACTION_IDS, LIFECYCLE_STATES, MIN_NONO_VERSION, NONO_BIN_ENV, PLUGIN_ID, RESULT_MARKER,
    RESULT_SCHEMA_VERSION, STATE_VERSION,
};
use crate::errors::ERROR_KINDS;

pub fn describe() -> Value {
    json!({
        "pluginId": PLUGIN_ID,
        "resultMarker": RESULT_MARKER,
        "resultSchemaVersion": RESULT_SCHEMA_VERSION,
        "nonoBinEnv": NONO_BIN_ENV,
        "minNonoVersion": MIN_NONO_VERSION,
        "stateVersion": STATE_VERSION,
        "actionIds": ACTION_IDS,
        "configKeys": CONFIG_KEYS,
        "errorKinds": ERROR_KINDS,
        "lifecycleStates": LIFECYCLE_STATES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_equals_the_snapshot_generated_from_the_js_modules() {
        let snapshot: Value =
            serde_json::from_str(include_str!("../tests/fixtures/describe.json")).unwrap();
        assert_eq!(describe(), snapshot);
    }

    #[test]
    fn action_ids_match_the_manifest_in_order() {
        let manifest =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/herdr-plugin.toml"))
                .unwrap();
        let ids: Vec<&str> = manifest
            .split("[[actions]]")
            .skip(1)
            .filter_map(|block| {
                block
                    .lines()
                    .find_map(|line| line.strip_prefix("id = \"")?.strip_suffix('"'))
            })
            .collect();
        assert_eq!(ids, ACTION_IDS);
    }
}
