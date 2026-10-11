//! The JSON Schemas in `schemas/v1` must match the output types. Set RESCYO_WRITE_SCHEMAS=1 to
//! regenerate them after an intentional, backwards-compatible change.

use std::path::PathBuf;

#[test]
fn json_schemas_match_the_committed_contract() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v1");
    let write = std::env::var("RESCYO_WRITE_SCHEMAS").as_deref() == Ok("1");
    if write {
        std::fs::create_dir_all(&dir).unwrap();
    }
    for (name, schema) in crate::dto::schemas() {
        let path = dir.join(format!("{name}.json"));
        let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
        if write {
            std::fs::write(&path, &text).unwrap();
        } else {
            let committed = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                committed, text,
                "schema {name} changed: review compatibility, then rerun with RESCYO_WRITE_SCHEMAS=1"
            );
        }
    }
}
