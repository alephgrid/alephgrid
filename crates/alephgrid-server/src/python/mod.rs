//! The Python runtime and its isolated workers (phase 8, docs/PYTHON_FUNCTIONS.md).
//!
//! * A **runtime** is a directory with `runtime.json` (made by [`create::create`]): the
//!   interpreter to run, the paths it must read, its packages and their hashes. Its id is a
//!   hash of that manifest; function versions record it.
//! * A **worker** is one process per batch, started under an OS sandbox ([`Isolation`]) with
//!   a cleared environment in a private, empty directory: it reads only the runtime and that
//!   directory, writes nowhere, has no network and cannot start processes. Wall time, CPU,
//!   memory and output are limited; a watchdog kills it when one is passed or the job is
//!   cancelled.
//! * At most `workers` run at once on the server ([`Slots`]).

pub mod create;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use alephgrid_core::external::{CallResult, FunctionRuntime, RuntimeIdentity, WorkerFailure, WorkerFailureKind, WorkerJob};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST: &str = "runtime.json";
const RUNNER: &str = include_str!("runner.py");

/// `runtime.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// `3.12.9`.
    pub python: String,
    pub implementation: String,
    pub platform: String,
    /// The interpreter binary, run directly (no launcher that would start another process).
    pub executable: PathBuf,
    /// Everything the interpreter reads: its prefix, libraries it links, the packages.
    pub read_paths: Vec<PathBuf>,
    /// Added to `sys.path` (the installed packages).
    pub site_packages: Vec<PathBuf>,
    pub packages: Vec<Package>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// SHA-256 of the wheel it was installed from.
    pub sha256: String,
}

impl Manifest {
    /// `py-` and 16 hex digits of the SHA-256 of the canonical manifest.
    pub fn id(&self) -> String {
        let digest = Sha256::digest(serde_json::to_vec(self).expect("serializable"));
        format!("py-{}", crate::runtime::hex(&digest[..8]))
    }

    pub fn load(dir: &Path) -> Result<Manifest> {
        let path = dir.join(MANIFEST);
        if !path.exists() {
            bail!("{} is not a Python runtime (no {MANIFEST}); make one with `alephgrid python-runtime create`", dir.display());
        }
        let m: Manifest = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?)
            .with_context(|| format!("{} is not a runtime manifest", path.display()))?;
        if m.format != 1 {
            bail!("{} has format {}; this server reads format 1", path.display(), m.format);
        }
        Ok(m)
    }
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    crate::runtime::write_durably(&dir.join(MANIFEST), &serde_json::to_vec_pretty(m)?)
}

/// How workers are kept from the rest of the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Isolation {
    /// macOS Seatbelt (`sandbox-exec`) with a deny-by-default profile.
    Seatbelt,
    /// Linux bubblewrap: new namespaces (no network), read-only binds, a private /tmp.
    Bubblewrap(PathBuf),
    /// No OS sandbox: allowed only when asked for (`--python-isolation none`, local mode).
    None,
}

impl Isolation {
    /// The sandbox this host offers, if any.
    pub fn detect() -> Option<Isolation> {
        if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists() {
            return Some(Isolation::Seatbelt);
        }
        if cfg!(target_os = "linux") {
            for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
                let p = Path::new(dir).join("bwrap");
                if p.exists() {
                    return Some(Isolation::Bubblewrap(p));
                }
            }
        }
        None
    }

    pub fn name(&self) -> &'static str {
        match self {
            Isolation::Seatbelt => "seatbelt",
            Isolation::Bubblewrap(_) => "bubblewrap",
            Isolation::None => "none",
        }
    }
}

/// Limits of one worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct Limits {
    pub timeout_ms: u64,
    pub cpu_ms: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    /// Workers running at once on the server.
    pub workers: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { timeout_ms: 30_000, cpu_ms: 30_000, memory_bytes: 512 << 20, output_bytes: 16 << 20, workers: 2 }
    }
}

/// Worker places, taken in turn.
struct Slots {
    free: Mutex<u64>,
    cv: Condvar,
}

impl Slots {
    fn acquire(&self, cancel: &AtomicBool) -> std::result::Result<SlotGuard<'_>, WorkerFailure> {
        let mut free = self.free.lock().unwrap();
        while *free == 0 {
            if cancel.load(Ordering::SeqCst) {
                return Err(WorkerFailure { kind: WorkerFailureKind::Cancelled, message: "cancelled".into() });
            }
            free = self.cv.wait_timeout(free, Duration::from_millis(50)).unwrap().0;
        }
        *free -= 1;
        Ok(SlotGuard(self))
    }
}

struct SlotGuard<'a>(&'a Slots);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap() += 1;
        self.0.cv.notify_one();
    }
}

pub struct PythonRuntime {
    pub dir: PathBuf,
    pub manifest: Manifest,
    id: String,
    pub isolation: Isolation,
    pub limits: Limits,
    /// Where workers' private directories are made.
    scratch: PathBuf,
    slots: Slots,
    /// Workers started (for tests and status).
    pub started: AtomicU64,
}

fn failure(kind: WorkerFailureKind, message: impl Into<String>) -> WorkerFailure {
    WorkerFailure { kind, message: message.into() }
}

impl PythonRuntime {
    /// Load the runtime in `dir`, with `isolation` (None: the host's sandbox), and check
    /// that a worker starts and reports the manifest's Python version.
    pub fn load(dir: &Path, isolation: Option<Isolation>, limits: Limits, scratch: &Path) -> Result<PythonRuntime> {
        let manifest = Manifest::load(dir)?;
        let isolation = match isolation {
            Some(i) => i,
            None => Isolation::detect().ok_or_else(|| {
                anyhow::anyhow!("no sandbox is available for Python workers (sandbox-exec on macOS, bwrap on Linux); install one, or turn Python functions off")
            })?,
        };
        std::fs::create_dir_all(scratch)?;
        let rt = PythonRuntime {
            dir: dir.to_path_buf(),
            id: manifest.id(),
            manifest,
            isolation,
            limits,
            scratch: scratch.to_path_buf(),
            slots: Slots { free: Mutex::new(limits.workers.max(1)), cv: Condvar::new() },
            started: AtomicU64::new(0),
        };
        rt.self_test()?;
        Ok(rt)
    }

    fn self_test(&self) -> Result<()> {
        let job = WorkerJob {
            function: "VERSION".into(),
            version: 0,
            source: "import sys\ndef version(x):\n    return '%d.%d.%d' % sys.version_info[:3]\n".into(),
            entry: "version".into(),
            arguments: vec!["x".into()],
            returns: alephgrid_core::external::ResultType::Text,
            context: alephgrid_core::external::CallContext::Sheet,
            calls: vec![vec![alephgrid_core::external::WorkerValue::Null]],
        };
        let out = self.run(&job, &AtomicBool::new(false)).map_err(|e| anyhow::anyhow!("the Python runtime in {} does not start: {e}", self.dir.display()))?;
        match out.first() {
            Some(CallResult::Text(v)) if *v == self.manifest.python => Ok(()),
            other => bail!("the Python runtime in {} reports {other:?}, its manifest says Python {}", self.dir.display(), self.manifest.python),
        }
    }

    /// The command that starts a worker in `work` under the sandbox.
    fn command(&self, work: &Path) -> Result<Command> {
        let exe = &self.manifest.executable;
        let script = work.join("runner.py");
        let python_args = ["-I", "-S", "-B", "-u"];
        let mut cmd = match &self.isolation {
            Isolation::Seatbelt => {
                let profile = work.join("sandbox.sb");
                std::fs::write(&profile, seatbelt_profile(&self.manifest, work))?;
                let mut c = Command::new("/usr/bin/sandbox-exec");
                c.arg("-f").arg(&profile).arg(exe).args(python_args).arg(&script);
                c
            }
            Isolation::Bubblewrap(bwrap) => {
                let mut c = Command::new(bwrap);
                c.args(["--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv"]);
                for dir in ["/usr", "/lib", "/lib64", "/bin", "/etc/ld.so.cache"] {
                    if Path::new(dir).exists() {
                        c.arg("--ro-bind").arg(dir).arg(dir);
                    }
                }
                for p in self.manifest.read_paths.iter().chain(std::iter::once(exe)) {
                    c.arg("--ro-bind").arg(p).arg(p);
                }
                c.arg("--ro-bind").arg(work).arg("/work");
                c.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--chdir", "/work"]);
                c.arg(exe).args(python_args).arg("/work/runner.py");
                c
            }
            Isolation::None => {
                let mut c = Command::new(exe);
                c.args(python_args).arg(&script);
                c
            }
        };
        cmd.env_clear().current_dir(work).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let limits = self.limits;
        // SAFETY: only async-signal-safe calls (setrlimit) between fork and exec.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                let set = |resource, value: u64| {
                    let lim = libc::rlimit { rlim_cur: value as libc::rlim_t, rlim_max: value as libc::rlim_t };
                    libc::setrlimit(resource, &lim);
                };
                set(libc::RLIMIT_CPU, limits.cpu_ms.div_ceil(1000).max(1));
                set(libc::RLIMIT_CORE, 0);
                set(libc::RLIMIT_NOFILE, 256);
                // Enforced by Linux; macOS ignores it, and the watchdog samples instead.
                #[cfg(target_os = "linux")]
                set(libc::RLIMIT_AS, limits.memory_bytes.saturating_add(256 << 20));
                Ok(())
            });
        }
        Ok(cmd)
    }

    /// Run one worker on `input`; its stdout, within the limits.
    fn run_worker(&self, input: Vec<u8>, cancel: &AtomicBool) -> std::result::Result<Vec<u8>, WorkerFailure> {
        let _slot = self.slots.acquire(cancel)?;
        let work = private_dir(&self.scratch).map_err(|e| failure(WorkerFailureKind::Unavailable, format!("{e:#}")))?;
        let _cleanup = RemoveOnDrop(work.clone());
        std::fs::write(work.join("runner.py"), RUNNER).map_err(|e| failure(WorkerFailureKind::Unavailable, e.to_string()))?;
        let mut cmd = self.command(&work).map_err(|e| failure(WorkerFailureKind::Unavailable, format!("{e:#}")))?;
        let mut child = cmd.spawn().map_err(|e| failure(WorkerFailureKind::Unavailable, format!("could not start a Python worker: {e}")))?;
        self.started.fetch_add(1, Ordering::Relaxed);
        let pid = child.id() as i32;
        let mut stdin = child.stdin.take().expect("piped");
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let max = self.limits.output_bytes;
        let over = Arc::new(AtomicBool::new(false));
        let mut stdout = child.stdout.take().expect("piped");
        let over2 = over.clone();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.len() as u64 + n as u64 > max {
                            over2.store(true, Ordering::SeqCst);
                            break;
                        }
                        out.extend_from_slice(&buf[..n]);
                    }
                }
            }
            out
        });
        let mut stderr = child.stderr.take().expect("piped");
        let errors = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = (&mut stderr).take(64 << 10).read_to_end(&mut out);
            // Drain the rest so the worker never blocks on a full pipe.
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            out
        });
        let started = Instant::now();
        let timeout = Duration::from_millis(self.limits.timeout_ms);
        let mut killed: Option<WorkerFailure> = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {}
                Err(_) => break None,
            }
            let reason = if cancel.load(Ordering::SeqCst) {
                Some(failure(WorkerFailureKind::Cancelled, "cancelled"))
            } else if started.elapsed() > timeout {
                Some(failure(WorkerFailureKind::Timeout, format!("the function ran for more than {} s", timeout.as_secs_f64())))
            } else if over.load(Ordering::SeqCst) {
                Some(failure(WorkerFailureKind::Output, format!("the function wrote more than {} bytes of results", max)))
            } else if resident(pid).is_some_and(|r| r > self.limits.memory_bytes) {
                Some(failure(WorkerFailureKind::Memory, format!("the function used more than {} MiB of memory", self.limits.memory_bytes >> 20)))
            } else {
                None
            };
            if let Some(r) = reason {
                let _ = child.kill();
                killed = Some(r);
                break child.wait().ok();
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let _ = writer.join();
        let out = reader.join().unwrap_or_default();
        let err = errors.join().unwrap_or_default();
        if let Some(k) = killed {
            return Err(k);
        }
        if over.load(Ordering::SeqCst) {
            return Err(failure(WorkerFailureKind::Output, format!("the function wrote more than {max} bytes of results")));
        }
        match status {
            Some(s) if s.success() => Ok(out),
            Some(s) => {
                use std::os::unix::process::ExitStatusExt;
                if s.signal() == Some(libc::SIGXCPU) {
                    return Err(failure(WorkerFailureKind::Timeout, format!("the function used more than {} s of CPU", self.limits.cpu_ms as f64 / 1000.0)));
                }
                let text = String::from_utf8_lossy(&err);
                let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                Err(failure(WorkerFailureKind::Crashed, format!("the Python worker stopped ({s}){}", if last.is_empty() { String::new() } else { format!(": {last}") })))
            }
            None => Err(failure(WorkerFailureKind::Crashed, "the Python worker could not be waited for")),
        }
    }
}

impl FunctionRuntime for PythonRuntime {
    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity {
            kind: "python".into(),
            id: self.id.clone(),
            version: self.manifest.python.clone(),
            packages: self.manifest.packages.iter().map(|p| format!("{}=={}", p.name, p.version)).collect(),
        }
    }

    fn run(&self, job: &WorkerJob, cancel: &AtomicBool) -> std::result::Result<Vec<CallResult>, WorkerFailure> {
        if job.calls.is_empty() {
            return Ok(vec![]);
        }
        let site: Vec<String> = self.manifest.site_packages.iter().map(|p| p.to_string_lossy().to_string()).collect();
        let input = serde_json::to_vec(&serde_json::json!({
            "source": job.source,
            "entry": job.entry,
            "returns": job.returns,
            "context": job.context,
            "site": site,
            "calls": job.calls,
        }))
        .map_err(|e| failure(WorkerFailureKind::Crashed, e.to_string()))?;
        let out = self.run_worker(input, cancel)?;
        #[derive(Deserialize)]
        struct Reply {
            results: Vec<CallResult>,
        }
        let line = out.split(|b| *b == b'\n').find(|l| !l.is_empty()).unwrap_or(&[]);
        let reply: Reply = serde_json::from_slice(line).map_err(|e| failure(WorkerFailureKind::Crashed, format!("the Python worker answered something else: {e}")))?;
        if reply.results.len() != job.calls.len() {
            return Err(failure(WorkerFailureKind::Crashed, format!("the Python worker answered {} results for {} calls", reply.results.len(), job.calls.len())));
        }
        Ok(reply.results)
    }
}

/// A new directory only this process can enter.
fn private_dir(root: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = root.join(format!("w{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::DirBuilder::new().mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(std::fs::canonicalize(&dir)?)
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Resident memory of `pid` (and, on Linux, the processes under it).
#[cfg(target_os = "macos")]
fn resident(pid: i32) -> Option<u64> {
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, &mut info as *mut _ as *mut libc::c_void, size);
        (n == size).then_some(info.pti_resident_size)
    }
}

#[cfg(target_os = "linux")]
fn resident(pid: i32) -> Option<u64> {
    fn one(pid: i32) -> u64 {
        let pages: u64 = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok().and_then(|s| s.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0);
        // SAFETY: sysconf has no preconditions.
        pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64
    }
    fn tree(pid: i32, depth: u32) -> u64 {
        let mut total = one(pid);
        if depth < 4 {
            if let Ok(children) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
                for c in children.split_whitespace().filter_map(|c| c.parse().ok()) {
                    total += tree(c, depth + 1);
                }
            }
        }
        total
    }
    Some(tree(pid, 0))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident(_pid: i32) -> Option<u64> {
    None
}

/// The deny-by-default Seatbelt profile of a worker: read the runtime and `work`, write
/// nothing, no network, run only the interpreter.
fn seatbelt_profile(m: &Manifest, work: &Path) -> String {
    let q = |p: &Path| format!("\"{}\"", p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""));
    let mut reads: Vec<PathBuf> = m.read_paths.clone();
    reads.push(work.to_path_buf());
    for system in ["/System/Library", "/usr/lib", "/private/var/db/dyld"] {
        reads.push(PathBuf::from(system));
    }
    // Directories above what it reads may be looked up (stat), not read.
    let mut ancestors = std::collections::BTreeSet::new();
    for p in reads.iter().chain(std::iter::once(&m.executable)) {
        let mut cur = p.parent();
        while let Some(a) = cur {
            ancestors.insert(a.to_path_buf());
            cur = a.parent();
        }
    }
    let read_rules: Vec<String> = reads.iter().map(|p| format!("(subpath {})", q(p))).collect();
    let meta_rules: Vec<String> = ancestors.iter().map(|p| format!("(literal {})", q(p))).collect();
    format!(
        "(version 1)
(deny default)
(allow sysctl-read)
(allow process-exec (literal {exe}))
(allow file-read* (literal \"/\") (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/random\") {reads})
(allow file-read-metadata {meta})
(allow file-write* (literal \"/dev/null\"))
",
        exe = q(&m.executable),
        reads = read_rules.join(" "),
        meta = meta_rules.join(" "),
    )
}
