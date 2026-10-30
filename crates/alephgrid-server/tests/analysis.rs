//! No-code operations use the existing scheduling, revision and idempotency envelope.
mod common;
use common::*;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn no_code_analysis_and_replacement_work_without_arbitrary_sql() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.allow_sql = false).await;
    let wb = server.create_workbook("No SQL").await;
    assert_eq!(server.upload(&wb, "data.csv", b"name,n\nAda,1\nAda,2\nLin,3\n".to_vec()).await.0, 200);
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    let spec = json!({"kind":"summary","table":"data","groups":["name"],"aggregate":"sum","value":"n","pivot":null});
    assert_eq!(a.ok("analyze", json!({"spec":spec})).await["result"]["rows"], json!([["Ada", 3], ["Lin", 3]]));
    assert!(a.call("query", json!({"sql":"SELECT * FROM data"})).await.is_err());
    let saved = a.ok("save_analysis", json!({"saved":null,"name":"Names","spec":spec,"chart":true})).await;
    b.event("resync").await.unwrap();
    let replace = json!({"table":"data","column":"name","find":"Ada","replacement":"Grace"});
    let preview = a.ok("replace_preview", json!({"spec":replace})).await;
    let params = json!({"spec":replace,"revision":preview["sample"]["revision"],"mutation_id":"replace-once"});
    let id = a.send("replace_apply", params.clone()).await;
    let first = a.reply(id).await.unwrap();
    assert_eq!(first["ok"], true, "{first}");
    b.event("table_changed").await.unwrap();
    let id = a.send("replace_apply", params).await;
    let replay = a.reply(id).await.unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"], first["revision"]);
    a.close().await;
    b.close().await;
    server.stop().await;
    let server = TestServer::start_with(dir.path(), |c| c.allow_sql = false).await;
    let mut a = server.ws(&wb).await;
    assert_eq!(a.ok("analyses", json!({})).await[0]["id"], saved["id"]);
    a.ok("replace_undo", json!({"revision":first["result"]["undo_revision"]})).await;
    assert_eq!(a.ok("analyze", json!({"spec":saved["spec"]})).await["result"]["rows"], json!([["Ada", 3], ["Lin", 3]]));
    a.close().await;
    server.stop().await;
}
