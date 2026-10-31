//! `alephgrid` — run the big-data spreadsheet on your laptop or your own server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use alephgrid_server::http;
use alephgrid_server::auth::oidc::OidcConfig;
use alephgrid_server::auth::{AuthConfig, AuthMode};
use alephgrid_server::state::{AppState, Config};

#[derive(Parser, Debug)]
#[command(name = "alephgrid", version, about = "Open-source spreadsheet for billion-row data. Runs locally or self-hosted.")]
struct Cli {
    /// Files to open (CSV, TSV, Parquet, JSON/JSONL, XLSX). XLSX files become editable sheets. They are imported into a new workbook.
    files: Vec<PathBuf>,

    /// Where workbooks are stored.
    #[arg(long, env = "ALEPHGRID_DATA_DIR", default_value = "./alephgrid-data")]
    data_dir: PathBuf,

    /// Interface to bind. Use 0.0.0.0 to serve your team (with --auth accounts).
    #[arg(long, env = "ALEPHGRID_HOST", default_value = "127.0.0.1")]
    host: String,

    #[arg(long, env = "ALEPHGRID_PORT", default_value_t = 7878)]
    port: u16,

    /// `local`: no sign-in, for your own machine (loopback only). `accounts`: users sign in
    /// with a password or single sign-on and roles apply. On first start with accounts and
    /// no users, a one-time setup code is printed (or set ALEPHGRID_BOOTSTRAP_EMAIL and
    /// ALEPHGRID_BOOTSTRAP_PASSWORD to create the admin).
    #[arg(long, env = "ALEPHGRID_AUTH", value_enum, default_value = "local")]
    auth: alephgrid_server::auth::AuthMode,

    /// Address people use to reach this server, e.g. https://grid.example.com. Required
    /// for single sign-on; with https, session cookies are marked Secure. Requests for
    /// other host names are refused.
    #[arg(long, env = "ALEPHGRID_PUBLIC_URL")]
    public_url: Option<String>,

    /// Hours a sign-in lasts.
    #[arg(long, env = "ALEPHGRID_SESSION_HOURS", default_value_t = 12)]
    session_hours: u64,

    /// OpenID Connect issuer URL for single sign-on (https).
    #[arg(long, env = "ALEPHGRID_OIDC_ISSUER", requires = "oidc_client_id")]
    oidc_issuer: Option<String>,

    #[arg(long, env = "ALEPHGRID_OIDC_CLIENT_ID")]
    oidc_client_id: Option<String>,

    /// Client secret (prefer the environment variable; omit for a public client).
    #[arg(long, env = "ALEPHGRID_OIDC_CLIENT_SECRET", hide_env_values = true)]
    oidc_client_secret: Option<String>,

    #[arg(long, env = "ALEPHGRID_OIDC_SCOPES", default_value = "openid email profile")]
    oidc_scopes: String,

    /// ID-token claim with the user's groups, for granting space roles to groups.
    #[arg(long, env = "ALEPHGRID_OIDC_GROUPS_CLAIM", default_value = "groups")]
    oidc_groups_claim: String,

    /// Text of the single sign-on button.
    #[arg(long, env = "ALEPHGRID_OIDC_LABEL", default_value = "Sign in with SSO")]
    oidc_label: String,

    /// Who may join through single sign-on: `invite` (people with an invitation) or `any`
    /// (anyone the identity provider signs in joins the default organization, with no
    /// space roles until granted).
    #[arg(long, env = "ALEPHGRID_OIDC_JOIN", value_enum, default_value = "invite")]
    oidc_join: alephgrid_server::auth::oidc::OidcJoin,

    /// Serve a network interface without sign-in. Anyone who can reach the port can read,
    /// change and delete every workbook.
    #[arg(long, env = "ALEPHGRID_UNSAFE_NO_AUTH")]
    unsafe_no_auth: bool,

    /// Create a demo workbook with this many synthetic flight rows.
    #[arg(long)]
    demo_rows: Option<u64>,

    /// Memory shared by all open workbooks' DuckDB instances, e.g. "8GB". Defaults to 80%
    /// of RAM (or of the container's memory limit). Bounds DuckDB's buffer managers, not
    /// the process's total memory.
    #[arg(long, alias = "memory-limit", env = "ALEPHGRID_MEMORY_BUDGET", value_parser = parse_size)]
    memory_budget: Option<u64>,

    /// DuckDB worker threads shared by all open workbooks. Defaults to all cores.
    #[arg(long, env = "ALEPHGRID_THREADS")]
    threads: Option<usize>,

    /// Temporary disk for DuckDB spill files and staged uploads (half each), e.g. "100GB".
    #[arg(long, env = "ALEPHGRID_TEMP_BUDGET", value_parser = parse_size, default_value = "64GiB")]
    temp_budget: u64,

    /// Smallest memory share one open workbook may get; below it, idle workbooks are closed
    /// or new ones refused.
    #[arg(long, env = "ALEPHGRID_MIN_WORKBOOK_MEMORY", value_parser = parse_size, default_value = "256MiB")]
    min_workbook_memory: u64,

    /// Seconds without sessions or work before a workbook is closed (its data stays).
    #[arg(long, env = "ALEPHGRID_IDLE_TIMEOUT", default_value_t = 600)]
    idle_timeout: u64,

    /// Seconds before an unused sorted or filtered view's materialization is dropped.
    #[arg(long, env = "ALEPHGRID_VIEW_TTL", default_value_t = 900)]
    view_ttl: u64,

    /// Largest file upload, e.g. "100GB".
    #[arg(long, env = "ALEPHGRID_MAX_UPLOAD", value_parser = parse_size, default_value = "100GiB")]
    max_upload: u64,

    /// Largest WebSocket message a client may send (edits, pastes).
    #[arg(long, env = "ALEPHGRID_MAX_MESSAGE", value_parser = parse_size, default_value = "16MiB")]
    max_message: u64,

    /// Seconds an interactive request (fetch, edit) may take, waiting included.
    #[arg(long, env = "ALEPHGRID_REQUEST_TIMEOUT", default_value_t = 30)]
    request_timeout: u64,

    /// Seconds a bulk job (import, query, sort, copy, export) may take.
    #[arg(long, env = "ALEPHGRID_JOB_TIMEOUT", default_value_t = 3600)]
    job_timeout: u64,

    /// Seconds running work may take to finish on shutdown before it is interrupted.
    #[arg(long, env = "ALEPHGRID_SHUTDOWN_GRACE", default_value_t = 30)]
    shutdown_grace: u64,

    /// Disable the ad-hoc SQL console (recommended for shared servers you don't fully trust users on).
    #[arg(long, env = "ALEPHGRID_DISABLE_SQL")]
    disable_sql: bool,

    /// Most rows copied from a table or query into a sheet.
    #[arg(long, env = "ALEPHGRID_SHEET_COPY_ROWS", default_value_t = alephgrid_server::ws::SHEET_COPY_ROWS)]
    sheet_copy_rows: usize,

    /// Back up every workbook in --data-dir into this new directory, then exit. Refuses
    /// while a server uses the data directory (use POST /api/admin/backups then).
    #[arg(long, value_name = "DIR")]
    backup_to: Option<PathBuf>,

    /// Restore the backup in this directory into --data-dir (which must be empty), then exit.
    #[arg(long, value_name = "DIR", conflicts_with = "backup_to")]
    restore_from: Option<PathBuf>,

    /// Serve the web UI from this directory instead of the embedded build.
    #[arg(long, env = "ALEPHGRID_WEB_DIR")]
    web_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "alephgrid=info,tower_http=warn".into()))
        .init();
    let cli = Cli::parse();

    if let Some(dest) = &cli.backup_to {
        let m = alephgrid_server::backup::backup_offline(&cli.data_dir, dest)?;
        println!("backed up {} workbook(s) to {}", m.workbooks.len(), dest.display());
        return Ok(());
    }
    if let Some(src) = &cli.restore_from {
        let m = alephgrid_server::backup::restore(src, &cli.data_dir)?;
        println!("restored {} workbook(s) into {}", m.workbooks.len(), cli.data_dir.display());
        return Ok(());
    }

    let loopback = matches!(cli.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if !loopback && cli.auth == AuthMode::Local {
        if !cli.unsafe_no_auth {
            anyhow::bail!(
                "serving {} without sign-in would let anyone who can reach it read and change every workbook; use --auth accounts (or --unsafe-no-auth if you really mean it)",
                cli.host
            );
        }
        tracing::warn!("serving on {} without sign-in: anyone who can reach this port can read, change and delete your data", cli.host);
    }
    let public_url = cli.public_url.as_deref().map(|u| u.trim_end_matches('/').to_string());
    if let Some(u) = &public_url {
        if !(u.starts_with("https://") || u.starts_with("http://")) || u.matches('/').count() > 2 {
            anyhow::bail!("--public-url must be a scheme and host, like https://grid.example.com (got {u})");
        }
    }
    let oidc = match (&cli.oidc_issuer, &cli.oidc_client_id) {
        (Some(issuer), Some(client_id)) => {
            let Some(public) = &public_url else { anyhow::bail!("single sign-on needs --public-url, to tell the identity provider where to send people back") };
            if cli.auth != AuthMode::Accounts {
                anyhow::bail!("single sign-on needs --auth accounts");
            }
            Some(OidcConfig {
                issuer: issuer.clone(),
                client_id: client_id.clone(),
                client_secret: cli.oidc_client_secret.clone().filter(|s| !s.is_empty()),
                scopes: cli.oidc_scopes.clone(),
                groups_claim: cli.oidc_groups_claim.clone(),
                label: cli.oidc_label.clone(),
                join: cli.oidc_join,
                redirect_uri: format!("{public}/api/auth/oidc/callback"),
            })
        }
        _ => None,
    };
    let bootstrap = match (std::env::var("ALEPHGRID_BOOTSTRAP_EMAIL"), std::env::var("ALEPHGRID_BOOTSTRAP_PASSWORD")) {
        (Ok(e), Ok(p)) if !e.is_empty() && !p.is_empty() => Some((e, p)),
        _ => None,
    };
    let allowed_hosts = match &public_url {
        Some(u) => {
            let host = u.split_once("://").map(|(_, h)| h).unwrap_or(u);
            let name = host.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host);
            let mut hosts = alephgrid_server::auth::loopback_hosts();
            hosts.push(name.to_ascii_lowercase());
            Some(hosts)
        }
        None if loopback => Some(alephgrid_server::auth::loopback_hosts()),
        None => None,
    };

    let mut config = Config::new(cli.data_dir.clone());
    config.sheet_copy_rows = cli.sheet_copy_rows.max(1);
    config.auth = AuthConfig {
        mode: cli.auth,
        public_url: public_url.clone(),
        session_ttl: Duration::from_secs(cli.session_hours.clamp(1, 24 * 30) * 3600),
        oidc,
        bootstrap,
        allowed_hosts,
    };
    config.allow_sql = !cli.disable_sql;
    config.memory_budget = cli.memory_budget;
    config.threads = cli.threads;
    config.temp_budget = Some(cli.temp_budget);
    config.min_workbook_memory = cli.min_workbook_memory;
    config.idle_timeout = Duration::from_secs(cli.idle_timeout);
    config.view_ttl = Duration::from_secs(cli.view_ttl);
    config.shutdown_grace = Duration::from_secs(cli.shutdown_grace);
    config.max_upload_bytes = cli.max_upload;
    config.max_message_bytes = cli.max_message as usize;
    config.request_timeout = Duration::from_secs(cli.request_timeout.max(1));
    config.job_timeout = Duration::from_secs(cli.job_timeout.max(1));
    let state = AppState::new(config)?;
    let janitor = alephgrid_server::state::start_janitor(&state);
    let default_space = state.auth.store.setting("default_space")?.context("the default space is missing")?;

    let mut open_path = String::new();
    if !cli.files.is_empty() {
        let first = cli.files[0].file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let h = state.create(&first, &default_space)?;
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
        let h = state.create("Demo: US flights", &default_space)?;
        let started = std::time::Instant::now();
        let info = h.with_engine(|wb| wb.create_demo_table(rows))?;
        tracing::info!("generated {} demo rows in {:.1?}", info.row_count, started.elapsed());
        h.refresh_summary()?;
        open_path = format!("w/{}", h.meta.lock().unwrap().id);
    }

    let app = http::router(state.clone(), cli.web_dir.clone());
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse().context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    let bound_port = listener.local_addr()?.port();
    let shown_host = if cli.host == "0.0.0.0" { "localhost" } else { cli.host.as_str() };
    let base = public_url.clone().unwrap_or_else(|| format!("http://{shown_host}:{bound_port}"));
    println!("\n  Alephgrid is running → {base}/{open_path}\n  Data: {}\n", cli.data_dir.display());
    if let Some(code) = state.auth.setup_code() {
        // Shown only on the console of whoever started the server; it creates the admin once.
        println!("  First start: open {base}/setup and enter the setup code {code}\n");
    }

    let stopping = state.clone();
    let (begun_tx, begun_rx) = tokio::sync::oneshot::channel::<()>();
    let grace = Duration::from_secs(cli.shutdown_grace) + Duration::from_secs(10);
    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Stop admitting work and drain every workbook now: open WebSocket sessions end
            // when their workbook closes, which lets the server finish.
            stopping.begin_shutdown();
            let s = stopping.clone();
            tokio::task::spawn_blocking(move || s.checkpoint_all());
            tokio::spawn(async {
                shutdown_signal().await;
                tracing::warn!("second signal: exiting without waiting for running work (committed changes are safe)");
                std::process::exit(130);
            });
            let _ = begun_tx.send(());
        });
    // Connections that never close (a client holding its socket) must not keep the
    // process alive: once shutdown began, wait at most the grace period plus 10 s.
    tokio::select! {
        r = serve => r?,
        _ = async { let _ = begun_rx.await; tokio::time::sleep(grace).await } => {
            tracing::warn!("connections still open after the grace period; stopping anyway");
        }
    }
    janitor.abort();
    let s = state.clone();
    tokio::task::spawn_blocking(move || s.checkpoint_all()).await?;
    tracing::info!("stopped cleanly");
    Ok(())
}

fn parse_size(s: &str) -> Result<u64, String> {
    alephgrid_server::runtime::budget::parse_bytes(s)
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
