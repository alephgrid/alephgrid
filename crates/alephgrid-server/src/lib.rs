//! Alephgrid server: HTTP API, WebSocket protocol and workbook registry. The `alephgrid`
//! binary (`main.rs`) is a thin CLI around [`run`]; tests start the same router in-process.

pub mod auth;
pub mod backup;
pub mod health;
pub mod http;
pub mod library;
pub mod mcp;
pub mod protocol;
pub mod python;
pub mod quota;
pub mod reserve;
pub mod runtime;
pub mod sched;
pub mod state;
pub mod ws;
