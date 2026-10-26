//! `rowfinity` — run the big-data spreadsheet on your laptop or your own server.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use rowfinity_server::http;
use rowfinity_server::state::{AppState, Config};

#[derive(Parser, Debug)]
#[command(name = "rowfinity", version, about = "Open-source spreadsheet for billion-row data. Runs locally or self-hosted.")]
struct Cli {
    /// Files to open (CSV, TSV, Parquet, JSON/JSONL, XLSX). XLSX files become editable sheets. They are imported into a new workbook.
    files: Vec<PathBuf>,

    /// Where workbooks are stored.
    #[arg(long, env = "ROWFINITY_DATA_DIR", default_value = "./rowfinity-data")]
    data_dir: PathBuf,

    /// Interface to bind. Use 0.0.0.0 to serve your team (set --token too).
    #[arg(long, env = "ROWFINITY_HOST", default_value = "127.0.0.1")]
    host: String,

    #[arg(long, env = "ROWFINITY_PORT", default_value_t = 7878)]
    port: u16,

    /// Shared access token required by the API and WebSocket.
    #[arg(long, env = "ROWFINITY_TOKEN")]
    token: Option<String>,

    /// Create a demo workbook with this many synthetic flight rows.
    #[arg(long)]
    demo_rows: Option<u64>,

    /// DuckDB memory limit, e.g. "8GB". Defaults to 80% of RAM.
    #[arg(long, env = "ROWFINITY_MEMORY_LIMIT")]
    memory_limit: Option<String>,

    /// DuckDB worker threads. Defaults to all cores.
    #[arg(long, env = "ROWFINITY_THREADS")]
    threads: Option<usize>,

    /// Disable the ad-hoc SQL console (recommended for shared servers you don't fully trust users on).
    #[arg(long, env = "ROWFINITY_DISABLE_SQL")]
    disable_sql: bool,

    /// Serve the web UI from this directory instead of the embedded build.
    #[arg(long, env = "ROWFINITY_WEB_DIR")]
    web_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "rowfinity=info,tower_http=warn".into()))
        .init();
    let cli = Cli::parse();

    if cli.host != "127.0.0.1" && cli.host != "localhost" && cli.token.is_none() {
        tracing::warn!("serving on {} without --token: anyone who can reach this port can read and edit your data", cli.host);
    }

    let state = AppState::new(Config {
        data_dir: cli.data_dir.clone(),
        token: cli.token.clone(),
        allow_sql: !cli.disable_sql,
        memory_limit: cli.memory_limit.clone(),
        threads: cli.threads,
    })?;

    let mut open_path = String::new();
    if !cli.files.is_empty() {
        let first = cli.files[0].file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let h = state.create(&first, None)?;
        for file in &cli.files {
            let path = file.canonicalize().with_context(|| format!("cannot open {}", file.display()))?;
            let started = std::time::Instant::now();
            let imported = h.with_engine(|wb| wb.import_any(&path, None))?;
            tracing::info!("imported {} in {:.1?}: {}", file.display(), started.elapsed(), serde_json::to_string(&imported)?);
        }
        h.refresh_summary()?;
        open_path = format!("w/{}", h.meta.lock().unwrap().id);
    }
    if let Some(rows) = cli.demo_rows {
        let h = state.create("Demo: US flights", None)?;
        let started = std::time::Instant::now();
        let info = h.with_engine(|wb| wb.create_demo_table(rows))?;
        tracing::info!("generated {} demo rows in {:.1?}", info.row_count, started.elapsed());
        h.refresh_summary()?;
        open_path = format!("w/{}", h.meta.lock().unwrap().id);
    }

    let app = http::router(state.clone(), cli.web_dir.clone());
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse().context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    let shown_host = if cli.host == "0.0.0.0" { "localhost" } else { cli.host.as_str() };
    let token = cli.token.as_ref().map(|t| format!("?token={t}")).unwrap_or_default();
    println!("\n  Rowfinity is running → http://{shown_host}:{}/{open_path}{token}\n  Data: {}\n", cli.port, cli.data_dir.display());

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    state.checkpoint_all();
    Ok(())
}

/// Ctrl-C, or SIGTERM from `docker stop` / systemd.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}
