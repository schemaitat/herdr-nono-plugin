//! `herdr-nono describe`: the facts the docs test checks (action ids, config
//! keys, error kinds, constants) as JSON, so tests need no second copy of them.

use serde_json::{json, Value};

use crate::agents::builtin_agents;
use crate::config::CONFIG_KEYS;
use crate::constants::{
    ACTION_IDS, LIFECYCLE_STATES, MIN_NONO_VERSION, NONO_BIN_ENV, PLUGIN_ID, RESULT_MARKER,
    RESULT_SCHEMA_VERSION, STATE_VERSION,
};
use crate::errors::ERROR_KINDS;
use crate::probes::LOOPBACK_NAMES;

pub fn describe() -> Value {
    let agents: serde_json::Map<String, Value> = builtin_agents()
        .into_iter()
        .map(|(kind, adapter)| (kind, json!({"profile": adapter.profile, "serverProfile": adapter.server.map(|server| server.profile)})))
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
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
        "loopbackNames": LOOPBACK_NAMES,
        "builtinAgents": agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_equals_the_snapshot() {
        // The snapshot began as the output of the Node modules this crate replaced; fields added since are written
        // by hand. The version changes with every release, so it is checked against the crate, not the snapshot.
        let mut snapshot: Value =
            serde_json::from_str(include_str!("../tests/fixtures/describe.json")).unwrap();
        snapshot["version"] = json!(env!("CARGO_PKG_VERSION"));
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
