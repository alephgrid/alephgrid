//! Phase 5 wire-level durability, idempotency and collaboration contract.
mod common;
use common::*;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn publish_retries_once_notifies_other_clients_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("Functions").await;
    let mut writer = server.ws(&wb).await;
    let mut observer = server.ws(&wb).await;
    let definition = json!({"name":"ADDONE","kind":"formula","arguments":["value"],"source":"value+1","documentation":"Add one","tests":[{"arguments":["3"],"expected":"4"}]});
    let request = json!({"definition":definition,"mutation_id":"publish-1"});
    let id = writer.send("publish_function", request.clone()).await;
    let first = writer.reply(id).await.unwrap();
    assert_eq!(first["ok"], true, "{first}");
    observer.event("resync").await.expect("other clients refresh their function state");
    let id = writer.send("publish_function", request).await;
    let retry = writer.reply(id).await.unwrap();
    assert_eq!(retry["replayed"], true);
    assert_eq!(retry["revision"], first["revision"]);
    assert_eq!(writer.ok("functions", json!({})).await[0]["versions"].as_array().unwrap().len(), 1);
    writer.ok("sheet_set", json!({"sheet":0,"inputs":[{"row":0,"column":0,"value":"=ADDONE(3)"}]})).await;
    let before = writer.ok("history", json!({})).await;
    assert!(writer.call("pin_functions", json!({"pins":{"ADDONE":999}})).await.is_err());
    assert_eq!(writer.ok("history", json!({})).await, before);
    writer.close().await;
    observer.close().await;
    server.stop().await;
    let server = TestServer::start(dir.path()).await;
    let mut ws = server.ws(&wb).await;
    assert_eq!(ws.ok("functions", json!({})).await[0]["pinned"], 1);
    let cell = ws.ok("sheet_fetch", json!({"sheet":0,"row_start":0,"row_count":1,"column_count":1})).await;
    assert_eq!(cell["rows"][0][0][0], "4");
    ws.close().await;
    server.stop().await;
}

/// U.06c/U.06e over the wire: typed test values round trip, and `function_usage` says what
/// activating a version affects before it is activated.
#[tokio::test(flavor = "multi_thread")]
async fn typed_tests_and_usage_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("Usage").await;
    let mut ws = server.ws(&wb).await;
    let tests = json!([
        { "arguments": [{ "type": "blank" }], "expected": "blank" },
        { "arguments": [{ "type": "text", "value": "" }], "expected": { "type": "text", "value": "other" } },
        { "arguments": [{ "type": "error", "value": "#N/A" }], "expected": "other" },
    ]);
    let definition = json!({"name":"SHAPE","kind":"formula","arguments":["value"],"source":"IF(ISBLANK(value),\"blank\",\"other\")","documentation":"","tests":tests});
    let results = ws.ok("test_function", json!({ "definition": definition })).await;
    assert_eq!(results[0]["actual"], "blank");
    assert_eq!(results[1]["kind"], "text");
    // ISBLANK of an error value is FALSE, so the error reaches IF as "other".
    assert_eq!(results.as_array().unwrap().iter().map(|r| r["passed"].as_bool().unwrap()).collect::<Vec<_>>(), [true, true, true], "{results}");
    ws.ok("publish_function", json!({ "definition": definition })).await;
    assert_eq!(ws.ok("functions", json!({})).await[0]["versions"][0]["definition"]["tests"], tests);
    ws.ok("sheet_set", json!({"sheet":0,"inputs":[{"row":2,"column":3,"value":"=SHAPE(A1)"}]})).await;
    let usage = ws.ok("function_usage", json!({ "name": "SHAPE" })).await;
    assert_eq!(usage["cell_count"], 1);
    assert_eq!(usage["cells"][0]["cell"], "D3");
    assert_eq!(usage["columns"], json!([]));
    assert_eq!(usage["dependents"], json!([]));
    let unknown = ws.call("function_usage", json!({ "name": "MISSING" })).await.unwrap_err();
    assert!(unknown.contains("unknown function"), "{unknown}");
    ws.close().await;
    server.stop().await;
}
