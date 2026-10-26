//! WebSocket protocol. See docs/PROTOCOL.md.
//!
//! Client -> server: `{"id": 7, "op": "fetch", "view_id": 1, "row_start": 0, "row_count": 200}`
//! Server -> client: `{"id": 7, "ok": true, "result": {...}}` or `{"id": 7, "ok": false, "error": "..."}`
//! Server pushes:    `{"event": "cells_changed", "table": "flights", "origin": 3}`

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use rowfinity_engine::{CellEdit, SheetInput, ViewSpec, Workbook};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use ts_rs::TS;

use crate::state::{Shared, WorkbookHandle};

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Cell values changed; refetch visible rows of this table.
    CellsChanged { table: String, origin: u64 },
    /// Rows were inserted/deleted or columns changed; views on this table were rebuilt.
    TableChanged { table: String, origin: u64 },
    /// Tables or sheets were added, removed or renamed.
    TablesChanged { origin: u64 },
    /// Sheet cells changed. `sheet` is None when every sheet may have changed (undo/redo).
    SheetChanged { sheet: Option<u32>, origin: u64 },
    /// The workbook was moved to the trash; the server closes the connection.
    WorkbookDeleted { origin: u64 },
}

impl Event {
    /// Events after which the home-screen summary (counts, rows) may be stale.
    pub fn changes_structure(&self) -> bool {
        matches!(self, Event::TableChanged { .. } | Event::TablesChanged { .. })
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    id: u64,
    #[serde(flatten)]
    op: Op,
}

/// Every request a client can send (`{"id": n, "op": "...", ...params}`).
#[derive(Debug, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Tables,
    OpenView { spec: ViewSpec },
    UpdateView { view_id: u64, spec: ViewSpec },
    ViewInfo { view_id: u64 },
    CloseView { view_id: u64 },
    Fetch { view_id: u64, row_start: u64, row_count: u64 },
    Edit { view_id: u64, edits: Vec<CellEdit> },
    InsertRows { view_id: u64, at: u64, count: u64 },
    DeleteRows { view_id: u64, start: u64, count: u64 },
    Stats { view_id: u64, column: String, start: u64, end: u64 },
    AddColumn { table: String, name: String, sql_type: String },
    AddFormulaColumn { table: String, name: String, formula: String },
    UpdateFormula { table: String, name: String, formula: String },
    DropColumn { table: String, name: String },
    RenameColumn { table: String, name: String, new_name: String },
    Query { sql: String, #[serde(default = "default_limit")] #[ts(optional, as = "Option<usize>")] limit: usize },
    CreateTableFromQuery { name: String, sql: String },
    CreateDemoTable { #[serde(default = "default_demo_rows")] #[ts(optional, as = "Option<u64>")] rows: u64 },
    DropTable { table: String },
    RenameTable { table: String, new_name: String },
    // Sheets (IronCalc). Rows/columns are 0-based.
    Sheets,
    SheetCreate { #[serde(default)] #[ts(optional, as = "Option<String>")] name: String },
    SheetRename { sheet: u32, name: String },
    SheetDelete { sheet: u32 },
    SheetFetch { sheet: u32, row_start: u32, row_count: u32, column_count: u32 },
    SheetSet { sheet: u32, inputs: Vec<SheetInput> },
    SheetUndo,
    SheetRedo,
    SheetInsertRows { sheet: u32, at: u32, count: u32 },
    SheetDeleteRows { sheet: u32, at: u32, count: u32 },
    SheetInsertColumns { sheet: u32, at: u32, count: u32 },
    SheetDeleteColumns { sheet: u32, at: u32, count: u32 },
    CreateSheetFromQuery { name: String, sql: String },
    SheetToTable { sheet: u32, name: String },
}

fn default_limit() -> usize {
    500
}

fn default_demo_rows() -> u64 {
    1_000_000
}

struct Ctx<'a> {
    session: u64,
    allow_sql: bool,
    views: &'a Mutex<HashSet<u64>>,
}

fn handle(wb: &mut Workbook, op: Op, ctx: &Ctx) -> Result<(Value, Option<Event>)> {
    let origin = ctx.session;
    let changed = |table: String| Some(Event::TableChanged { table, origin });
    Ok(match op {
        Op::Tables => (json!(wb.list_tables()?), None),
        Op::OpenView { spec } => {
            let info = wb.open_view(spec)?;
            ctx.views.lock().unwrap().insert(info.view_id);
            (json!(info), None)
        }
        Op::UpdateView { view_id, spec } => (json!(wb.update_view(view_id, spec)?), None),
        Op::ViewInfo { view_id } => (json!(wb.view_info(view_id)?), None),
        Op::CloseView { view_id } => {
            wb.close_view(view_id);
            ctx.views.lock().unwrap().remove(&view_id);
            (Value::Null, None)
        }
        Op::Fetch { view_id, row_start, row_count } => (json!(wb.fetch(view_id, row_start, row_count)?), None),
        Op::Edit { view_id, edits } => {
            let out = wb.edit(view_id, &edits)?;
            let event = if out.rows_appended > 0 {
                Event::TableChanged { table: out.table.clone(), origin }
            } else {
                Event::CellsChanged { table: out.table.clone(), origin }
            };
            (json!(out), Some(event))
        }
        Op::InsertRows { view_id, at, count } => {
            let table = wb.insert_rows(view_id, at, count)?;
            (Value::Null, changed(table))
        }
        Op::DeleteRows { view_id, start, count } => {
            let table = wb.delete_rows(view_id, start, count)?;
            (Value::Null, changed(table))
        }
        Op::Stats { view_id, column, start, end } => (json!(wb.stats(view_id, &column, start, end)?), None),
        Op::AddColumn { table, name, sql_type } => {
            wb.add_column(&table, &name, &sql_type)?;
            (Value::Null, changed(table))
        }
        Op::AddFormulaColumn { table, name, formula } => {
            wb.add_formula_column(&table, &name, &formula)?;
            (Value::Null, changed(table))
        }
        Op::UpdateFormula { table, name, formula } => {
            wb.update_formula(&table, &name, &formula)?;
            (Value::Null, changed(table))
        }
        Op::DropColumn { table, name } => {
            wb.drop_column(&table, &name)?;
            (Value::Null, changed(table))
        }
        Op::RenameColumn { table, name, new_name } => {
            wb.rename_column(&table, &name, &new_name)?;
            (Value::Null, changed(table))
        }
        Op::Query { sql, limit } => {
            if !ctx.allow_sql {
                bail!("SQL is disabled on this server");
            }
            (json!(wb.preview_query(&sql, limit.min(10_000))?), None)
        }
        Op::CreateTableFromQuery { name, sql } => {
            if !ctx.allow_sql {
                bail!("SQL is disabled on this server");
            }
            (json!(wb.create_table_from_query(&name, &sql)?), Some(Event::TablesChanged { origin }))
        }
        Op::CreateDemoTable { rows } => {
            (json!(wb.create_demo_table(rows.min(1_000_000_000))?), Some(Event::TablesChanged { origin }))
        }
        Op::DropTable { table } => {
            wb.drop_table(&table)?;
            (Value::Null, Some(Event::TablesChanged { origin }))
        }
        Op::RenameTable { table, new_name } => {
            let name = wb.rename_table(&table, &new_name)?;
            (json!(name), Some(Event::TablesChanged { origin }))
        }
        Op::Sheets => (json!(wb.sheets().list()), None),
        Op::SheetCreate { name } => {
            let name = if name.trim().is_empty() { "Sheet".to_string() } else { name };
            (json!(wb.with_sheets(|s| s.create(&name))?), Some(Event::TablesChanged { origin }))
        }
        Op::SheetRename { sheet, name } => {
            wb.with_sheets(|s| s.rename(sheet, &name))?;
            (Value::Null, Some(Event::TablesChanged { origin }))
        }
        Op::SheetDelete { sheet } => {
            wb.with_sheets(|s| s.delete(sheet))?;
            (Value::Null, Some(Event::TablesChanged { origin }))
        }
        Op::SheetFetch { sheet, row_start, row_count, column_count } => {
            (json!(wb.sheets().fetch(sheet, row_start, row_count, column_count)?), None)
        }
        Op::SheetSet { sheet, inputs } => {
            wb.with_sheets(|s| s.set(sheet, &inputs))?;
            (json!(wb.sheets().info(sheet)?), Some(Event::SheetChanged { sheet: Some(sheet), origin }))
        }
        Op::SheetUndo | Op::SheetRedo => {
            let before: Vec<String> = wb.sheets().list().into_iter().map(|s| s.name).collect();
            let changed = wb.with_sheets(|s| if matches!(op, Op::SheetUndo) { s.undo() } else { s.redo() })?;
            let after: Vec<String> = wb.sheets().list().into_iter().map(|s| s.name).collect();
            // Undoing a sheet creation, rename or deletion changes the tab list too.
            let event = if before != after { Event::TablesChanged { origin } } else { Event::SheetChanged { sheet: None, origin } };
            (json!(changed), changed.then_some(event))
        }
        Op::SheetInsertRows { sheet, at, count } => {
            wb.with_sheets(|s| s.insert_rows(sheet, at, count))?;
            (Value::Null, Some(Event::SheetChanged { sheet: Some(sheet), origin }))
        }
        Op::SheetDeleteRows { sheet, at, count } => {
            wb.with_sheets(|s| s.delete_rows(sheet, at, count))?;
            (Value::Null, Some(Event::SheetChanged { sheet: Some(sheet), origin }))
        }
        Op::SheetInsertColumns { sheet, at, count } => {
            wb.with_sheets(|s| s.insert_columns(sheet, at, count))?;
            (Value::Null, Some(Event::SheetChanged { sheet: Some(sheet), origin }))
        }
        Op::SheetDeleteColumns { sheet, at, count } => {
            wb.with_sheets(|s| s.delete_columns(sheet, at, count))?;
            (Value::Null, Some(Event::SheetChanged { sheet: Some(sheet), origin }))
        }
        Op::CreateSheetFromQuery { name, sql } => {
            if !ctx.allow_sql {
                bail!("SQL is disabled on this server");
            }
            (json!(wb.create_sheet_from_query(&name, &sql, 100_000)?), Some(Event::TablesChanged { origin }))
        }
        Op::SheetToTable { sheet, name } => {
            (json!(wb.sheet_to_table(sheet, &name)?), Some(Event::TablesChanged { origin }))
        }
    })
}

pub async fn serve_socket(socket: WebSocket, state: Shared, handle: Arc<WorkbookHandle>) {
    let session = state.next_session_id();
    let _ = handle.touch(true);
    {
        // Keep home-screen counts accurate, including for workbooks made by older versions.
        let h = handle.clone();
        tokio::task::spawn_blocking(move || h.refresh_summary());
    }
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (close_tx, mut close_rx) = mpsc::unbounded_channel::<()>();

    let hello = json!({ "event": "hello", "session": session, "workbook": handle.meta.lock().unwrap().clone() });
    let _ = out_tx.send(hello.to_string());

    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                text = out_rx.recv() => match text {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = close_rx.recv() => {
                    // Flush what is queued (the deletion event), then close.
                    while let Ok(text) = out_rx.try_recv() {
                        let _ = sink.send(Message::Text(text.into())).await;
                    }
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    let mut events = handle.events.subscribe();
    let event_tx = out_tx.clone();
    // Set when the server ends the session itself (workbook deleted); the read loop stops
    // without waiting for the client to acknowledge the close.
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let forwarder = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    let deleted = matches!(ev, Event::WorkbookDeleted { .. });
                    if event_tx.send(serde_json::to_string(&ev).unwrap()).is_err() {
                        break;
                    }
                    if deleted {
                        let _ = close_tx.send(());
                        let _ = stop_tx.send(true);
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let views = Arc::new(Mutex::new(HashSet::new()));
    loop {
        let msg = tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(msg)) => msg,
                _ => break,
            },
            _ = stop_rx.changed() => break,
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let envelope: Envelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                let id = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["id"].as_u64());
                let _ = out_tx.send(json!({ "id": id, "ok": false, "error": format!("bad request: {e}") }).to_string());
                continue;
            }
        };
        let handle = handle.clone();
        let out_tx = out_tx.clone();
        let views = views.clone();
        let allow_sql = state.config.allow_sql;
        tokio::spawn(async move {
            let id = envelope.id;
            let h = handle.clone();
            let result = tokio::task::spawn_blocking(move || {
                let ctx = Ctx { session, allow_sql, views: &views };
                h.with_engine(|wb| handle_logged(wb, envelope.op, &ctx))
            })
            .await;
            let reply = match result {
                Ok(Ok((value, event))) => {
                    if let Some(ev) = event {
                        let structural = ev.changes_structure();
                        let _ = handle.events.send(ev);
                        let h = handle.clone();
                        tokio::task::spawn_blocking(move || {
                            let result = if structural { h.refresh_summary() } else { h.touch(false) };
                            if let Err(e) = result {
                                tracing::warn!("updating workbook metadata: {e:#}");
                            }
                        });
                    }
                    json!({ "id": id, "ok": true, "result": value })
                }
                Ok(Err(e)) => json!({ "id": id, "ok": false, "error": format!("{e:#}") }),
                Err(e) => json!({ "id": id, "ok": false, "error": format!("internal error: {e}") }),
            };
            let _ = out_tx.send(reply.to_string());
        });
    }

    forwarder.abort();
    drop(out_tx);
    let _ = writer.await;
    let leftover: Vec<u64> = views.lock().unwrap().drain().collect();
    if !leftover.is_empty() && !handle.is_deleted() {
        let h = handle.clone();
        let _ = tokio::task::spawn_blocking(move || {
            h.with_engine(|wb| {
                for v in leftover {
                    wb.close_view(v);
                }
                Ok(())
            })
        })
        .await;
    }
}

fn handle_logged(wb: &mut Workbook, op: Op, ctx: &Ctx) -> Result<(Value, Option<Event>)> {
    let label = format!("{op:?}");
    let started = std::time::Instant::now();
    let out = handle(wb, op, ctx);
    let ms = started.elapsed().as_millis();
    if ms > 250 {
        tracing::info!(ms, "slow op {}", label.chars().take(120).collect::<String>());
    }
    out
}
