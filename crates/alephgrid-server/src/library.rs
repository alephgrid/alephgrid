//! Space libraries (7.11): saved queries, views and function versions published from a
//! workbook of a space for everyone in it to read and to copy into their workbooks.
//!
//! Permissions are kept apart:
//!
//! * **publish** an entry, or a new version of it: the space's publish role (`editor` by
//!   default, or `owner`), and viewing the source workbook, which must be in the space;
//! * **read** entries (their SQL, view spec or function source) and **execute** them by
//!   copying into a workbook to run: any role in the space, then `edit` on the target workbook
//!   (`publish` there for a function, which becomes a new version of that workbook's function);
//! * **change** an entry (rename, retire): the member who created it, or an owner.
//!
//! Versions are immutable. Entries live in the control database, never in a workbook, so a
//! library survives the workbook it was published from.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use duckdb::{params, OptionalExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ts_rs::TS;

use crate::auth::{not_found, Cap, Denied, Principal, SpaceRole, Store};
use crate::http::{blocking, ApiResult};
use crate::state::{Shared, Work};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LibraryKind {
    Query,
    View,
    Function,
}

impl LibraryKind {
    fn as_str(self) -> &'static str {
        match self {
            LibraryKind::Query => "query",
            LibraryKind::View => "view",
            LibraryKind::Function => "function",
        }
    }

    fn parse(s: &str) -> LibraryKind {
        match s {
            "view" => LibraryKind::View,
            "function" => LibraryKind::Function,
            _ => LibraryKind::Query,
        }
    }
}

/// One immutable version of an entry.
#[derive(Debug, Clone, Serialize, TS)]
pub struct LibraryVersion {
    pub version: u64,
    /// The saved query (`name`, `kind`, `sql`, `params`), view (`name`, `spec`, `widths`) or
    /// function definition as published.
    #[ts(type = "Record<string, unknown>")]
    pub payload: Value,
    pub source_workbook: String,
    pub published_by: String,
    pub published_at: u64,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct LibraryEntry {
    pub id: String,
    pub space_id: String,
    pub kind: LibraryKind,
    pub name: String,
    pub created_by: String,
    pub created_at: u64,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<u64>,
    pub latest: LibraryVersion,
}

fn bad(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Denied::BadRequest(msg.into()))
}

// ------------------------------------------------------------------ store

fn entries(store: &Store, space: &str, entry: Option<&str>) -> anyhow::Result<Vec<LibraryEntry>> {
    store.txn(|c| {
        let mut s = c.prepare(
            "SELECT e.id, e.space_id, e.kind, e.name, e.created_by, e.created_at, e.retired_at, v.version, v.payload, v.source_workbook, v.published_by, v.published_at
             FROM library_entries e JOIN library_versions v ON v.entry_id = e.id
             WHERE e.space_id = ?1 AND (?2 IS NULL OR e.id = ?2)
               AND v.version = (SELECT max(version) FROM library_versions WHERE entry_id = e.id)
             ORDER BY e.kind, lower(e.name)",
        )?;
        let rows = s.query_map(params![space, entry], |r| {
            let payload: String = r.get(8)?;
            Ok(LibraryEntry {
                id: r.get(0)?,
                space_id: r.get(1)?,
                kind: LibraryKind::parse(&r.get::<_, String>(2)?),
                name: r.get(3)?,
                created_by: r.get(4)?,
                created_at: r.get::<_, i64>(5)? as u64,
                retired_at: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                latest: LibraryVersion {
                    version: r.get::<_, i64>(7)? as u64,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                    source_workbook: r.get(9)?,
                    published_by: r.get(10)?,
                    published_at: r.get::<_, i64>(11)? as u64,
                },
            })
        })?;
        Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
    })
}

fn versions(store: &Store, entry: &str) -> anyhow::Result<Vec<LibraryVersion>> {
    store.txn(|c| {
        let mut s = c.prepare("SELECT version, payload, source_workbook, published_by, published_at FROM library_versions WHERE entry_id = ? ORDER BY version")?;
        let rows = s.query_map(params![entry], |r| {
            let payload: String = r.get(1)?;
            Ok(LibraryVersion {
                version: r.get::<_, i64>(0)? as u64,
                payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                source_workbook: r.get(2)?,
                published_by: r.get(3)?,
                published_at: r.get::<_, i64>(4)? as u64,
            })
        })?;
        Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
    })
}

/// Publish `payload` as a new entry, or as the next version of the live entry of that kind
/// and name. Returns the entry.
fn publish(store: &Store, space: &str, kind: LibraryKind, name: &str, payload: &Value, workbook: &str, by: &str) -> anyhow::Result<String> {
    store.txn(|c| {
        let now = crate::auth::now_ms() as i64;
        let existing: Option<String> = c
            .query_row(
                "SELECT id FROM library_entries WHERE space_id = ? AND kind = ? AND lower(name) = lower(?) AND retired_at IS NULL",
                params![space, kind.as_str(), name],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                let id = crate::auth::random_id();
                c.execute("INSERT INTO library_entries VALUES (?, ?, ?, ?, ?, ?, NULL)", params![id, space, kind.as_str(), name, by, now])?;
                id
            }
        };
        let next: i64 = c.query_row("SELECT coalesce(max(version), 0) + 1 FROM library_versions WHERE entry_id = ?", params![id], |r| r.get(0))?;
        c.execute("INSERT INTO library_versions VALUES (?, ?, ?, ?, ?, ?)", params![id, next, payload.to_string(), workbook, by, now])?;
        Ok(id)
    })
}

// ----------------------------------------------------------------- routes

/// The live entries of a space's library (`?retired=true` adds retired ones).
pub async fn list(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(space): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_space(&p, &space, Cap::View)?;
            Ok(entries(&state.auth.store, &space, None)?.into_iter().filter(|e| e.retired_at.is_none()).collect::<Vec<_>>())
        })
        .await?,
    ))
}

/// One entry with every version.
pub async fn entry(State(state): State<Shared>, Extension(p): Extension<Principal>, Path((space, entry)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_space(&p, &space, Cap::View)?;
            let e = entries(&state.auth.store, &space, Some(&entry))?.into_iter().next().ok_or_else(|| not_found("library entry not found"))?;
            Ok(json!({ "entry": e, "versions": versions(&state.auth.store, &entry)? }))
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct PublishBody {
    workbook: String,
    kind: LibraryKind,
    /// A saved query's or view's id, or a function's name.
    object: String,
    /// The function version to publish (the pinned one by default).
    #[serde(default)]
    version: Option<u32>,
}

/// Publish a saved query, view or function version of one of the space's workbooks.
pub async fn publish_entry(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(space): Path<String>, Json(b): Json<PublishBody>) -> ApiResult<impl IntoResponse> {
    let (s, pp, sp, wb) = (state.clone(), p.clone(), space.clone(), b.workbook.clone());
    let (space_rec, _) = blocking(move || {
        let out = s.auth.require_space(&pp, &sp, Cap::Publish)?;
        let (wspace, _) = s.require_workbook(&pp, &wb, Cap::View)?;
        if wspace.id != sp {
            return Err(bad("publish from a workbook of this space"));
        }
        Ok(out)
    })
    .await?;
    let h = {
        let (s, w) = (state.clone(), b.workbook.clone());
        blocking(move || s.get(&w)).await?
    };
    let (kind, object, version) = (b.kind, b.object.clone(), b.version);
    let (name, payload) = h
        .run(
            crate::http::http_job(crate::sched::Class::Interactive, &state),
            None,
            None,
            Work::Read(Box::new(move |r: &alephgrid_core::Reader| -> anyhow::Result<(String, Value)> {
                match kind {
                    LibraryKind::Query => {
                        let q = r.saved_queries()?.into_iter().find(|q| q.id == object).ok_or_else(|| not_found("saved query not found"))?;
                        Ok((q.name.clone(), json!({ "name": q.name, "kind": q.kind, "sql": q.sql, "params": q.params, "version": q.version })))
                    }
                    LibraryKind::View => {
                        let v = r.saved_views(None)?.into_iter().find(|v| v.id == object).ok_or_else(|| not_found("saved view not found"))?;
                        Ok((v.name.clone(), json!({ "name": v.name, "spec": v.spec, "widths": v.widths })))
                    }
                    LibraryKind::Function => {
                        let f = r.functions()?.into_iter().find(|f| f.name.eq_ignore_ascii_case(&object)).ok_or_else(|| not_found("function not found"))?;
                        let want = version.unwrap_or(f.pinned);
                        let v = f.versions.iter().find(|v| v.version == want).ok_or_else(|| not_found(format!("{} has no version {want}", f.name)))?;
                        Ok((f.name.clone(), json!({ "definition": v.definition, "version": v.version })))
                    }
                }
            })),
        )
        .await?;
    let out = blocking(move || {
        let id = publish(&state.auth.store, &space, b.kind, &name, &payload, &b.workbook, &p.actor())?;
        let e = entries(&state.auth.store, &space, Some(&id))?.into_iter().next().ok_or_else(|| not_found("library entry not found"))?;
        state.auth.audit(&p, "library_published", Some(&space_rec.org_id), Some(&space), Some(&b.workbook), json!({ "entry": id, "kind": b.kind, "name": name, "version": e.latest.version }));
        Ok(e)
    })
    .await?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct EntryPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    retired: Option<bool>,
}

/// Rename or retire an entry: whoever created it, or an owner of the space.
pub async fn change_entry(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path((space, entry)): Path<(String, String)>,
    Json(b): Json<EntryPatch>,
) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let (sp, role) = state.auth.require_space(&p, &space, Cap::View)?;
        let e = entries(&state.auth.store, &space, Some(&entry))?.into_iter().next().ok_or_else(|| not_found("library entry not found"))?;
        if e.created_by != p.actor() && role < SpaceRole::Owner {
            return Err(crate::auth::forbidden("change library entries other people published"));
        }
        state.auth.store.txn(|c| {
            if let Some(n) = b.name.as_deref().map(str::trim) {
                if n.is_empty() {
                    return Err(bad("name cannot be empty"));
                }
                c.execute("UPDATE library_entries SET name = ? WHERE id = ?", params![n, entry])?;
            }
            match b.retired {
                Some(true) => c.execute("UPDATE library_entries SET retired_at = ? WHERE id = ?", params![crate::auth::now_ms() as i64, entry])?,
                Some(false) => c.execute("UPDATE library_entries SET retired_at = NULL WHERE id = ?", params![entry])?,
                None => 0,
            };
            Ok(())
        })?;
        state.auth.audit(&p, "library_changed", Some(&sp.org_id), Some(&space), None, json!({ "entry": entry, "name": b.name, "retired": b.retired }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct UseBody {
    entry: String,
    #[serde(default)]
    version: Option<u64>,
    /// Name it gets in the workbook (the entry's by default).
    #[serde(default)]
    name: Option<String>,
}

/// Copy a library entry of the workbook's space into the workbook: a saved query or view
/// (`edit`), or a new version of a function (`publish`).
pub async fn use_entry(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>, Json(b): Json<UseBody>) -> ApiResult<impl IntoResponse> {
    let (s, pp, wid, entry_id) = (state.clone(), p.clone(), id.clone(), b.entry.clone());
    let (space, entry, version) = blocking(move || {
        let (space, _) = s.require_workbook(&pp, &wid, Cap::Edit)?;
        let e = entries(&s.auth.store, &space.id, Some(&entry_id))?.into_iter().next().ok_or_else(|| not_found("library entry not found"))?;
        if e.retired_at.is_some() {
            return Err(bad("this library entry was retired"));
        }
        if e.kind == LibraryKind::Function {
            s.require_workbook(&pp, &wid, Cap::Publish)?;
        }
        let v = match b.version {
            Some(n) => versions(&s.auth.store, &e.id)?.into_iter().find(|v| v.version == n).ok_or_else(|| not_found(format!("version {n} not found")))?,
            None => e.latest.clone(),
        };
        Ok((space, e, v))
    })
    .await?;
    let h = {
        let (s, w) = (state.clone(), id.clone());
        blocking(move || s.get(&w)).await?
    };
    let (kind, payload, name, actor) = (entry.kind, version.payload.clone(), b.name.clone(), p.actor());
    let summary = format!("use library entry {:?} v{}", entry.name, version.version);
    let (result, revision) = h
        .run(
            crate::http::http_job(crate::sched::Class::Interactive, &state),
            None,
            None,
            Work::Write(Box::new(move |wb: &mut alephgrid_core::Workbook| {
                let m = alephgrid_core::Mutation { id: None, op: "use_library", actor: Some(&actor), summary };
                let c = wb.mutate(m, None, |wb| {
                    let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
                    Ok(match kind {
                        LibraryKind::Query => {
                            let qk: alephgrid_core::QueryKind = serde_json::from_value(payload["kind"].clone())?;
                            let params: Vec<alephgrid_core::QueryParam> = serde_json::from_value(payload["params"].clone())?;
                            let n = name.unwrap_or_else(|| payload["name"].as_str().unwrap_or("Query").to_string());
                            json!(wb.save_query(None, &n, qk, payload["sql"].as_str().unwrap_or(""), &params)?)
                        }
                        LibraryKind::View => {
                            let spec: alephgrid_core::ViewSpec = serde_json::from_value(payload["spec"].clone())?;
                            let widths = serde_json::from_value(payload["widths"].clone()).unwrap_or_default();
                            let n = name.unwrap_or_else(|| payload["name"].as_str().unwrap_or("View").to_string());
                            json!(wb.save_view(None, &n, spec, widths)?)
                        }
                        LibraryKind::Function => {
                            let def: alephgrid_core::FunctionDefinition = serde_json::from_value(payload["definition"].clone())?;
                            json!(wb.publish_function(def)?)
                        }
                    })
                })?;
                Ok((c.result, c.revision))
            })),
        )
        .await?;
    let event = match kind {
        LibraryKind::Query => crate::ws::Event::QueriesChanged { origin: 0, revision },
        LibraryKind::View => crate::ws::Event::ViewsChanged { table: result["spec"]["table"].as_str().unwrap_or("").to_string(), origin: 0, revision },
        LibraryKind::Function => crate::ws::Event::Resync {},
    };
    let _ = h.events.send(event);
    state.auth.record(&p, "library_used", &space, &id, json!({ "entry": entry.id, "kind": kind, "version": version.version, "revision": revision }));
    if kind == LibraryKind::Function {
        state.auth.record(&p, "function_published", &space, &id, json!({ "function": result["name"], "from_library": entry.id, "revision": revision }));
    }
    Ok(Json(json!({ "result": result, "revision": revision })))
}
