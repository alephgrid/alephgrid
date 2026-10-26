//! 1.10: the frontend's protocol types are generated from the Rust types and must match.

#[test]
fn protocol_bindings_are_up_to_date() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/protocol.ts");
    let generated = rowfinity_server::protocol::bindings();
    if std::env::var("UPDATE_BINDINGS").is_ok() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "web/src/protocol.ts is out of date with the Rust types; run \
         `UPDATE_BINDINGS=1 cargo test -p rowfinity-server --test protocol` and commit the result"
    );
}

#[test]
fn every_op_has_a_result_type() {
    let ops = rowfinity_server::protocol::op_names();
    let results: Vec<String> = rowfinity_server::protocol::RESULTS.iter().map(|(o, _)| o.to_string()).collect();
    assert!(!ops.is_empty());
    assert_eq!(ops, results, "protocol::RESULTS must list every Op, in order");
}
