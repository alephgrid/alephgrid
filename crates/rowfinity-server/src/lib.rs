//! Rowfinity server: HTTP API, WebSocket protocol and workbook registry. The `rowfinity`
//! binary (`main.rs`) is a thin CLI around [`run`]; tests start the same router in-process.

pub mod backup;
pub mod http;
pub mod protocol;
pub mod runtime;
pub mod sched;
pub mod state;
pub mod ws;
