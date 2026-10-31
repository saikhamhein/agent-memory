//! Kept from v1: backup, frontmatter encoding and the JSON decoder.
mod common;

use agent_memory_server::{model, store::Store};
use common::{TempStore, content, make_writable};
use serde_json::{Value, json};

#[test]
fn backup_restores_store_and_refuses_overwrite() {
    let mut t = TempStore::new();
    let numbers: Value = serde_json::from_str("[18446744073709551617, 1e-07, -0.0]").unwrap();
    let meta =
        json!({"evidence": [{"source": "s", "locator": "l", "note": "n", "numbers": numbers}]});
    let raw = content("Saved", "Body မြန်မာ", &meta);
    t.store
        .create("live", "/global/saved.md", &raw, "token:test")
        .unwrap();
    let backup = t.path().join("backup.sqlite3");
    Store::backup(&t.db, &backup).unwrap();
    assert!(Store::backup(&t.db, &backup).is_err());
    let mnt = t.path().join("backup-mnt");
    let copy = Store::open(&backup, &mnt).unwrap();
    assert_eq!(
        copy.list("live", "/").unwrap(),
        t.store.list("live", "/").unwrap()
    );
    assert_eq!(
        copy.live_memories().unwrap(),
        t.store.live_memories().unwrap()
    );
    make_writable(&mnt);
}

#[test]
fn numeric_encoding_matches_python() {
    // Differential coverage against Python's json.dumps (memory.py's former encode()),
    // the frontmatter format of migrated memories: across binary64 exponents, signed zero, subnormals,
    // formatting boundaries, integers larger than u64/i128 and nested values.
    let script = r#"
import json, math, random, struct, sys
rng = random.Random(812)
values = [0.0, -0.0, 1e-7, 1e-5, 1e-4, 1e15, 1e16, 1e20, 1e23,
          5e-324, 1.7976931348623157e308, 18446744073709551617, -(2**130),
          {'nested': [1.2345678901234567, '\u0001 မြန်မာ 😀', {}, []]}]
for _ in range(5000):
    number = struct.unpack('!d', rng.getrandbits(64).to_bytes(8, 'big'))[0]
    if math.isfinite(number):
        values.append(number)
print(json.dumps({'value': values, 'encoded': json.dumps(values, ensure_ascii=False, sort_keys=True, indent=2)}, ensure_ascii=False))
"#;
    let output = std::process::Command::new("python3")
        .args(["-c", script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        model::encode(&data["value"]).unwrap(),
        data["encoded"].as_str().unwrap()
    );
    let overflowing: Value = serde_json::from_str("[1e999]").unwrap();
    assert!(model::encode(&overflowing).is_err());
}

#[test]
fn document_decoder_preserves_reserved_keys_and_bounds_nesting() {
    for text in [
        r#"{"$serde_json::private::Number":"123"}"#,
        r#"{"$serde_json::private::Number":"ordinary metadata","extra":true}"#,
        r#"[{"$serde_json::private::RawValue":{"$serde_json::private::Number":null}}]"#,
    ] {
        let decoded = model::decode(text).unwrap();
        // Independent typed decoders in Python verify ordinary objects survive.
        let encoded = model::encode(&decoded).unwrap();
        let check = std::process::Command::new("python3")
            .args([
                "-c",
                "import json,sys; assert json.loads(sys.argv[1]) == json.loads(sys.argv[2])",
                text,
                &encoded,
            ])
            .status()
            .unwrap();
        assert!(check.success());
    }
    assert!(model::decode(&format!("{}0{}", "[".repeat(130), "]".repeat(130))).is_err());
    assert!(model::decode("[1,]").is_err());
}
