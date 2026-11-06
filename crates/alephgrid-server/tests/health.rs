//! Liveness, readiness and request ids for operators (9.13, docs/OPERATIONS.md).

mod common;

use std::sync::atomic::Ordering;

use common::TestServer;

#[tokio::test]
async fn liveness_answers_without_signing_in() {
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start_with(dir.path(), |c| c.auth.mode = alephgrid_server::auth::AuthMode::Accounts).await;
    let (status, body) = srv.get("/healthz").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "ok");
    // The API itself still needs a sign-in.
    assert_eq!(srv.get("/api/workbooks").await.0, 401);
    srv.stop().await;
}

#[tokio::test]
async fn readiness_reports_each_check_and_fails_when_the_data_directory_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start_with(dir.path(), |c| c.auth.mode = alephgrid_server::auth::AuthMode::Accounts).await;
    let (status, body) = srv.get("/readyz").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["checks"]["accepting_work"], "ok");
    assert_eq!(body["checks"]["data_dir_writable"], "ok");
    assert_eq!(body["checks"]["control_db"], "ok");
    assert_eq!(body["checks"]["disk_reserve"], "off", "tests run without a reserve");
    // Nothing about where or how big: the endpoint is public.
    let text = body.to_string();
    assert!(!text.contains(&dir.path().display().to_string()), "{text}");
    // The probe leaves nothing behind.
    let control = dir.path().join(".control");
    assert!(!std::fs::read_dir(&control).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ready-")));

    // Root ignores permissions, so the read-only half only means something for others.
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt;
        let before = std::fs::metadata(&control).unwrap().permissions();
        std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o555)).unwrap();
        let (status, body) = srv.get("/readyz").await;
        std::fs::set_permissions(&control, before).unwrap();
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["status"], "unavailable");
        assert_eq!(body["checks"]["data_dir_writable"], "failed");
        assert_eq!(body["checks"]["control_db"], "ok");
        // Liveness does no I/O and stays up.
        assert_eq!(srv.get("/healthz").await.0, 200);
    }
    assert_eq!(srv.get("/readyz").await.0, 200);
    srv.stop().await;
}

#[tokio::test]
async fn readiness_shows_a_released_disk_reserve_as_degraded_but_still_ready() {
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start_with(dir.path(), |c| c.disk_reserve = 1 << 20).await;
    let (status, body) = srv.get("/readyz").await;
    assert_eq!((status, body["checks"]["disk_reserve"].as_str()), (200, Some("held")), "{body}");
    assert_eq!(body["status"], "ok");
    // The disk filled up: the reserve is given back. Administrators must still reach the
    // server to free space, so it stays ready.
    assert!(srv.state.controller.disk_reserve.release());
    let (status, body) = srv.get("/readyz").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["checks"]["disk_reserve"], "released");
    srv.stop().await;
}

#[tokio::test]
async fn readiness_fails_once_shutdown_begins() {
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start(dir.path()).await;
    assert_eq!(srv.get("/readyz").await.0, 200);
    srv.state.shutting_down.store(true, Ordering::SeqCst);
    let (status, body) = srv.get("/readyz").await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["checks"]["accepting_work"], "failed");
    assert_eq!(srv.get("/healthz").await.0, 200);
    srv.state.shutting_down.store(false, Ordering::SeqCst);
    srv.stop().await;
}

#[tokio::test]
async fn every_response_carries_a_request_id() {
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start(dir.path()).await;
    let res = srv.http.get(srv.url("/api/config")).send().await.unwrap();
    let id = res.headers().get("x-request-id").expect("a request id").to_str().unwrap().to_string();
    assert_eq!(id.len(), 16, "{id}");
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    let other = srv.http.get(srv.url("/api/config")).send().await.unwrap();
    assert_ne!(other.headers()["x-request-id"].to_str().unwrap(), id, "ids are per request");

    // A proxy's own plain id is kept; anything else is replaced, so it cannot forge log lines.
    let kept = srv.http.get(srv.url("/healthz")).header("x-request-id", "caddy-1234.abc_d").send().await.unwrap();
    assert_eq!(kept.headers()["x-request-id"], "caddy-1234.abc_d");
    let replaced = srv.http.get(srv.url("/healthz")).header("x-request-id", "a b\"c").send().await.unwrap();
    let new = replaced.headers()["x-request-id"].to_str().unwrap();
    assert!(new.len() == 16 && new.bytes().all(|b| b.is_ascii_hexdigit()), "{new}");
    let long = "a".repeat(65);
    let replaced = srv.http.get(srv.url("/healthz")).header("x-request-id", &long).send().await.unwrap();
    assert_ne!(replaced.headers()["x-request-id"].to_str().unwrap(), long);
    // Errors carry one too.
    let missing = srv.http.get(srv.url("/api/workbooks/nope")).send().await.unwrap();
    assert!(missing.status().is_client_error());
    assert!(missing.headers().get("x-request-id").is_some());
    srv.stop().await;
}
