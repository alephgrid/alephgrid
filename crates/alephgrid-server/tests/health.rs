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
    // Probes at the same time each use their own file, so none reports a false failure.
    let probes = futures_util::future::join_all((0..32).map(|_| srv.get("/readyz"))).await;
    assert!(probes.iter().all(|(status, b)| *status == 200 && b["checks"]["data_dir_writable"] == "ok"), "{probes:?}");
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

// ------------------------------------------------------------------ metrics

/// One sample of the text exposition: name, labels, value.
type Sample = (String, Vec<(String, String)>, f64);

fn samples(text: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (series, value) = line.rsplit_once(' ').unwrap_or_else(|| panic!("no value: {line}"));
        let value: f64 = value.parse().unwrap_or_else(|_| panic!("bad value: {line}"));
        let (name, labels) = match series.split_once('{') {
            None => (series.to_string(), vec![]),
            Some((name, rest)) => {
                let body = rest.strip_suffix('}').unwrap_or_else(|| panic!("unclosed labels: {line}"));
                let mut labels = Vec::new();
                let mut rest = body;
                while !rest.is_empty() {
                    let (k, v) = rest.split_once("=\"").unwrap_or_else(|| panic!("bad label: {line}"));
                    let end = v.find('"').unwrap_or_else(|| panic!("unterminated label: {line}"));
                    labels.push((k.to_string(), v[..end].to_string()));
                    rest = v[end + 1..].trim_start_matches(',');
                }
                (name.to_string(), labels)
            }
        };
        out.push((name, labels, value));
    }
    out
}

fn value(samples: &[Sample], name: &str, labels: &[(&str, &str)]) -> f64 {
    samples
        .iter()
        .find(|(n, l, _)| n == name && l.len() == labels.len() && labels.iter().all(|(k, v)| l.iter().any(|(a, b)| a == k && b == v)))
        .unwrap_or_else(|| panic!("no sample {name}{labels:?}"))
        .2
}

async fn scrape(srv: &TestServer) -> (String, Vec<Sample>) {
    let res = srv.http.get(srv.url("/metrics")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "text/plain; version=0.0.4; charset=utf-8");
    let text = res.text().await.unwrap();
    let s = samples(&text);
    (text, s)
}

#[tokio::test]
async fn metrics_need_an_instance_admin_or_the_metrics_token() {
    use reqwest::Method;
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start_accounts(dir.path(), |c| c.metrics_token = Some("scrape-token-0123456789".into())).await;
    let get = |s: common::Session| async move {
        let mut req = s.http.get(s.url("/metrics"));
        if let Some(c) = &s.cookie {
            req = req.header("Cookie", format!("alephgrid_session={c}"));
        }
        if let Some(b) = &s.bearer {
            req = req.header("Authorization", format!("Bearer {b}"));
        }
        let res = req.send().await.unwrap();
        (res.status().as_u16(), res.text().await.unwrap())
    };
    assert_eq!(get(srv.anonymous()).await.0, 401);
    assert_eq!(get(srv.bearer("scrape-token-0123456788")).await.0, 401, "a near miss is no token");
    let (status, text) = get(srv.bearer("scrape-token-0123456789")).await;
    assert_eq!(status, 200, "{text}");
    assert!(text.contains(&format!("alephgrid_build_info{{version=\"{}\"}} 1\n", env!("CARGO_PKG_VERSION"))), "{text}");
    let admin = srv.admin().await;
    assert_eq!(get(admin.clone()).await.0, 200);

    // A signed-in member who is not an instance admin, and a service token, are refused.
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let code = admin.invite(&org, "member@example.com", None, None).await;
    let member = srv.accept(&code, "Member", "member password 123").await;
    assert_eq!(get(member).await.0, 403);
    let space = admin.ok(Method::POST, "/api/spaces", Some(serde_json::json!({ "name": "Ops" }))).await["id"].as_str().unwrap().to_string();
    let token = admin.ok(Method::POST, &format!("/api/spaces/{space}/tokens"), Some(serde_json::json!({ "name": "t", "role": "viewer", "days": 30 }))).await;
    assert_eq!(get(srv.bearer(token["secret"].as_str().unwrap())).await.0, 403);
    srv.stop().await;

    // Without a metrics token, only instance admins.
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start_accounts(dir.path(), |_| {}).await;
    assert_eq!(get(srv.bearer("scrape-token-0123456789")).await.0, 401);
    assert_eq!(get(srv.admin().await).await.0, 200);
    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_follow_workbook_lifecycle_events_with_bounded_labels() {
    use reqwest::Method;
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start(dir.path()).await;
    let (_, m) = scrape(&srv).await;
    assert_eq!(value(&m, "alephgrid_allocation_starts_total", &[]), 0.0);
    assert_eq!(value(&m, "alephgrid_workbooks_open", &[]), 0.0);
    assert_eq!(value(&m, "alephgrid_ready", &[]), 1.0);
    assert_eq!(value(&m, "alephgrid_disk_reserve_state", &[("state", "off")]), 1.0);
    assert_eq!(value(&m, "alephgrid_memory_budget_bytes", &[]), (1u64 << 30) as f64, "the test server's budget");
    assert_eq!(value(&m, "alephgrid_last_backup_timestamp_seconds", &[]), 0.0, "no backup yet");

    // A new workbook opens at once.
    let name = "Payroll of Ann Lee";
    let wb = srv.create_workbook(name).await;
    let (_, m) = scrape(&srv).await;
    assert_eq!(value(&m, "alephgrid_allocation_starts_total", &[]), 1.0);
    assert_eq!(value(&m, "alephgrid_allocation_start_seconds_count", &[]), 1.0);
    assert_eq!(value(&m, "alephgrid_workbooks_open", &[]), 1.0);
    assert_eq!(value(&m, "alephgrid_allocations", &[("state", "ready")]), 1.0);
    assert!(value(&m, "alephgrid_memory_reserved_bytes", &[]) > 0.0);
    let bulk_ok = value(&m, "alephgrid_jobs_total", &[("class", "bulk"), ("outcome", "ok")]);
    let bulk_writes = value(&m, "alephgrid_job_wait_seconds_count", &[("class", "bulk"), ("lane", "write")]);

    // An import: 14 + 9 + 9 = 32 bytes, one bulk write job.
    let (status, body) = srv.upload(&wb, "sales.csv", b"region,amount\nnorth,10\nsouth,20\n".to_vec()).await;
    assert_eq!(status, 200, "{body}");
    let (_, m) = scrape(&srv).await;
    assert_eq!(value(&m, "alephgrid_imports_total", &[("result", "ok")]), 1.0);
    assert_eq!(value(&m, "alephgrid_imports_total", &[("result", "failed")]), 0.0);
    assert_eq!(value(&m, "alephgrid_import_bytes_total", &[]), 32.0);
    assert_eq!(value(&m, "alephgrid_jobs_total", &[("class", "bulk"), ("outcome", "ok")]), bulk_ok + 1.0);
    assert_eq!(value(&m, "alephgrid_job_wait_seconds_count", &[("class", "bulk"), ("lane", "write")]), bulk_writes + 1.0);
    assert_eq!(value(&m, "alephgrid_job_duration_seconds_count", &[("class", "bulk"), ("lane", "write")]), bulk_writes + 1.0);

    // A WebSocket session and a query (a bulk read).
    let mut ws = srv.ws(&wb).await;
    let (_, m) = scrape(&srv).await;
    assert_eq!(value(&m, "alephgrid_websocket_connections", &[]), 1.0);
    assert_eq!(value(&m, "alephgrid_websocket_connections_total", &[]), 1.0);
    let reads = value(&m, "alephgrid_job_duration_seconds_count", &[("class", "bulk"), ("lane", "read")]);
    let q = ws.ok("query", serde_json::json!({ "sql": "SELECT region FROM sales WHERE amount > 15" })).await;
    assert_eq!(q["rows"], serde_json::json!([["south"]]));
    let (_, m) = scrape(&srv).await;
    assert_eq!(value(&m, "alephgrid_job_duration_seconds_count", &[("class", "bulk"), ("lane", "read")]), reads + 1.0);
    assert_eq!(value(&m, "alephgrid_jobs_waiting", &[("lane", "read")]), 0.0);
    assert_eq!(value(&m, "alephgrid_jobs_running", &[]), 0.0);
    ws.close().await;
    let mut open = 1.0;
    for _ in 0..200 {
        open = value(&scrape(&srv).await.1, "alephgrid_websocket_connections", &[]);
        if open == 0.0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(open, 0.0, "the session ended");

    // Stopping its compute.
    let (status, _) = srv.send(Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(serde_json::json!({ "action": "stop" }))).await;
    assert_eq!(status, 200);
    let (text, m) = scrape(&srv).await;
    assert_eq!(value(&m, "alephgrid_allocation_stops_total", &[("reason", "requested")]), 1.0);
    assert_eq!(value(&m, "alephgrid_allocation_stops_total", &[("reason", "evicted")]), 0.0);
    assert_eq!(value(&m, "alephgrid_workbooks_open", &[]), 0.0);
    assert_eq!(value(&m, "alephgrid_allocations", &[("state", "ready")]), 0.0);

    // Every label is from a fixed set; nothing names the workbook, its data or the disk.
    let allowed: &[(&str, &[&str])] = &[
        ("reason", &["incompatible", "capacity", "quota", "owned_elsewhere", "startup_failed", "cancelled", "lost", "deleted", "evicted", "requested", "shutdown", "restarting"]),
        ("state", &["requested", "starting", "running", "ready", "draining", "held", "released", "off"]),
        ("class", &["interactive", "bulk"]),
        ("lane", &["read", "write"]),
        ("outcome", &["ok", "failed", "cancelled", "deadline_exceeded", "rejected"]),
        ("result", &["ok", "failed"]),
        ("version", &[env!("CARGO_PKG_VERSION")]),
        ("le", &["0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "2.5", "5", "10", "30", "60", "300", "900", "3600", "+Inf"]),
    ];
    for (name, labels, _) in &m {
        assert!(name.starts_with("alephgrid_"), "{name}");
        for (k, v) in labels {
            let values = allowed.iter().find(|(key, _)| key == k).unwrap_or_else(|| panic!("unexpected label {k} on {name}")).1;
            assert!(values.contains(&v.as_str()), "unexpected {k}=\"{v}\" on {name}");
        }
    }
    for leak in [wb.as_str(), name, "sales", "south", &dir.path().display().to_string()] {
        assert!(!text.contains(leak), "{leak} in metrics");
    }
    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_starts_are_counted_by_reason() {
    use alephgrid_server::runtime::{AllocationState, FailureKind, RuntimeFailure};
    use reqwest::Method;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let f = fail.clone();
    let hook: alephgrid_server::runtime::controller::StageHook = Arc::new(move |_: &str, stage: AllocationState| {
        if stage == AllocationState::Starting && f.load(Ordering::SeqCst) {
            return Err(RuntimeFailure::new(FailureKind::StartupFailed, "injected startup failure"));
        }
        Ok(())
    });
    let srv = TestServer::start_with(dir.path(), |c| {
        c.stage_hook = Some(hook);
        c.quota.open_workbooks = Some(1);
    })
    .await;
    let runtime = |wb: &str, action: &str| {
        let (path, body) = (format!("/api/workbooks/{wb}/runtime"), serde_json::json!({ "action": action }));
        let srv = &srv;
        async move { srv.send(Method::POST, &path, Some(body)).await }
    };
    let a = srv.create_workbook("a").await;
    assert_eq!(runtime(&a, "stop").await.0, 200);
    let b = srv.create_workbook("b").await;
    // The space may have one workbook open: b is.
    let (status, body) = runtime(&a, "start").await;
    assert!(status >= 400, "{body}");
    assert_eq!(runtime(&b, "stop").await.0, 200);
    fail.store(true, Ordering::SeqCst);
    let (status, body) = runtime(&a, "start").await;
    assert!(status >= 400, "{body}");

    let (_, m) = scrape(&srv).await;
    // a and b created (2), a refused by the quota, a failing to open.
    assert_eq!(value(&m, "alephgrid_allocation_starts_total", &[]), 4.0);
    assert_eq!(value(&m, "alephgrid_allocation_start_failures_total", &[("reason", "quota")]), 1.0);
    assert_eq!(value(&m, "alephgrid_allocation_start_failures_total", &[("reason", "startup_failed")]), 1.0);
    assert_eq!(value(&m, "alephgrid_allocation_start_failures_total", &[("reason", "capacity")]), 0.0);
    assert_eq!(value(&m, "alephgrid_allocation_start_seconds_count", &[]), 2.0, "two starts reached ready");
    assert_eq!(value(&m, "alephgrid_allocation_stops_total", &[("reason", "requested")]), 2.0);
    assert_eq!(value(&m, "alephgrid_workbooks_open", &[]), 0.0);
    fail.store(false, Ordering::SeqCst);
    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_sets_the_last_backup_time() {
    use reqwest::Method;
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start(dir.path()).await;
    srv.create_workbook("w").await;
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let (status, body) = srv.send(Method::POST, "/api/admin/backups", None).await;
    assert_eq!(status, 200, "{body}");
    let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let at = value(&scrape(&srv).await.1, "alephgrid_last_backup_timestamp_seconds", &[]);
    assert!(at >= before.floor() && at <= after, "{before} <= {at} <= {after}");
    srv.stop().await;
}

// -------------------------------------------------------------- diagnostics

#[tokio::test(flavor = "multi_thread")]
async fn the_diagnostics_bundle_holds_no_secrets_names_or_contents() {
    use alephgrid_server::auth::oidc::{OidcConfig, OidcJoin};
    use reqwest::Method;
    alephgrid_server::diagnostics::init_recent_logs_only();
    let oidc = || OidcConfig {
        issuer: "https://idp.secret-corp.example".into(),
        client_id: "client-id-SECRET-555".into(),
        client_secret: Some("oidc-client-SECRET-999".into()),
        scopes: "openid email".into(),
        groups_claim: "groups".into(),
        label: "SSO".into(),
        join: OidcJoin::Invite,
        redirect_uri: "https://grid.secret-corp.example/api/auth/oidc/callback".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start_accounts(dir.path(), |c| {
        c.metrics_token = Some("metrics-token-SECRET-777".into());
        c.auth.oidc = Some(oidc());
    })
    .await;
    let admin = srv.admin().await;
    let space = admin.ok(Method::POST, "/api/spaces", Some(serde_json::json!({ "name": "Space SECRET-SPACE" }))).await["id"].as_str().unwrap().to_string();
    let wb = admin.ok(Method::POST, "/api/workbooks", Some(serde_json::json!({ "name": "Payroll SECRET-NAME", "space": space }))).await["id"].as_str().unwrap().to_string();
    let (s, v) = admin.send_bytes(Method::POST, &format!("/api/workbooks/{wb}/import?filename=secret-file.csv"), b"who,pay\nCELL-SECRET-123,5\n".to_vec()).await;
    assert_eq!(s, 200, "{v}");
    let service = admin.ok(Method::POST, &format!("/api/spaces/{space}/tokens"), Some(serde_json::json!({ "name": "t", "role": "viewer", "days": 30 }))).await["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let agent = admin.ok(Method::POST, "/api/agents", Some(serde_json::json!({ "name": "a", "workbooks": [wb], "days": 7 }))).await["secret"].as_str().unwrap().to_string();
    // Even a careless log line is cut before it goes into the bundle.
    tracing::warn!(target: "alephgrid_server::diagnostics_test", "diagnostics test: metrics-token-SECRET-777 {service} {} member@example.com", common::ADMIN_EMAIL);

    let anon = srv.anonymous();
    assert_eq!(anon.get("/api/admin/diagnostics").await.0, 401);
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let code = admin.invite(&org, "member@example.com", None, None).await;
    let member = srv.accept(&code, "Member", "member password 123").await;
    assert_eq!(member.get("/api/admin/diagnostics").await.0, 403);

    let res = admin.http.get(admin.url("/api/admin/diagnostics")).header("Cookie", format!("alephgrid_session={}", admin.cookie.clone().unwrap())).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.headers()["content-disposition"].to_str().unwrap().starts_with("attachment; filename=\"alephgrid-diagnostics-"));
    let text = res.text().await.unwrap();
    let bundle: serde_json::Value = serde_json::from_str(&text).unwrap();

    let secrets = [
        common::ADMIN_PASSWORD,
        common::ADMIN_EMAIL,
        "member@example.com",
        "metrics-token-SECRET-777",
        "oidc-client-SECRET-999",
        "client-id-SECRET-555",
        "secret-corp",
        service.as_str(),
        &service[4..],
        agent.as_str(),
        &agent[4..],
        admin.cookie.as_deref().unwrap(),
        "SECRET-NAME",
        "SECRET-SPACE",
        "CELL-SECRET-123",
        "secret-file",
        &dir.path().display().to_string(),
    ];
    for s in secrets {
        assert!(!text.contains(s), "{s} is in the bundle");
    }
    // Log lines keep workbook and space ids (random, they match the server's own log);
    // nothing else does.
    let mut facts = bundle.clone();
    facts.as_object_mut().unwrap().remove("logs");
    let facts = facts.to_string();
    for id in [wb.as_str(), space.as_str()] {
        assert!(!facts.contains(id), "{id} is in the bundle");
    }
    // What it does say.
    assert_eq!(bundle["source"], "server");
    assert_eq!(bundle["build"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(bundle["config"]["auth"]["mode"], "accounts");
    assert_eq!(bundle["config"]["auth"]["bootstrap_admin"], "set");
    assert_eq!(bundle["config"]["auth"]["oidc"]["client_secret"], "set");
    assert_eq!(bundle["config"]["metrics_token"], "set");
    assert_eq!(bundle["health"]["ready"], true);
    assert_eq!(bundle["storage"]["workbooks"], 1);
    assert_eq!(bundle["accounts"]["users"], 2, "the admin and the member");
    assert_eq!(bundle["runtime"]["allocations"]["ready"], 1);
    assert!(bundle["metrics"].as_str().unwrap().contains("alephgrid_imports_total{result=\"ok\"} 1\n"));
    let lines = bundle["logs"]["lines"].as_array().unwrap();
    assert!(
        lines.iter().any(|l| l.as_str().unwrap().contains("diagnostics test: [redacted] agt_[redacted] [redacted] [email]")),
        "{lines:?}"
    );

    // The same from the data directory alone, while the server runs and after it stopped.
    let offline = |c: &mut alephgrid_server::state::Config| {
        c.auth.mode = alephgrid_server::auth::AuthMode::Accounts;
        c.auth.bootstrap = Some((common::ADMIN_EMAIL.into(), common::ADMIN_PASSWORD.into()));
        c.auth.oidc = Some(oidc());
        c.metrics_token = Some("metrics-token-SECRET-777".into());
    };
    let mut config = alephgrid_server::state::Config::new(dir.path());
    offline(&mut config);
    let running = alephgrid_server::diagnostics::offline(&config);
    assert_eq!(running["health"]["server_running"], true);
    srv.stop().await;
    let stopped = alephgrid_server::diagnostics::offline(&config);
    assert_eq!(stopped["health"]["server_running"], false);
    assert_eq!(stopped["health"]["data_dir_writable"], true);
    assert_eq!(stopped["source"], "offline");
    assert_eq!(stopped["storage"]["workbooks"], 1);
    // Storage format 9 is this build's (alephgrid_core::storage::FORMAT).
    assert_eq!(stopped["storage"]["workbooks_by_storage_format"], serde_json::json!({ "9": 1 }));
    assert_eq!(stopped["config"]["auth"]["oidc"]["client_secret"], "set");
    assert_eq!(stopped["logs"]["lines"], serde_json::json!([]));
    let text = stopped.to_string();
    for s in secrets.iter().chain(&[wb.as_str(), space.as_str()]) {
        assert!(!text.contains(s), "{s} is in the offline bundle");
    }
}
