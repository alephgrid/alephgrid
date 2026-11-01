//! Phase 7 (7.01–7.06, 7.17; 7.T01): organizations and spaces, sign-in, roles enforced on
//! every HTTP route and WebSocket operation, cross-space ids, revocation, service tokens,
//! browser protections, single sign-on and the migration from workspaces.
//!
//! Expected permissions are written out here by hand from docs/TEAMS.md; they are not read
//! from the server's own tables.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use common::*;
use reqwest::Method;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PASSWORD: &str = "a long enough password";

fn csv() -> Vec<u8> {
    b"n,s\n1,a\n2,b\n3,c\n".to_vec()
}

/// An organization with spaces Alpha and Beta, a workbook with table `t` in each, and users
/// with each role in Alpha plus an organization member with no space role.
struct Team {
    server: TestServer,
    admin: Session,
    org: String,
    alpha: String,
    beta: String,
    wa: String,
    wb: String,
    viewer: Session,
    editor: Session,
    owner: Session,
    outsider: Session,
}

async fn team(dir: &std::path::Path) -> Team {
    team_with(dir, |_| {}).await
}

async fn team_with(dir: &std::path::Path, configure: impl FnOnce(&mut alephgrid_server::state::Config)) -> Team {
    let server = TestServer::start_accounts(dir, configure).await;
    let admin = server.admin().await;
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let alpha = admin.ok(Method::POST, "/api/spaces", Some(json!({ "name": "Alpha" }))).await["id"].as_str().unwrap().to_string();
    let beta = admin.ok(Method::POST, "/api/spaces", Some(json!({ "name": "Beta" }))).await["id"].as_str().unwrap().to_string();
    let mut wbs = Vec::new();
    for (name, space) in [("wa", &alpha), ("wb", &beta)] {
        let id = admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": name, "space": space }))).await["id"].as_str().unwrap().to_string();
        let (s, v) = admin.send_bytes(Method::POST, &format!("/api/workbooks/{id}/import?filename=t.csv"), csv()).await;
        assert_eq!(s, 200, "{v}");
        wbs.push(id);
    }
    let mut people = Vec::new();
    for (email, role) in [("viewer@example.com", Some("viewer")), ("editor@example.com", Some("editor")), ("owner@example.com", Some("owner")), ("outsider@example.com", None)] {
        let code = admin.invite(&org, email, role.map(|_| alpha.as_str()), role).await;
        people.push(server.accept(&code, email.split('@').next().unwrap(), PASSWORD).await);
    }
    let outsider = people.pop().unwrap();
    let owner = people.pop().unwrap();
    let editor = people.pop().unwrap();
    let viewer = people.pop().unwrap();
    Team { server, admin, org, alpha, beta, wa: wbs[0].clone(), wb: wbs[1].clone(), viewer, editor, owner, outsider }
}

/// A refusal of access: 401, 403, or the "not found" a principal gets for what it cannot see.
/// (A 404 for an object that is simply missing — an upload, a download, a member — is an
/// answer to an allowed request.)
fn refused(status: u16, v: &Value) -> bool {
    let msg = v["error"].as_str().unwrap_or("");
    let hidden = (msg.starts_with("workbook ") && msg.ends_with(" not found")) || msg == "space not found" || msg == "organization not found";
    matches!(status, 401 | 403) || (status == 404 && hidden)
}

/// Every HTTP route answers each principal as docs/TEAMS.md says (7.T01).
#[tokio::test(flavor = "multi_thread")]
async fn every_http_route_follows_the_role_table() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let (wa, alpha, org) = (t.wa.clone(), t.alpha.clone(), t.org.clone());
    let spec = common::urlencode(&json!({ "table": "t" }).to_string());
    // (method, path, body, who may: viewer < editor < owner, or org/instance admins only)
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Least {
        /// Any signed-in principal (what it sees is filtered).
        Anyone,
        Viewer,
        Editor,
        Owner,
        OrgAdmin,
        /// An admin of the space's organization; others in the space are refused, outsiders
        /// do not see it.
        SpaceOrgAdmin,
        InstanceAdmin,
    }
    use Least::*;
    /// Method, path, JSON body, raw body, who may.
    type Row = (Method, String, Option<Value>, Option<Vec<u8>>, Least);
    let admin_id = t.admin.user_id();
    let other = t.viewer.user_id();
    let rows: Vec<Row> = vec![
        (Method::GET, "/api/me".into(), None, None, Anyone),
        (Method::GET, "/api/workbooks".into(), None, None, Anyone),
        (Method::GET, "/api/spaces".into(), None, None, Anyone),
        (Method::GET, "/api/trash".into(), None, None, Anyone),
        (Method::DELETE, "/api/trash".into(), None, None, Anyone),
        (Method::GET, "/api/orgs".into(), None, None, Anyone),
        (Method::POST, "/api/spaces".into(), Some(json!({ "name": "" })), None, Anyone),
        (Method::GET, format!("/api/workbooks/{wa}/downloads/none"), None, None, Viewer),
        (Method::POST, format!("/api/workbooks/{wa}/uploads/none/preview"), Some(json!({})), None, Editor),
        (Method::POST, format!("/api/workbooks/{wa}/uploads/none/import"), Some(json!({})), None, Editor),
        (Method::DELETE, format!("/api/workbooks/{wa}/uploads/none"), None, None, Editor),
        (Method::POST, format!("/api/trash/{wa}/restore"), None, None, Editor),
        (Method::DELETE, format!("/api/trash/{wa}"), None, None, Owner),
        (Method::DELETE, format!("/api/spaces/{alpha}/members/group/nobody"), None, None, Owner),
        (Method::POST, format!("/api/spaces/{alpha}/tokens"), Some(json!({ "name": "t", "role": "viewer", "days": 0 })), None, Owner),
        (Method::DELETE, format!("/api/spaces/{alpha}/tokens/none"), None, None, Owner),
        (Method::DELETE, format!("/api/spaces/{alpha}"), None, None, Owner),
        (Method::PATCH, format!("/api/orgs/{org}/members/{admin_id}"), Some(json!({ "role": "member" })), None, OrgAdmin),
        (Method::DELETE, format!("/api/orgs/{org}/members/nobody"), None, None, OrgAdmin),
        (Method::POST, format!("/api/orgs/{org}/invites"), Some(json!({ "email": "not an email" })), None, OrgAdmin),
        (Method::DELETE, format!("/api/orgs/{org}/invites/none"), None, None, OrgAdmin),
        (Method::PATCH, format!("/api/users/{other}"), Some(json!({ "disabled": false })), None, InstanceAdmin),
        (Method::POST, "/api/admin/backups".into(), None, None, InstanceAdmin),
        (Method::GET, format!("/api/orgs/{org}/usage"), None, None, OrgAdmin),
        (Method::GET, "/api/admin/usage".into(), None, None, InstanceAdmin),
        (Method::PUT, format!("/api/spaces/{alpha}/quota"), Some(json!({})), None, SpaceOrgAdmin),
        (Method::GET, format!("/api/spaces/{alpha}/library"), None, None, Viewer),
        (Method::GET, format!("/api/spaces/{alpha}/library/none"), None, None, Viewer),
        (Method::PATCH, format!("/api/spaces/{alpha}/library/none"), Some(json!({})), None, Viewer),
        (Method::POST, format!("/api/spaces/{alpha}/library"), Some(json!({ "workbook": wa, "kind": "query", "object": "none" })), None, Editor),
        (Method::GET, format!("/api/workbooks/{wa}/shareable"), None, None, Viewer),
        (Method::POST, format!("/api/workbooks/{wa}/library"), Some(json!({ "entry": "none" })), None, Editor),
        (Method::GET, format!("/api/workbooks/{wa}"), None, None, Viewer),
        (Method::GET, format!("/api/workbooks/{wa}/runtime"), None, None, Viewer),
        (Method::POST, format!("/api/workbooks/{wa}/runtime"), Some(json!({ "action": "start" })), None, Viewer),
        (Method::GET, format!("/api/workbooks/{wa}/profiles"), None, None, Viewer),
        (Method::PUT, format!("/api/spaces/{alpha}/profiles"), Some(json!({ "profiles": [] })), None, SpaceOrgAdmin),
        (Method::GET, format!("/api/workbooks/{wa}/export?spec={spec}&format=csv"), None, None, Viewer),
        (Method::GET, format!("/api/workbooks/{wa}/export.xlsx"), None, None, Viewer),
        (Method::GET, format!("/api/workbooks/{wa}/jobs/nothing"), None, None, Viewer),
        (Method::GET, format!("/api/spaces/{alpha}/members"), None, None, Viewer),
        (Method::PATCH, format!("/api/workbooks/{wa}"), Some(json!({ "starred": true })), None, Editor),
        (Method::POST, format!("/api/workbooks/{wa}/duplicate"), None, None, Editor),
        (Method::POST, format!("/api/workbooks/{wa}/uploads?filename=u.csv"), None, Some(csv()), Editor),
        (Method::POST, format!("/api/workbooks/{wa}/import?filename=i.csv"), None, Some(csv()), Editor),
        (Method::DELETE, format!("/api/workbooks/{wa}/jobs/nothing"), None, None, Editor),
        (Method::POST, format!("/api/workbooks/{wa}/runtime"), Some(json!({ "action": "cancel" })), None, Editor),
        (Method::POST, "/api/workbooks".into(), Some(json!({ "name": "new", "space": alpha })), None, Editor),
        (Method::PUT, format!("/api/spaces/{alpha}/members"), Some(json!({ "kind": "group", "principal": "analysts", "role": "viewer" })), None, Owner),
        (Method::GET, format!("/api/spaces/{alpha}/tokens"), None, None, Owner),
        (Method::PATCH, format!("/api/spaces/{alpha}"), Some(json!({ "name": "Alpha" })), None, Owner),
        (Method::GET, format!("/api/orgs/{org}/members"), None, None, OrgAdmin),
        (Method::GET, format!("/api/orgs/{org}/invites"), None, None, OrgAdmin),
        (Method::GET, format!("/api/orgs/{org}/audit"), None, None, OrgAdmin),
        (Method::PATCH, format!("/api/orgs/{org}"), Some(json!({ "name": "Default organization" })), None, OrgAdmin),
        (Method::GET, "/api/admin/resources".into(), None, None, InstanceAdmin),
        (Method::GET, "/api/admin/sessions".into(), None, None, InstanceAdmin),
        (Method::GET, "/api/admin/audit".into(), None, None, InstanceAdmin),
        (Method::POST, "/api/orgs".into(), Some(json!({ "name": "Second" })), None, InstanceAdmin),
    ];
    let principals: Vec<(&str, &Session, u8)> = vec![("viewer", &t.viewer, 1), ("editor", &t.editor, 2), ("owner", &t.owner, 3), ("outsider", &t.outsider, 0), ("admin", &t.admin, 9)];
    // Every route of the router is here, except `/ws/{id}` and deleting a workbook (their own
    // tests below) and `/api/me/password` (only for oneself).
    let shape = |p: &str| {
        p.split('?').next().unwrap().split('/').map(|seg| if [&wa, &alpha, &org, &admin_id, &other].contains(&&seg.to_string()) || seg == "none" || seg == "nothing" || seg == "nobody" { "{}" } else { seg }).collect::<Vec<_>>().join("/")
    };
    let covered: BTreeSet<String> = rows.iter().map(|(m, p, ..)| format!("{m} {}", shape(p))).collect();
    let api_routes = [
        "GET /api/me", "GET /api/workbooks", "POST /api/workbooks", "GET /api/workbooks/{}", "PATCH /api/workbooks/{}", "POST /api/workbooks/{}/duplicate",
        "GET /api/workbooks/{}/export.xlsx", "GET /api/trash", "DELETE /api/trash", "DELETE /api/trash/{}", "POST /api/trash/{}/restore", "GET /api/spaces",
        "POST /api/spaces", "PATCH /api/spaces/{}", "DELETE /api/spaces/{}", "GET /api/spaces/{}/members", "PUT /api/spaces/{}/members",
        "DELETE /api/spaces/{}/members/group/{}", "GET /api/spaces/{}/tokens", "POST /api/spaces/{}/tokens", "DELETE /api/spaces/{}/tokens/{}", "GET /api/orgs",
        "POST /api/orgs", "PATCH /api/orgs/{}", "GET /api/orgs/{}/members", "PATCH /api/orgs/{}/members/{}", "DELETE /api/orgs/{}/members/{}",
        "GET /api/orgs/{}/invites", "POST /api/orgs/{}/invites", "DELETE /api/orgs/{}/invites/{}", "GET /api/orgs/{}/audit", "PATCH /api/users/{}",
        "POST /api/workbooks/{}/import", "GET /api/workbooks/{}/export", "GET /api/workbooks/{}/downloads/{}", "POST /api/workbooks/{}/uploads",
        "DELETE /api/workbooks/{}/uploads/{}", "POST /api/workbooks/{}/uploads/{}/preview", "POST /api/workbooks/{}/uploads/{}/import",
        "GET /api/workbooks/{}/jobs/{}", "DELETE /api/workbooks/{}/jobs/{}", "GET /api/workbooks/{}/runtime", "POST /api/workbooks/{}/runtime",
        "GET /api/admin/resources", "GET /api/admin/sessions", "POST /api/admin/backups", "GET /api/admin/audit",
        "GET /api/orgs/{}/usage", "GET /api/admin/usage", "PUT /api/spaces/{}/quota", "GET /api/spaces/{}/library", "POST /api/spaces/{}/library",
        "GET /api/spaces/{}/library/{}", "PATCH /api/spaces/{}/library/{}", "GET /api/workbooks/{}/shareable", "POST /api/workbooks/{}/library",
    ];
    for r in api_routes {
        assert!(covered.contains(r), "route {r} is not in the matrix ({covered:?})");
    }
    let anonymous = t.server.anonymous();
    for (method, path, body, bytes, least) in &rows {
        let send = |s: &Session| {
            let (s, method, path, body, bytes) = (s.clone(), method.clone(), path.clone(), body.clone(), bytes.clone());
            async move {
                match bytes {
                    Some(b) => s.send_bytes(method, &path, b).await,
                    None => s.send(method, &path, body).await,
                }
            }
        };
        let (status, v) = send(&anonymous).await;
        assert_eq!(status, 401, "anonymous {method} {path}: {v}");
        for (who, s, rank) in &principals {
            let (status, v) = send(s).await;
            let space_scoped = matches!(least, Viewer | Editor | Owner | SpaceOrgAdmin);
            let needed = match least {
                Anyone => 0,
                Viewer => 1,
                Editor => 2,
                Owner => 3,
                OrgAdmin | SpaceOrgAdmin | InstanceAdmin => 9,
            };
            if *rank >= needed {
                assert!(!refused(status, &v), "{who} may {method} {path}, got {status} {v}");
            } else if space_scoped && *rank == 0 {
                assert_eq!(status, 404, "{who} cannot see {method} {path}: {v}");
            } else {
                assert_eq!(status, 403, "{who} may not {method} {path}: {v}");
            }
        }
    }
    t.server.stop().await;
}

/// Ids from a space one cannot see answer exactly like ids that do not exist (7.T01).
#[tokio::test(flavor = "multi_thread")]
async fn ids_from_another_space_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let (wb, beta) = (t.wb.clone(), t.beta.clone());
    for (path, missing) in [
        (format!("/api/workbooks/{wb}"), "/api/workbooks/000000000000".to_string()),
        (format!("/api/workbooks/{wb}/export.xlsx"), "/api/workbooks/000000000000/export.xlsx".into()),
        (format!("/api/spaces/{beta}/members"), "/api/spaces/0000000000000000/members".into()),
    ] {
        let (s1, v1) = t.owner.get(&path).await;
        let (s2, v2) = t.owner.get(&missing).await;
        assert_eq!((s1, s2), (404, 404), "{v1} {v2}");
        let strip = |v: &Value| v["error"].as_str().unwrap().replace(&wb, "ID").replace("000000000000", "ID");
        assert_eq!(strip(&v1), strip(&v2), "the same message for {path}");
    }
    for (method, path, body) in [
        (Method::PATCH, format!("/api/workbooks/{wb}"), Some(json!({ "name": "mine" }))),
        (Method::DELETE, format!("/api/workbooks/{wb}"), None),
        (Method::POST, format!("/api/workbooks/{wb}/duplicate"), None),
        (Method::POST, format!("/api/trash/{wb}/restore"), None),
        (Method::DELETE, format!("/api/trash/{wb}"), None),
        (Method::POST, "/api/workbooks".into(), Some(json!({ "name": "x", "space": beta }))),
        (Method::DELETE, format!("/api/spaces/{beta}"), None),
        (Method::GET, format!("/api/spaces/{beta}/tokens"), None),
    ] {
        let (s, v) = t.owner.send(method.clone(), &path, body).await;
        assert_eq!(s, 404, "{method} {path}: {v}");
    }
    // Moving one's workbook into a space one cannot see.
    let (s, v) = t.owner.send(Method::PATCH, &format!("/api/workbooks/{}", t.wa), Some(json!({ "space": beta }))).await;
    assert_eq!(s, 404, "{v}");
    // Opening a WebSocket.
    let e = t.owner.ws(&wb).await.err().unwrap();
    assert_eq!(e, "HTTP 404");
    // A download made in Beta, fetched through a workbook of Alpha.
    let mut admin_ws = t.admin.ws(&wb).await.unwrap();
    let export = admin_ws.ok("export_query", json!({ "sql": "SELECT * FROM t" })).await;
    let download = export["download"].as_str().unwrap();
    let (s, _) = t.owner.get(&format!("/api/workbooks/{}/downloads/{download}", t.wa)).await;
    assert_eq!(s, 404);
    let (s, _) = t.owner.get(&format!("/api/workbooks/{wb}/downloads/{download}")).await;
    assert_eq!(s, 404);
    let (s, _) = t.admin.get(&format!("/api/workbooks/{wb}/downloads/{download}")).await;
    assert_eq!(s, 200, "the admin, who can see Beta, still gets it");
    // Listings show only what one can see.
    let (_, list) = t.owner.get("/api/workbooks").await;
    let ids: BTreeSet<&str> = list.as_array().unwrap().iter().map(|w| w["id"].as_str().unwrap()).collect();
    assert!(ids.contains(t.wa.as_str()) && !ids.contains(wb.as_str()), "{list}");
    assert!(list.as_array().unwrap().iter().all(|w| w["role"] == "owner"), "{list}");
    let (_, spaces) = t.viewer.get("/api/spaces").await;
    assert_eq!(spaces.as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap(), s["role"].as_str().unwrap())).collect::<Vec<_>>(), vec![("Alpha", "viewer")]);
    let (_, none) = t.outsider.get("/api/spaces").await;
    assert_eq!(none, json!([]));
    let (_, none) = t.outsider.get("/api/workbooks").await;
    assert_eq!(none, json!([]));
    admin_ws.close().await;
    t.server.stop().await;
}

/// Operations a viewer may send (independent of `ws::required_caps`): reading, querying and
/// exporting. Everything else needs an editor.
const VIEWER_OPS: &[&str] = &[
    "tables", "open_view", "update_view", "view_info", "fetch", "stats", "find_in_view", "column_values", "sheet_find", "sheet_find_all", "comments", "hidden_sheets", "sheet_rules", "sheet_charts", "sheet_layout", "sheet_column_texts", "sheet_range_values", "query", "sheets", "sheet_fetch", "sheet_names", "sheet_cell_detail",
    "sheet_check_formula", "close_view", "history", "schema", "saved_views", "query_versions", "saved_queries", "export_query", "materialized",
    "analyze", "analyses", "dashboards", "dashboard", "validations", "preview_transform", "inspect_formula", "xlsx_compatibility", "drill",
    "presentation", "replace_preview", "functions", "test_function", "cancel",
];

/// Every WebSocket operation answers a viewer and an editor as the role table says (7.T01).
#[tokio::test(flavor = "multi_thread")]
async fn every_websocket_operation_follows_the_role_table() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let mut viewer = t.viewer.ws(&t.wa).await.unwrap();
    assert_eq!(viewer.hello["role"], "viewer");
    let mut editor = t.editor.ws(&t.wa).await.unwrap();
    assert_eq!(editor.hello["role"], "editor");
    let script = common::operation_script();
    let all: BTreeSet<&str> = alephgrid_server::protocol::RESULTS.iter().map(|(op, _)| *op).collect();
    assert_eq!(script.iter().map(|(op, _)| *op).collect::<BTreeSet<_>>(), all, "the script covers every operation");
    let revision = |ws: &Ws| ws.hello["revision"].as_u64().unwrap();
    let before = revision(&viewer);
    // Ids of objects the viewer never made: any well-formed value will do.
    let fill = |mut p: Value| {
        for (k, placeholder, value) in [("saved", "PLACEHOLDER", json!("none")), ("saved", "QUERY", json!("none")), ("analysis", "ANALYSIS", json!("none")),
            ("dashboard", "DASHBOARD", json!("none")), ("revision", "PREVIEW_REV", json!(1)), ("revision", "UNDO_REV", json!(1))] {
            if p[k] == placeholder {
                p[k] = value;
            }
        }
        if p["tiles"][0]["analysis"] == "ANALYSIS" {
            p["tiles"][0]["analysis"] = json!("none");
        }
        p
    };
    for (op, params) in &script {
        let code = viewer.code(op, fill(params.clone())).await;
        if VIEWER_OPS.contains(op) {
            assert_ne!(code.as_deref(), Some("forbidden"), "a viewer may {op}");
        } else {
            assert_eq!(code.as_deref(), Some("forbidden"), "a viewer may not {op}");
        }
    }
    // The viewer changed nothing.
    let again = t.viewer.ws(&t.wa).await.unwrap();
    assert_eq!(revision(&again), before, "refused operations changed nothing");
    again.close().await;
    for (op, params) in &script {
        let code = editor.code(op, fill(params.clone())).await;
        assert_ne!(code.as_deref(), Some("forbidden"), "an editor may {op}");
    }
    viewer.close().await;
    editor.close().await;
    t.server.stop().await;
}

/// Open connections follow role changes, removal, sign-out, disabling and moves at once.
#[tokio::test(flavor = "multi_thread")]
async fn open_connections_lose_access_when_it_is_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let alpha = t.alpha.clone();
    let mut editor = t.editor.ws(&t.wa).await.unwrap();
    let view = editor.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    editor.ok("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "s", "value": "x" }] })).await;
    // Demoted to viewer: told so, and the next change is refused.
    t.owner.ok(Method::PUT, &format!("/api/spaces/{alpha}/members"), Some(json!({ "kind": "user", "principal": "editor@example.com", "role": "viewer" }))).await;
    let ev = editor.event("role_changed").await.expect("role_changed");
    assert_eq!(ev["role"], "viewer");
    assert_eq!(editor.code("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "s", "value": "y" }] })).await.as_deref(), Some("forbidden"));
    editor.ok("fetch", json!({ "view_id": view, "row_start": 0, "row_count": 3 })).await;
    // Removed from the space: the connection ends.
    let editor_id = t.editor.user_id();
    t.owner.ok(Method::DELETE, &format!("/api/spaces/{alpha}/members/user/{editor_id}"), None).await;
    let ev = editor.event("access_revoked").await.expect("access_revoked");
    assert_eq!(ev["reason"], "no_access");
    assert!(editor.closed().await, "the server closed the connection");
    assert_eq!(t.editor.ws(&t.wa).await.err().unwrap(), "HTTP 404");

    // Signing out ends the session's connections.
    let mut viewer = t.viewer.ws(&t.wa).await.unwrap();
    let (s, _) = t.viewer.post("/api/auth/logout", json!({})).await;
    assert_eq!(s, 204);
    let ev = viewer.event("access_revoked").await.expect("access_revoked");
    assert_eq!(ev["reason"], "signed_out");
    assert_eq!(t.viewer.get("/api/me").await.0, 401);

    // Disabling a user ends theirs too, and they cannot sign in again.
    let mut owner = t.owner.ws(&t.wa).await.unwrap();
    t.admin.ok(Method::PATCH, &format!("/api/users/{}", t.owner.user_id()), Some(json!({ "disabled": true }))).await;
    assert_eq!(owner.event("access_revoked").await.unwrap()["reason"], "signed_out");
    assert_eq!(t.owner.get("/api/workbooks").await.0, 401);
    assert!(t.server.login("owner@example.com", PASSWORD).await.is_err());

    // Moving a workbook into a space someone cannot see ends their connection.
    let code = t.admin.invite(&t.org, "watcher@example.com", Some(&alpha), Some("viewer")).await;
    let watcher = t.server.accept(&code, "watcher", PASSWORD).await;
    let mut w = watcher.ws(&t.wa).await.unwrap();
    t.admin.ok(Method::PATCH, &format!("/api/workbooks/{}", t.wa), Some(json!({ "space": t.beta }))).await;
    assert_eq!(w.event("access_revoked").await.unwrap()["reason"], "no_access");
    t.server.stop().await;
}

/// Service tokens: one space, their role, header only, revocable, expiring (7.02).
#[tokio::test(flavor = "multi_thread")]
async fn service_tokens_are_scoped_to_their_space_and_role() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let alpha = t.alpha.clone();
    let (s, v) = t.owner.post(&format!("/api/spaces/{alpha}/tokens"), json!({ "name": "nightly", "role": "owner", "days": 30 })).await;
    assert_eq!(s, 400, "no owner tokens: {v}");
    let (s, _) = t.owner.post(&format!("/api/spaces/{alpha}/tokens"), json!({ "name": "nightly", "role": "viewer", "days": 400 })).await;
    assert_eq!(s, 400, "at most 366 days");
    let made = t.owner.ok(Method::POST, &format!("/api/spaces/{alpha}/tokens"), Some(json!({ "name": "nightly", "role": "viewer", "days": 30 }))).await;
    let secret = made["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("agt_"));
    let token = t.server.bearer(&secret);
    assert_eq!(token.get(&format!("/api/workbooks/{}", t.wa)).await.0, 200);
    assert_eq!(token.get(&format!("/api/workbooks/{}", t.wb)).await.0, 404, "another space");
    assert_eq!(token.post("/api/spaces", json!({ "name": "Mine" })).await.0, 403);
    let (_, spaces) = token.get("/api/spaces").await;
    assert_eq!(spaces.as_array().unwrap().len(), 1);
    let mut ws = token.ws(&t.wa).await.unwrap();
    assert_eq!(ws.hello["role"], "viewer");
    ws.ok("tables", json!({})).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    assert_eq!(ws.code("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "s", "value": "q" }] })).await.as_deref(), Some("forbidden"));
    // Never from a URL.
    let (s, _) = t.server.anonymous().get(&format!("/api/workbooks/{}?token={secret}", t.wa)).await;
    assert_eq!(s, 401);
    // Its secret is shown once: listings carry neither it nor its hash.
    let (_, list) = t.owner.get(&format!("/api/spaces/{alpha}/tokens")).await;
    let text = list.to_string();
    let raw = secret.rsplit('_').next().unwrap();
    assert!(!text.contains(raw) && !text.contains("hash"), "{text}");
    // A wrong secret for a real id.
    let forged = format!("{}_{}", secret.rsplit_once('_').unwrap().0, "x".repeat(43));
    assert_eq!(t.server.bearer(&forged).get("/api/spaces").await.0, 401);
    // Revoked: refused at once, and its connection ends.
    let tid = made["token"]["id"].as_str().unwrap();
    t.owner.ok(Method::DELETE, &format!("/api/spaces/{alpha}/tokens/{tid}"), None).await;
    assert_eq!(ws.event("access_revoked").await.unwrap()["reason"], "signed_out");
    assert_eq!(token.get(&format!("/api/workbooks/{}", t.wa)).await.0, 401);
    // Expired.
    let store = &t.server.state.auth.store;
    let h = format!("{:x}", Sha256::digest(b"expired-secret"));
    store.create_token("deadbeef00000000", &h, "old", &alpha, alephgrid_server::auth::SpaceRole::Viewer, "test", alephgrid_server::auth::now_ms() - 1).unwrap();
    assert_eq!(t.server.bearer("agt_deadbeef00000000_expired-secret").get("/api/spaces").await.0, 401);
    t.server.stop().await;
}

/// Proxy identity headers, other sites' pages and other host names are refused (7.06, 7.17).
#[tokio::test(flavor = "multi_thread")]
async fn browser_and_proxy_protections() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let url = |p: &str| t.server.url(p);
    let cookie = format!("alephgrid_session={}", t.editor.cookie.as_deref().unwrap());
    let http = reqwest::Client::new();
    for h in ["X-Forwarded-User", "X-Remote-User", "Remote-User", "X-Auth-Request-Email", "X-Forwarded-Email"] {
        let r = http.get(url("/api/me")).header("Cookie", &cookie).header(h, "admin@example.com").send().await.unwrap();
        assert_eq!(r.status().as_u16(), 400, "{h}");
        let r = http.get(url("/api/config")).header(h, "admin@example.com").send().await.unwrap();
        assert_eq!(r.status().as_u16(), 400, "{h} on a public route");
    }
    // A change from another site's page, and one with a cookie that says nothing of where it comes from.
    let body = json!({ "name": "x", "space": t.alpha });
    let r = http.post(url("/api/workbooks")).header("Cookie", &cookie).header("Origin", "http://evil.example").json(&body).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let r = http.post(url("/api/workbooks")).header("Cookie", &cookie).json(&body).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let r = http.post(url("/api/workbooks")).header("Cookie", &cookie).header("Sec-Fetch-Site", "same-origin").json(&body).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "a same-origin fetch without Origin");
    // A WebSocket opened from another site's page.
    let e = Ws::try_connect_with(t.server.addr, &t.wa, &[("Cookie".into(), cookie.clone()), ("Origin".into(), "http://evil.example".into())]).await.err().unwrap();
    assert_eq!(e, "HTTP 403");
    // Another host name (DNS rebinding to a loopback server).
    let r = http.get(url("/api/config")).header("Host", "evil.example").send().await.unwrap();
    assert_eq!(r.status().as_u16(), 421);
    // The session cookie's attributes.
    let r = http.post(url("/api/auth/login")).header("Origin", t.server.origin()).json(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD })).send().await.unwrap();
    let set = r.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax") && set.contains("Path=/"), "{set}");
    t.server.stop().await;
}

/// A session ends at its expiry, and its connections with it.
#[tokio::test(flavor = "multi_thread")]
async fn sessions_expire() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_accounts(dir.path(), |c| c.auth.session_ttl = Duration::from_secs(3)).await;
    let admin = server.admin().await;
    let wb = admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": "w" }))).await["id"].as_str().unwrap().to_string();
    let mut ws = admin.ws(&wb).await.unwrap();
    assert_eq!(admin.get("/api/me").await.0, 200);
    let ev = tokio::time::timeout(Duration::from_secs(10), ws.event("access_revoked")).await.expect("revoked within 10 s");
    assert_eq!(ev.unwrap()["reason"], "signed_out");
    assert_eq!(admin.get("/api/me").await.0, 401);
    server.stop().await;
}

/// First start without users: the one-time setup code creates the instance admin (7.02).
#[tokio::test(flavor = "multi_thread")]
async fn setup_code_creates_the_instance_admin_once() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.auth.mode = alephgrid_server::auth::AuthMode::Accounts).await;
    let (_, config) = server.get("/api/config").await;
    assert_eq!(config["auth"], "accounts");
    assert_eq!(config["setup_required"], true);
    let anon = server.anonymous();
    assert_eq!(anon.get("/api/workbooks").await.0, 401);
    assert_eq!(anon.get("/ws/x").await.0, 401);
    let code = server.state.auth.setup_code().unwrap();
    let body = |code: &str, password: &str| json!({ "code": code, "email": "Root@Example.com", "name": "Root", "password": password });
    assert_eq!(anon.post("/api/auth/setup", body("wrong-code", PASSWORD)).await.0, 403);
    assert_eq!(anon.post("/api/auth/setup", body(&code, "short")).await.0, 400);
    let (s, me) = anon.post("/api/auth/setup", body(&code, PASSWORD)).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["user"]["email"], "root@example.com");
    assert_eq!(me["instance_admin"], true);
    assert_eq!(me["orgs"][0]["role"], "admin");
    assert_eq!(anon.post("/api/auth/setup", body(&code, PASSWORD)).await.0, 400, "once");
    assert_eq!(server.get("/api/config").await.1["setup_required"], false);
    let root = server.login("root@example.com", PASSWORD).await.unwrap();
    assert_eq!(root.get("/api/admin/resources").await.0, 200);
    server.stop().await;
}

/// Wrong passwords: one answer for every mistake, then a lockout for that email (7.02).
#[tokio::test(flavor = "multi_thread")]
async fn password_sign_in_refuses_guessing() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_accounts(dir.path(), |_| {}).await;
    let (s1, wrong_password) = server.login(ADMIN_EMAIL, "not the password").await.err().unwrap();
    let (s2, wrong_email) = server.login("nobody@example.com", "not the password").await.err().unwrap();
    assert_eq!((s1, s2), (403, 403));
    assert_eq!(wrong_password["error"], wrong_email["error"], "nothing tells whether the email exists");
    for _ in 0..4 {
        assert!(server.login(ADMIN_EMAIL, "not the password").await.is_err());
    }
    let (_, locked) = server.login(ADMIN_EMAIL, ADMIN_PASSWORD).await.err().expect("locked out after 5 failures");
    assert!(locked["error"].as_str().unwrap().contains("too many failed sign-ins"), "{locked}");
    let audit = server.state.auth.store.audit_events(None, None, None, 100).unwrap();
    assert!(audit.iter().filter(|e| e.action == "sign_in_failed").count() >= 5);
    server.stop().await;
}

/// Changing one's password needs the current one and ends one's other sessions.
#[tokio::test(flavor = "multi_thread")]
async fn password_change_needs_the_current_password() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_accounts(dir.path(), |_| {}).await;
    let here = server.admin().await;
    let elsewhere = server.admin().await;
    let new = "another long password";
    let (s, v) = here.post("/api/me/password", json!({ "current": "wrong", "new": new })).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(here.post("/api/me/password", json!({ "current": ADMIN_PASSWORD, "new": "short" })).await.0, 400);
    assert_eq!(here.post("/api/me/password", json!({ "current": ADMIN_PASSWORD, "new": new })).await.0, 204);
    assert_eq!(here.get("/api/me").await.0, 200, "this session goes on");
    assert_eq!(elsewhere.get("/api/me").await.0, 401, "the other one ends");
    assert!(server.login(ADMIN_EMAIL, ADMIN_PASSWORD).await.is_err());
    assert!(server.login(ADMIN_EMAIL, new).await.is_ok());
    let token = server.bearer("agt_x_y");
    assert_eq!(token.post("/api/me/password", json!({ "current": "a", "new": new })).await.0, 401);
    server.stop().await;
}

/// Invitations: one use, the named email, expiry; an existing user signs in first.
#[tokio::test(flavor = "multi_thread")]
async fn invitations_are_single_use_and_bound_to_their_email() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let code = t.admin.invite(&t.org, "new@example.com", Some(&t.alpha), Some("editor")).await;
    let anon = t.server.anonymous();
    let (s, info) = anon.post("/api/invites/lookup", json!({ "code": code })).await;
    assert_eq!(s, 200);
    assert_eq!(info["email"], "new@example.com");
    assert_eq!(info["space"], "Alpha");
    assert_eq!(info["space_role"], "editor");
    // The secret part matters.
    let forged = format!("{}.{}", code.split('.').next().unwrap(), "x".repeat(43));
    assert_eq!(anon.post("/api/invites/lookup", json!({ "code": forged })).await.0, 404);
    // Signed in as someone else.
    let (s, v) = t.viewer.post("/api/invites/accept", json!({ "code": code })).await;
    assert_eq!(s, 403, "{v}");
    let new = t.server.accept(&code, "New", PASSWORD).await;
    assert_eq!(new.get(&format!("/api/workbooks/{}", t.wa)).await.1["role"], "editor");
    assert_eq!(anon.post("/api/invites/accept", json!({ "code": code, "password": PASSWORD })).await.0, 404, "used once");
    // An invitation for an existing user is accepted by signing in as them.
    let code = t.admin.invite(&t.org, "viewer@example.com", Some(&t.beta), Some("viewer")).await;
    let (s, v) = anon.post("/api/invites/accept", json!({ "code": code, "password": PASSWORD })).await;
    assert_eq!(s, 400, "{v}");
    let viewer = t.server.login("viewer@example.com", PASSWORD).await.unwrap();
    let (s, v) = viewer.post("/api/invites/accept", json!({ "code": code })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(viewer.get(&format!("/api/workbooks/{}", t.wb)).await.0, 200);
    // Revoked.
    let made = t.admin.ok(Method::POST, &format!("/api/orgs/{}/invites", t.org), Some(json!({ "email": "late@example.com" }))).await;
    t.admin.ok(Method::DELETE, &format!("/api/orgs/{}/invites/{}", t.org, made["invite"]["id"].as_str().unwrap()), None).await;
    assert_eq!(anon.post("/api/invites/lookup", json!({ "code": made["code"] })).await.0, 404);
    // Only organization admins invite.
    assert_eq!(t.owner.post(&format!("/api/orgs/{}/invites", t.org), json!({ "email": "x@example.com" })).await.0, 403);
    t.server.stop().await;
}

/// Organization and space rules: members only, last admin kept, spaces deleted only when empty.
#[tokio::test(flavor = "multi_thread")]
async fn membership_rules() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    // A role goes only to members of the organization.
    let second = t.admin.ok(Method::POST, "/api/orgs", Some(json!({ "name": "Second" }))).await["id"].as_str().unwrap().to_string();
    let code = t.admin.invite(&second, "stranger@example.com", None, None).await;
    t.server.accept(&code, "Stranger", PASSWORD).await;
    let (s, v) = t.owner.send(Method::PUT, &format!("/api/spaces/{}/members", t.alpha), Some(json!({ "kind": "user", "principal": "stranger@example.com", "role": "viewer" }))).await;
    assert_eq!(s, 400, "{v}");
    // The last admin stays.
    let admin_id = t.admin.user_id();
    let (s, v) = t.admin.send(Method::PATCH, &format!("/api/orgs/{}/members/{admin_id}", t.org), Some(json!({ "role": "member" }))).await;
    assert_eq!(s, 400, "{v}");
    // Making someone else admin makes them owner of every space.
    t.admin.ok(Method::PATCH, &format!("/api/orgs/{}/members/{}", t.org, t.outsider.user_id()), Some(json!({ "role": "admin" }))).await;
    assert_eq!(t.outsider.get(&format!("/api/workbooks/{}", t.wb)).await.1["role"], "owner");
    // Removing from the organization removes space roles.
    t.admin.ok(Method::DELETE, &format!("/api/orgs/{}/members/{}", t.org, t.editor.user_id()), None).await;
    assert_eq!(t.editor.get(&format!("/api/workbooks/{}", t.wa)).await.0, 404);
    let (_, members) = t.admin.get(&format!("/api/spaces/{}/members", t.alpha)).await;
    assert!(!members.to_string().contains(&t.editor.user_id()), "{members}");
    // Deleting a workbook needs an editor.
    assert_eq!(t.viewer.send(Method::DELETE, &format!("/api/workbooks/{}", t.wa), None).await.0, 403);
    // Spaces: refused while they hold workbooks, the trash included.
    let (s, v) = t.owner.send(Method::DELETE, &format!("/api/spaces/{}", t.alpha), None).await;
    assert_eq!(s, 400, "{v}");
    t.owner.ok(Method::DELETE, &format!("/api/workbooks/{}", t.wa), None).await;
    assert_eq!(t.owner.send(Method::DELETE, &format!("/api/spaces/{}", t.alpha), None).await.0, 400, "trash still holds it");
    t.owner.ok(Method::DELETE, &format!("/api/trash/{}", t.wa), None).await;
    t.owner.ok(Method::DELETE, &format!("/api/spaces/{}", t.alpha), None).await;
    assert_eq!(t.viewer.get("/api/spaces").await.1, json!([]));
    // Editors restore but do not purge.
    let w = t.admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": "w", "space": t.beta }))).await["id"].as_str().unwrap().to_string();
    t.admin.ok(Method::PUT, &format!("/api/spaces/{}/members", t.beta), Some(json!({ "kind": "user", "principal": "viewer@example.com", "role": "editor" }))).await;
    t.viewer.ok(Method::DELETE, &format!("/api/workbooks/{w}"), None).await;
    let (_, trash) = t.viewer.get("/api/trash").await;
    assert_eq!(trash.as_array().unwrap().len(), 1);
    assert_eq!(t.viewer.send(Method::DELETE, &format!("/api/trash/{w}"), None).await.0, 403);
    t.viewer.ok(Method::POST, &format!("/api/trash/{w}/restore"), None).await;
    t.server.stop().await;
}

/// Nothing a client can read contains password hashes, secrets or identity-provider subjects;
/// the audit log records who did what (7.06, 7.T01).
#[tokio::test(flavor = "multi_thread")]
async fn secrets_stay_out_of_replies_and_the_audit_log_attributes_actions() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let made = t.owner.ok(Method::POST, &format!("/api/spaces/{}/tokens", t.alpha), Some(json!({ "name": "ci", "role": "editor", "days": 1 }))).await;
    let secret = made["secret"].as_str().unwrap().to_string();
    let session = t.owner.cookie.clone().unwrap();
    let mut texts = Vec::new();
    for path in [
        "/api/me".to_string(),
        format!("/api/orgs/{}/members", t.org),
        format!("/api/orgs/{}/invites", t.org),
        format!("/api/orgs/{}/audit", t.org),
        "/api/admin/audit".into(),
        "/api/admin/sessions".into(),
        format!("/api/spaces/{}/members", t.alpha),
        format!("/api/spaces/{}/tokens", t.alpha),
        "/api/workbooks".into(),
    ] {
        let (s, v) = t.admin.get(&path).await;
        assert_eq!(s, 200, "{path}");
        texts.push(v.to_string());
    }
    for text in &texts {
        for secret_text in ["$argon2", "password_hash", PASSWORD, ADMIN_PASSWORD, secret.as_str(), session.as_str(), "oidc_subject"] {
            assert!(!text.contains(secret_text), "{secret_text:?} leaked: {text}");
        }
    }
    let events = t.server.state.auth.store.audit_events(Some(&t.org), None, None, 1000).unwrap();
    // Oldest first: the first time each happened.
    let find = |action: &str| events.iter().rev().find(|e| e.action == action).unwrap_or_else(|| panic!("no {action} in {events:?}"));
    assert_eq!(find("token_created").actor, format!("user:{}", t.owner.user_id()));
    assert_eq!(find("token_created").space_id.as_deref(), Some(t.alpha.as_str()));
    assert_eq!(find("invitation_accepted").org_id.as_deref(), Some(t.org.as_str()));
    assert_eq!(find("workbook_created").workbook.as_deref(), Some(t.wa.as_str()));
    assert_eq!(find("space_created").actor, format!("user:{}", t.admin.user_id()));
    // The data directory never appears in errors.
    let (_, v) = t.editor.send_bytes(Method::POST, &format!("/api/workbooks/{}/import?filename=bad.parquet", t.wa), b"not parquet".to_vec()).await;
    let dir_text = dir.path().to_string_lossy().to_string();
    assert!(!v.to_string().contains(&dir_text), "{v}");
    t.server.stop().await;
}

/// 7.01: workspaces of an older version become spaces; workbooks, live and trashed, keep their
/// grouping; the migration runs once and survives being repeated.
#[tokio::test(flavor = "multi_thread")]
async fn workspaces_migrate_to_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let a = server.create_workbook("in finance").await;
    let b = server.create_workbook("unfiled").await;
    let c = server.create_workbook("trashed in sales").await;
    assert_eq!(server.send(Method::DELETE, &format!("/api/workbooks/{c}"), None).await.0, 204);
    server.stop().await;
    // Make the directory look like a pre-spaces one.
    let control = dir.path().join(".control");
    std::fs::remove_file(control.join("control.duckdb")).unwrap();
    let _ = std::fs::remove_file(control.join("control.duckdb.wal"));
    std::fs::write(dir.path().join("workspaces.json"), json!([
        { "id": "ws0finance", "name": "Finance", "created_at": 1 },
        { "id": "ws0sales", "name": "Sales", "created_at": 2 },
        { "id": "ws0general", "name": "General", "created_at": 3 },
    ]).to_string()).unwrap();
    for (dir, ws) in [(dir.path().join(&a), Some("ws0finance")), (dir.path().join(&b), None), (dir.path().join(".trash").join(&c), Some("ws0sales"))] {
        let p = dir.join("workbook.json");
        let mut meta: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        meta.as_object_mut().unwrap().remove("space");
        meta["workspace"] = json!(ws);
        std::fs::write(&p, meta.to_string()).unwrap();
    }
    let server = TestServer::start(dir.path()).await;
    let (_, spaces) = server.get("/api/spaces").await;
    let by_name: HashMap<String, String> = spaces.as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap().to_string(), s["id"].as_str().unwrap().to_string())).collect();
    assert_eq!(by_name.get("Finance").map(String::as_str), Some("ws0finance"), "{spaces}");
    assert_eq!(by_name.get("Sales").map(String::as_str), Some("ws0sales"));
    assert_eq!(by_name.get("General").map(String::as_str), Some("ws0general"), "a workspace named General keeps its name");
    let general = &by_name["General (2)"];
    let (_, list) = server.get("/api/workbooks").await;
    let space_of: HashMap<&str, &str> = list.as_array().unwrap().iter().map(|w| (w["id"].as_str().unwrap(), w["space"].as_str().unwrap())).collect();
    assert_eq!(space_of[a.as_str()], "ws0finance");
    assert_eq!(space_of[b.as_str()], general.as_str(), "unfiled workbooks join the default space");
    let (_, trash) = server.get("/api/trash").await;
    assert_eq!(trash[0]["space"], "ws0sales");
    assert!(!dir.path().join("workspaces.json").exists());
    assert!(control.join("workspaces.json.migrated").exists());
    let meta: Value = serde_json::from_slice(&std::fs::read(dir.path().join(&a).join("workbook.json")).unwrap()).unwrap();
    assert!(meta.get("workspace").is_none(), "{meta}");
    server.stop().await;
    // Again: nothing changes.
    let server = TestServer::start(dir.path()).await;
    let (_, again) = server.get("/api/spaces").await;
    assert_eq!(again.as_array().unwrap().len(), spaces.as_array().unwrap().len());
    // A restored workbook goes back to its space.
    assert_eq!(server.send(Method::POST, &format!("/api/trash/{c}/restore"), None).await.1["space"], "ws0sales");
    server.stop().await;
}

/// A backup carries the control database: restored users sign in and keep their roles.
#[tokio::test(flavor = "multi_thread")]
async fn backups_restore_identities_and_roles() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let (s, v) = t.admin.post("/api/admin/backups", json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["manifest"]["control_sha256"].is_string());
    let backup = std::path::PathBuf::from(v["path"].as_str().unwrap());
    let (wa, alpha) = (t.wa.clone(), t.alpha.clone());
    t.server.stop().await;
    let restored = tempfile::tempdir().unwrap();
    let target = restored.path().join("data");
    alephgrid_server::backup::restore(&backup, &target).unwrap();
    let server = TestServer::start_accounts(&target, |_| {}).await;
    let editor = server.login("editor@example.com", PASSWORD).await.expect("restored user signs in");
    let (s, v) = editor.get(&format!("/api/workbooks/{wa}")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["role"], "editor");
    assert_eq!(v["workbook"]["space"], alpha.as_str());
    server.stop().await;
    // A damaged control database is refused.
    std::fs::write(backup.join("control.duckdb"), b"damaged").unwrap();
    let again = restored.path().join("again");
    let e = alephgrid_server::backup::restore(&backup, &again).unwrap_err();
    assert!(format!("{e:#}").contains("control database is damaged"), "{e:#}");
}

// ------------------------------------------------------------- single sign-on

/// A minimal OpenID provider: discovery, keys, and a token endpoint that checks PKCE and the
/// client's credentials and returns the ID token the test prepared for a code.
struct Idp {
    issuer: String,
    codes: Arc<Mutex<HashMap<String, Grant>>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct Grant {
    claims: Value,
    challenge: String,
    /// Sign with the provider's key (`idp`), a stranger's (`other`) or not at all (`none`).
    signer: &'static str,
}

const CLIENT_ID: &str = "alephgrid-test";
const CLIENT_SECRET: &str = "client secret";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oidc").join(name)).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sign(claims: &Value, signer: &str) -> String {
    match signer {
        "none" => format!("{}.{}.", b64(br#"{"alg":"none","typ":"JWT"}"#), b64(claims.to_string().as_bytes())),
        s => {
            let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
            header.kid = Some("k1".into());
            let key = jsonwebtoken::EncodingKey::from_rsa_pem(fixture(if s == "other" { "other.pem" } else { "idp.pem" }).as_bytes()).unwrap();
            jsonwebtoken::encode(&header, claims, &key).unwrap()
        }
    }
}

impl Idp {
    async fn start() -> Idp {
        use axum::routing::{get, post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let codes: Arc<Mutex<HashMap<String, Grant>>> = Arc::default();
        let disc = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks"),
            "token_endpoint_auth_methods_supported": ["client_secret_basic"],
        });
        let jwks = json!({ "keys": [{ "kty": "RSA", "kid": "k1", "use": "sig", "alg": "RS256", "n": fixture("idp.n").trim(), "e": "AQAB" }] });
        let c = codes.clone();
        let app = axum::Router::new()
            .route("/.well-known/openid-configuration", get(move || async move { axum::Json(disc) }))
            .route("/jwks", get(move || async move { axum::Json(jwks) }))
            .route(
                "/token",
                post(move |headers: axum::http::HeaderMap, axum::Form(form): axum::Form<HashMap<String, String>>| {
                    let c = c.clone();
                    async move {
                        let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{}", CLIENT_SECRET.replace(' ', "%20"))));
                        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(basic.as_str()) {
                            return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "invalid_client" })));
                        }
                        let grant = c.lock().unwrap().remove(form.get("code").map(String::as_str).unwrap_or(""));
                        let Some(g) = grant else { return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_grant" }))) };
                        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
                        if b64(&Sha256::digest(verifier.as_bytes())) != g.challenge || form.get("grant_type").map(String::as_str) != Some("authorization_code") {
                            return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_grant" })));
                        }
                        (axum::http::StatusCode::OK, axum::Json(json!({ "access_token": "x", "token_type": "Bearer", "id_token": sign(&g.claims, g.signer) })))
                    }
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Idp { issuer, codes, task }
    }
}

/// Starts a sign-in at the server, plays the identity provider's part with `claims` (the
/// test's nonce is filled in unless `claims` sets one), and returns the callback's reply.
async fn sso(server: &TestServer, idp: &Idp, claims: Value, signer: &'static str) -> (u16, String, Option<String>) {
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let r = http.get(server.url("/api/auth/oidc/start?next=/w/abc")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 303);
    let location = reqwest::Url::parse(r.headers()["location"].to_str().unwrap()).unwrap();
    assert!(location.as_str().starts_with(&format!("{}/authorize", idp.issuer)));
    let q: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["client_id"], CLIENT_ID);
    let mut claims = claims;
    if claims.get("nonce").is_none() {
        claims["nonce"] = json!(q["nonce"]);
    }
    let code = alephgrid_server::auth::random_id();
    idp.codes.lock().unwrap().insert(code.clone(), Grant { claims, challenge: q["code_challenge"].clone(), signer });
    let r = http.get(server.url(&format!("/api/auth/oidc/callback?code={code}&state={}", common::urlencode(&q["state"])))).send().await.unwrap();
    let status = r.status().as_u16();
    let cookie = session_cookie(r.headers());
    let to = r.headers().get("location").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    (status, to, cookie)
}

fn claims(idp: &Idp, sub: &str, email: &str, groups: &[&str]) -> Value {
    let now = alephgrid_server::auth::now_ms() / 1000;
    json!({ "iss": idp.issuer, "aud": CLIENT_ID, "sub": sub, "email": email, "email_verified": true, "name": "Sam", "groups": groups, "iat": now, "exp": now + 300 })
}

/// 7.02, 7.17: OIDC sign-in, invitations, group roles, refusal of bad tokens and of
/// unconfirmed or colliding emails.
#[tokio::test(flavor = "multi_thread")]
async fn single_sign_on_maps_claims_to_users_and_roles() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let issuer = idp.issuer.clone();
    let t = team_with(dir.path(), move |c| {
        c.auth.oidc = Some(alephgrid_server::auth::oidc::OidcConfig {
            issuer,
            client_id: CLIENT_ID.into(),
            client_secret: Some(CLIENT_SECRET.into()),
            scopes: "openid email profile".into(),
            groups_claim: "groups".into(),
            label: "Sign in with Test".into(),
            join: alephgrid_server::auth::oidc::OidcJoin::Invite,
            redirect_uri: "http://127.0.0.1/api/auth/oidc/callback".into(),
        });
    })
    .await;
    let s = &t.server;
    assert_eq!(s.get("/api/config").await.1["sso"]["label"], "Sign in with Test");
    // Not invited.
    let (status, to, cookie) = sso(s, &idp, claims(&idp, "sub-1", "sam@example.com", &[]), "idp").await;
    assert_eq!(status, 303);
    assert!(to.starts_with("/login?error=") && to.contains("invited"), "{to}");
    assert!(cookie.is_none());
    // Invited (to the organization only); the group grants a role in Beta.
    t.admin.invite(&t.org, "sam@example.com", None, None).await;
    t.admin.ok(Method::PUT, &format!("/api/spaces/{}/members", t.beta), Some(json!({ "kind": "group", "principal": "analysts", "role": "editor" }))).await;
    let (status, to, cookie) = sso(s, &idp, claims(&idp, "sub-1", "sam@example.com", &["analysts"]), "idp").await;
    assert_eq!((status, to.as_str()), (303, "/w/abc"), "back where the sign-in started");
    let sam = Session { cookie, ..s.anonymous() };
    let (st, me) = sam.get("/api/me").await;
    assert_eq!(st, 200);
    assert_eq!(me["user"]["email"], "sam@example.com");
    assert_eq!(me["user"]["sign_in"], json!(["sso"]));
    assert_eq!(sam.get(&format!("/api/workbooks/{}", t.wb)).await.1["role"], "editor", "through the group");
    assert_eq!(sam.get(&format!("/api/workbooks/{}", t.wa)).await.0, 404);
    // Next sign-in without the group: the role is gone. The same subject is the same user,
    // even with another email.
    let (_, _, cookie) = sso(s, &idp, claims(&idp, "sub-1", "sam.renamed@example.com", &[]), "idp").await;
    let sam2 = Session { cookie, ..s.anonymous() };
    assert_eq!(sam2.get("/api/me").await.1["user"]["id"], me["user"]["id"]);
    assert_eq!(sam2.get(&format!("/api/workbooks/{}", t.wb)).await.0, 404, "group removed at the identity provider");

    // Refused: wrong audience, wrong issuer, expired, another nonce, a stranger's key, no signature.
    let mut cases = Vec::new();
    let base = || claims(&idp, "sub-2", "pat@example.com", &[]);
    t.admin.invite(&t.org, "pat@example.com", None, None).await;
    let mut c = base();
    c["aud"] = json!("someone-else");
    cases.push(("audience", c, "idp"));
    let mut c = base();
    c["iss"] = json!("http://127.0.0.1:1");
    cases.push(("issuer", c, "idp"));
    let mut c = base();
    c["exp"] = json!(alephgrid_server::auth::now_ms() / 1000 - 3600);
    cases.push(("expired", c, "idp"));
    let mut c = base();
    c["nonce"] = json!("replayed");
    cases.push(("nonce", c, "idp"));
    let mut c = base();
    c["aud"] = json!([CLIENT_ID, "other"]);
    c["azp"] = json!("other");
    cases.push(("authorized party", c, "idp"));
    cases.push(("foreign key", base(), "other"));
    cases.push(("unsigned", base(), "none"));
    let mut c = base();
    c["email_verified"] = json!(false);
    cases.push(("unverified email", c, "idp"));
    let mut c = base();
    c["sub"] = json!("sub-3");
    c["email"] = json!("viewer@example.com");
    cases.push(("email of a password account", c, "idp"));
    for (what, c, signer) in cases {
        let (status, to, cookie) = sso(s, &idp, c, signer).await;
        assert_eq!(status, 303, "{what}");
        assert!(to.starts_with("/login?error="), "{what}: {to}");
        assert!(cookie.is_none(), "{what} must not sign in");
    }
    // The state is good once.
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let r = http.get(s.url("/api/auth/oidc/callback?code=x&state=never-issued")).send().await.unwrap();
    assert!(r.headers()["location"].to_str().unwrap().contains("expired"), "{:?}", r.headers());
    // The valid one still works after all that.
    let (_, to, cookie) = sso(s, &idp, base(), "idp").await;
    assert_eq!(to, "/w/abc");
    assert!(cookie.is_some());
    // Only the server's own paths are followed afterwards.
    assert_eq!(alephgrid_server::auth::oidc::safe_next("//evil.example/x"), "/");
    assert_eq!(alephgrid_server::auth::oidc::safe_next("https://evil.example"), "/");
    assert_eq!(alephgrid_server::auth::oidc::safe_next("/w/1?x=2"), "/w/1?x=2");
    idp.task.abort();
    t.server.stop().await;
}

/// 7.10: changes, imports, exports, downloads and function publication are attributed to who
/// made them, filtered by workbook for admins, and survive a restart right after them.
#[tokio::test(flavor = "multi_thread")]
async fn changes_imports_exports_and_publication_are_audited() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let wa = t.wa.clone();
    let user = |s: &Session| format!("user:{}", s.user_id());
    // An import over HTTP: audited, and the workbook's history names the importer.
    let (s, v) = t.editor.send_bytes(Method::POST, &format!("/api/workbooks/{wa}/import?filename=more.csv"), csv()).await;
    assert_eq!(s, 200, "{v}");
    let mut editor = t.editor.ws(&wa).await.unwrap();
    let history = editor.ok("history", json!({ "after": 0, "limit": 100 })).await;
    let import = history.as_array().unwrap().iter().rev().find(|h| h["op"] == "import").unwrap().clone();
    assert_eq!(import["actor"], user(&t.editor).as_str());
    // A change, a function published and pinned.
    let view = editor.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    editor.ok("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "s", "value": "audited" }] })).await;
    editor.ok("publish_function", json!({ "definition": { "name": "TWICE", "kind": "formula", "arguments": ["x"], "source": "x*2", "documentation": "", "tests": [] } })).await;
    editor.ok("pin_functions", json!({ "pins": { "TWICE": 1 } })).await;
    // A viewer's refused change leaves nothing; their exports are recorded.
    let mut viewer = t.viewer.ws(&wa).await.unwrap();
    let vview = viewer.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    assert_eq!(viewer.code("edit", json!({ "view_id": vview, "edits": [{ "row": 0, "column": "s", "value": "x" }] })).await.as_deref(), Some("forbidden"));
    let export = viewer.ok("export_query", json!({ "sql": "SELECT * FROM t" })).await;
    assert_eq!(t.viewer.get(&format!("/api/workbooks/{wa}/downloads/{}", export["download"].as_str().unwrap())).await.0, 200);
    let spec = common::urlencode(&json!({ "table": "t" }).to_string());
    assert_eq!(t.viewer.get(&format!("/api/workbooks/{wa}/export?spec={spec}&format=csv")).await.0, 200);
    assert_eq!(t.viewer.get(&format!("/api/workbooks/{wa}/export.xlsx")).await.0, 200);
    // One last change, then stop at once: it is recorded all the same.
    editor.ok("edit", json!({ "view_id": view, "edits": [{ "row": 1, "column": "s", "value": "last" }] })).await;
    editor.close().await;
    viewer.close().await;
    let (org, admin_email) = (t.org.clone(), ADMIN_EMAIL);
    let (editor_actor, viewer_actor) = (user(&t.editor), user(&t.viewer));
    t.server.stop().await;
    let server = TestServer::start_accounts(dir.path(), |_| {}).await;
    let admin = server.login(admin_email, ADMIN_PASSWORD).await.unwrap();
    let (s, events) = admin.get(&format!("/api/orgs/{org}/audit?workbook={wa}&limit=1000")).await;
    assert_eq!(s, 200, "{events}");
    let events = events.as_array().unwrap();
    assert!(events.iter().all(|e| e["workbook"] == wa.as_str()), "filtered by workbook");
    let of = |actor: &str, action: &str| events.iter().filter(|e| e["actor"] == actor && e["action"] == action).cloned().collect::<Vec<_>>();
    let imported = of(&editor_actor, "data_imported");
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0]["detail"]["file"], "more.csv");
    assert_eq!(imported[0]["space_id"], t.alpha.as_str());
    let changed = of(&editor_actor, "workbook_changed");
    let ops: Vec<&str> = changed.iter().rev().map(|e| e["detail"]["op"].as_str().unwrap()).collect();
    assert_eq!(ops, vec!["edit", "publish_function", "pin_functions", "edit"], "in order, the last one included");
    assert_eq!(of(&editor_actor, "function_published")[0]["detail"]["function"], "TWICE");
    assert_eq!(of(&editor_actor, "functions_pinned").len(), 1);
    assert!(of(&viewer_actor, "workbook_changed").is_empty(), "refused changes are not changes");
    let exported: BTreeSet<String> = of(&viewer_actor, "data_exported").iter().map(|e| e["detail"]["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(exported, ["query", "table", "xlsx"].into_iter().map(String::from).collect());
    assert_eq!(of(&viewer_actor, "export_downloaded").len(), 1);
    server.stop().await;
}

const SLOW_SQL: &str = "SELECT sum(i) AS s FROM range(100000000000) r(i)";

/// 7.07: per-space limits on storage, workbooks, bulk jobs and open workbooks, set by
/// organization admins over server defaults, and the usage they see.
#[tokio::test(flavor = "multi_thread")]
async fn space_quotas_limit_storage_workbooks_jobs_and_open_workbooks() {
    let dir = tempfile::tempdir().unwrap();
    let t = team_with(dir.path(), |c| c.quota.workbooks = Some(2)).await;
    let (alpha, beta, org) = (t.alpha.clone(), t.beta.clone(), t.org.clone());
    let quota = |space: &str, q: Value| {
        let (admin, space) = (t.admin.clone(), space.to_string());
        async move { admin.ok(Method::PUT, &format!("/api/spaces/{space}/quota"), Some(q)).await }
    };
    // Workbooks: the default of 2 (Alpha holds one).
    let new = |s: &Session, space: &str| {
        let (s, space) = (s.clone(), space.to_string());
        async move { s.post("/api/workbooks", json!({ "name": "n", "space": space })).await }
    };
    assert_eq!(new(&t.owner, &alpha).await.0, 200);
    let (s, v) = new(&t.owner, &alpha).await;
    assert_eq!(s, 507, "{v}");
    assert!(v["error"].as_str().unwrap().contains("holds 2 workbook(s)"), "{v}");
    // Owners cannot raise their own limits; organization admins can.
    assert_eq!(t.owner.send(Method::PUT, &format!("/api/spaces/{alpha}/quota"), Some(json!({ "workbooks": 10 }))).await.0, 403);
    assert_eq!(t.outsider.send(Method::PUT, &format!("/api/spaces/{alpha}/quota"), Some(json!({ "workbooks": 10 }))).await.0, 404);
    let u = quota(&alpha, json!({ "workbooks": 10 })).await;
    assert_eq!(u["limits"]["workbooks"], 10);
    assert_eq!(u["overrides"]["workbooks"], 10);
    assert_eq!(new(&t.owner, &alpha).await.0, 200);

    // Storage: over the limit, imports and bulk changes are refused and change nothing;
    // small edits still work, so people can make room.
    quota(&alpha, json!({ "workbooks": 10, "storage_bytes": 1 })).await;
    let (s, v) = t.editor.send_bytes(Method::POST, &format!("/api/workbooks/{}/import?filename=big.csv", t.wa), csv()).await;
    assert_eq!(s, 507, "{v}");
    let mut ws = t.editor.ws(&t.wa).await.unwrap();
    assert_eq!(ws.code("create_table_from_query", json!({ "name": "copy", "sql": "SELECT * FROM t" })).await.as_deref(), Some("quota"));
    let tables: Vec<String> = ws.ok("tables", json!({})).await.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(tables, vec!["t"], "nothing was added");
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    ws.ok("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "s", "value": "still editable" }] })).await;
    assert_eq!(new(&t.owner, &alpha).await.0, 507, "no new workbooks either");
    quota(&alpha, json!({ "workbooks": 10 })).await;
    ws.ok("create_table_from_query", json!({ "name": "copy", "sql": "SELECT * FROM t" })).await;

    // Jobs: one bulk job at a time in Alpha; other spaces are not affected.
    quota(&alpha, json!({ "workbooks": 10, "jobs": 1 })).await;
    let slow = ws.send("query", json!({ "sql": SLOW_SQL })).await;
    let admin = t.admin.clone();
    let running = || {
        let admin = admin.clone();
        let (org, alpha) = (org.clone(), alpha.clone());
        async move {
            let (_, usage) = admin.get(&format!("/api/orgs/{org}/usage")).await;
            usage.as_array().unwrap().iter().find(|u| u["space"]["id"] == alpha.as_str()).unwrap()["jobs"].as_u64().unwrap()
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while running().await == 0 {
        assert!(std::time::Instant::now() < deadline, "the slow query never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut other = t.viewer.ws(&t.wa).await.unwrap();
    assert_eq!(other.code("query", json!({ "sql": "SELECT 1" })).await.as_deref(), Some("quota"));
    other.ok("tables", json!({})).await;
    let mut beta_ws = t.admin.ws(&t.wb).await.unwrap();
    beta_ws.ok("query", json!({ "sql": "SELECT 1" })).await;
    ws.ok("cancel", json!({ "request": slow })).await;
    assert!(ws.reply(slow).await.unwrap()["ok"] == false);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while running().await > 0 {
        assert!(std::time::Instant::now() < deadline, "the job slot was not released");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    other.ok("query", json!({ "sql": "SELECT 1" })).await;

    // Open workbooks: Beta may have one open at a time.
    quota(&beta, json!({ "open_workbooks": 1 })).await;
    let (s, v) = new(&t.admin, &beta).await;
    assert_eq!(s, 429, "Beta's workbook is open: {v}");
    beta_ws.close().await;
    t.admin.ok(Method::POST, &format!("/api/workbooks/{}/runtime", t.wb), Some(json!({ "action": "stop" }))).await;
    let (s, v) = new(&t.admin, &beta).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = t.admin.get(&format!("/api/workbooks/{}", t.wb)).await;
    assert_eq!(s, 429, "the new one is open now: {v}");

    // Usage as admins see it.
    let (_, usage) = t.admin.get(&format!("/api/orgs/{org}/usage")).await;
    let a = usage.as_array().unwrap().iter().find(|u| u["space"]["id"] == alpha.as_str()).unwrap().clone();
    assert!(a["storage_bytes"].as_u64().unwrap() > 0);
    assert_eq!(a["workbooks"], 3);
    assert_eq!(a["limits"]["jobs"], 1);
    assert_eq!(t.owner.get(&format!("/api/orgs/{org}/usage")).await.0, 403);
    let (_, all) = t.admin.get("/api/admin/usage").await;
    assert_eq!(all["defaults"]["workbooks"], 2);
    let events = t.server.state.auth.store.audit_events(Some(&org), None, None, 1000).unwrap();
    assert!(events.iter().any(|e| e.action == "space_quota_changed" && e.space_id.as_deref() == Some(alpha.as_str())));
    for w in [ws, other] {
        w.close().await;
    }
    t.server.stop().await;
}

/// 7.11: a space library shares saved queries, views and function versions; publishing,
/// changing entries and using them need different permissions, and a space can keep
/// publishing to its owners.
#[tokio::test(flavor = "multi_thread")]
async fn space_libraries_share_saved_objects_with_distinct_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path()).await;
    let (alpha, wa) = (t.alpha.clone(), t.wa.clone());
    let lib = format!("/api/spaces/{alpha}/library");
    // The editor saves a query and a view and publishes them.
    let mut ed = t.editor.ws(&wa).await.unwrap();
    let q = ed.ok("save_query", json!({ "name": "Big", "sql": "SELECT n FROM t WHERE n > 1" })).await;
    let v = ed.ok("save_view", json!({ "name": "Desc", "spec": { "table": "t", "sort": [{ "column": "n", "desc": true }], "filters": [] } })).await;
    let published = t.editor.ok(Method::POST, &lib, Some(json!({ "workbook": wa, "kind": "query", "object": q["id"] }))).await;
    assert_eq!((published["name"].as_str(), published["latest"]["version"].as_u64()), (Some("Big"), Some(1)));
    assert_eq!(published["latest"]["payload"]["sql"], "SELECT n FROM t WHERE n > 1");
    let view_entry = t.editor.ok(Method::POST, &lib, Some(json!({ "workbook": wa, "kind": "view", "object": v["id"] }))).await;
    let entry = published["id"].as_str().unwrap().to_string();
    // Everyone in the space reads it; a viewer neither publishes nor copies it in.
    let (_, list) = t.viewer.get(&lib).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(t.viewer.post(&lib, json!({ "workbook": wa, "kind": "query", "object": q["id"] })).await.0, 403);
    assert_eq!(t.viewer.post(&format!("/api/workbooks/{wa}/library"), json!({ "entry": entry })).await.0, 403);
    assert_eq!(t.outsider.get(&lib).await.0, 404);
    // Another workbook of the space takes them in; the viewer runs the query there.
    let wa2 = t.editor.ok(Method::POST, "/api/workbooks", Some(json!({ "name": "wa2", "space": alpha }))).await["id"].as_str().unwrap().to_string();
    t.editor.send_bytes(Method::POST, &format!("/api/workbooks/{wa2}/import?filename=t.csv"), csv()).await;
    let used = t.editor.ok(Method::POST, &format!("/api/workbooks/{wa2}/library"), Some(json!({ "entry": entry }))).await;
    assert_eq!(used["result"]["sql"], "SELECT n FROM t WHERE n > 1");
    t.editor.ok(Method::POST, &format!("/api/workbooks/{wa2}/library"), Some(json!({ "entry": view_entry["id"] }))).await;
    let mut viewer = t.viewer.ws(&wa2).await.unwrap();
    let saved = viewer.ok("saved_queries", json!({})).await;
    assert_eq!(saved[0]["name"], "Big");
    let rows = viewer.ok("query", json!({ "sql": saved[0]["sql"] })).await;
    assert_eq!(rows["rows"].as_array().unwrap().len(), 2, "{rows}");
    assert_eq!(viewer.ok("saved_views", json!({ "table": "t" })).await[0]["name"], "Desc");
    let history = viewer.ok("history", json!({ "after": 0, "limit": 100 })).await;
    let entry_use = history.as_array().unwrap().iter().find(|h| h["op"] == "use_library").unwrap().clone();
    assert_eq!(entry_use["actor"], format!("user:{}", t.editor.user_id()).as_str());
    // A new version; old versions stay usable.
    ed.ok("save_query", json!({ "saved": q["id"], "name": "Big", "sql": "SELECT n FROM t WHERE n > 2" })).await;
    let v2 = t.editor.ok(Method::POST, &lib, Some(json!({ "workbook": wa, "kind": "query", "object": q["id"] }))).await;
    assert_eq!((v2["id"].as_str(), v2["latest"]["version"].as_u64()), (Some(entry.as_str()), Some(2)));
    let (_, detail) = t.viewer.get(&format!("{lib}/{entry}")).await;
    assert_eq!(detail["versions"].as_array().unwrap().iter().map(|v| v["payload"]["sql"].as_str().unwrap()).collect::<Vec<_>>(), vec!["SELECT n FROM t WHERE n > 1", "SELECT n FROM t WHERE n > 2"]);
    let old = t.editor.ok(Method::POST, &format!("/api/workbooks/{wa2}/library"), Some(json!({ "entry": entry, "version": 1, "name": "Big v1" }))).await;
    assert_eq!(old["result"]["sql"], "SELECT n FROM t WHERE n > 1");
    // Publishing kept to owners: the editor's open connection follows at once.
    t.owner.ok(Method::PATCH, &format!("/api/spaces/{alpha}"), Some(json!({ "publish_role": "owner" }))).await;
    assert_eq!(t.editor.post(&lib, json!({ "workbook": wa, "kind": "query", "object": q["id"] })).await.0, 403);
    let fdef = json!({ "definition": { "name": "TRIPLE", "kind": "formula", "arguments": ["x"], "source": "x*3", "documentation": "", "tests": [] } });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ed.code("publish_function", fdef.clone()).await.as_deref() != Some("forbidden") {
        assert!(std::time::Instant::now() < deadline, "the editor's connection kept publishing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ed.ok("save_query", json!({ "name": "Still editing", "sql": "SELECT 1" })).await;
    let mut owner = t.owner.ws(&wa).await.unwrap();
    owner.ok("publish_function", fdef.clone()).await;
    let f = t.owner.ok(Method::POST, &lib, Some(json!({ "workbook": wa, "kind": "function", "object": "TRIPLE" }))).await;
    assert_eq!(f["latest"]["payload"]["definition"]["source"], "x*3");
    assert_eq!(t.editor.post(&format!("/api/workbooks/{wa2}/library"), json!({ "entry": f["id"] })).await.0, 403, "a function becomes a published version");
    t.owner.ok(Method::POST, &format!("/api/workbooks/{wa2}/library"), Some(json!({ "entry": f["id"] }))).await;
    assert_eq!(viewer.ok("functions", json!({})).await[0]["name"], "TRIPLE");
    // Changing entries: their publisher or an owner.
    assert_eq!(t.viewer.send(Method::PATCH, &format!("{lib}/{entry}"), Some(json!({ "name": "Mine" }))).await.0, 403);
    t.editor.ok(Method::PATCH, &format!("{lib}/{entry}"), Some(json!({ "name": "Bigger" }))).await;
    t.owner.ok(Method::PATCH, &format!("{lib}/{}", view_entry["id"].as_str().unwrap()), Some(json!({ "retired": true }))).await;
    let names: Vec<String> = t.viewer.get(&lib).await.1.as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, vec!["TRIPLE", "Bigger"]);
    assert_eq!(t.editor.post(&format!("/api/workbooks/{wa2}/library"), json!({ "entry": view_entry["id"] })).await.0, 400, "retired");
    // Libraries do not cross spaces.
    let (s, v) = t.admin.post(&lib, json!({ "workbook": t.wb, "kind": "query", "object": "x" })).await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(t.admin.post(&format!("/api/workbooks/{}/library", t.wb), json!({ "entry": entry })).await.0, 404);
    t.server.state.auth.flush_audit();
    let events = t.server.state.auth.store.audit_events(Some(&t.org), None, None, 1000).unwrap();
    for action in ["library_published", "library_used", "library_changed", "space_publish_role_changed"] {
        assert!(events.iter().any(|e| e.action == action), "{action}");
    }
    for w in [ed, viewer, owner] {
        w.close().await;
    }
    t.server.stop().await;
}
