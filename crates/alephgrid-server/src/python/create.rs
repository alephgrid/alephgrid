//! `alephgrid python-runtime create`: a pinned Python runtime, made offline (8.05).
//!
//! The runtime is the given interpreter plus packages installed from a local folder of
//! wheels with `pip --no-index --require-hashes` into the runtime's own `site-packages`.
//! `runtime.json` records the interpreter, every path it reads, and each package with its
//! version and wheel hash; the runtime's id is a hash of that. Nothing is downloaded.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::{Manifest, Package};

pub struct Options {
    /// The interpreter to build on.
    pub python: PathBuf,
    /// A folder of wheels to install from (offline).
    pub wheels: Option<PathBuf>,
    /// Requirements with hashes (`name==version --hash=sha256:…`).
    pub requirements: Option<PathBuf>,
    /// The runtime directory to create (must not exist, or be empty).
    pub dir: PathBuf,
}

#[derive(Deserialize)]
struct Probe {
    version: String,
    implementation: String,
    platform: String,
    executable: String,
    base_prefix: String,
    dynload: Option<String>,
}

const PROBE: &str = r#"
import json, os, sys, sysconfig
print(json.dumps({
    "version": "%d.%d.%d" % sys.version_info[:3],
    "implementation": sys.implementation.name,
    "platform": sysconfig.get_platform(),
    "executable": os.path.realpath(sys.executable),
    "base_prefix": os.path.realpath(sys.base_prefix),
    "dynload": sysconfig.get_path("platstdlib") and os.path.join(sysconfig.get_path("platstdlib"), "lib-dynload"),
}))
"#;

fn clean_env(cmd: &mut Command) -> &mut Command {
    // No user configuration, index or proxy can reach pip.
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PIP_NO_INDEX", "1")
        .env("PIP_CONFIG_FILE", "/dev/null")
        .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
        .env("PYTHONNOUSERSITE", "1")
}

pub fn create(opts: &Options) -> Result<Manifest> {
    if opts.dir.exists() && std::fs::read_dir(&opts.dir)?.next().is_some() {
        bail!("{} is not empty; choose a new directory for the runtime", opts.dir.display());
    }
    std::fs::create_dir_all(&opts.dir)?;
    let dir = std::fs::canonicalize(&opts.dir)?;
    let out = clean_env(&mut Command::new(&opts.python)).args(["-I", "-c", PROBE]).output().with_context(|| format!("running {}", opts.python.display()))?;
    if !out.status.success() {
        bail!("{} does not run: {}", opts.python.display(), String::from_utf8_lossy(&out.stderr));
    }
    let probe: Probe = serde_json::from_slice(&out.stdout).context("unexpected answer from the interpreter")?;
    if probe.implementation != "cpython" {
        bail!("only CPython is supported, not {}", probe.implementation);
    }
    // A macOS framework build's `python3` is a launcher that starts Python.app: run that.
    let base = PathBuf::from(&probe.base_prefix);
    let app = base.join("Resources/Python.app/Contents/MacOS/Python");
    let executable = if app.exists() { std::fs::canonicalize(app)? } else { PathBuf::from(&probe.executable) };

    let mut packages = Vec::new();
    let site = dir.join("site-packages");
    if let Some(req) = &opts.requirements {
        let wheels = opts.wheels.as_ref().ok_or_else(|| anyhow::anyhow!("--wheels is needed with --requirements: packages are installed only from local wheels"))?;
        let report = dir.join("install-report.json");
        let out = clean_env(&mut Command::new(&opts.python))
            .args(["-I", "-m", "pip", "install", "--no-index", "--require-hashes", "--only-binary", ":all:", "--no-compile", "--no-deps"])
            .arg("--find-links")
            .arg(wheels)
            .arg("--target")
            .arg(&site)
            .arg("--report")
            .arg(&report)
            .arg("-r")
            .arg(req)
            .output()
            .context("running pip")?;
        if !out.status.success() {
            bail!("installing the packages failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        packages = installed(&report)?;
        let _ = std::fs::remove_file(&report);
    }
    let mut read_paths = vec![base.clone()];
    if site.exists() {
        read_paths.push(site.clone());
    }
    for lib in linked_libraries(&executable, probe.dynload.as_deref().map(Path::new)) {
        if !read_paths.iter().any(|p| lib.starts_with(p)) {
            read_paths.push(lib);
        }
    }
    let manifest = Manifest {
        format: 1,
        python: probe.version,
        implementation: probe.implementation,
        platform: probe.platform,
        executable,
        read_paths,
        site_packages: if site.exists() { vec![site] } else { vec![] },
        packages,
    };
    super::write_manifest(&dir, &manifest)?;
    Ok(manifest)
}

/// Packages pip installed, from its `--report`.
fn installed(report: &Path) -> Result<Vec<Package>> {
    #[derive(Deserialize)]
    struct Report {
        install: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        metadata: Meta,
        download_info: Download,
    }
    #[derive(Deserialize)]
    struct Meta {
        name: String,
        version: String,
    }
    #[derive(Deserialize)]
    struct Download {
        archive_info: Option<Archive>,
    }
    #[derive(Deserialize)]
    struct Archive {
        hashes: Option<std::collections::BTreeMap<String, String>>,
        hash: Option<String>,
    }
    let r: Report = serde_json::from_slice(&std::fs::read(report)?).context("unreadable pip report")?;
    let mut out = Vec::new();
    for i in r.install {
        let a = i.download_info.archive_info;
        let sha = a
            .as_ref()
            .and_then(|a| a.hashes.as_ref().and_then(|h| h.get("sha256").cloned()))
            .or_else(|| a.and_then(|a| a.hash).and_then(|h| h.strip_prefix("sha256=").map(str::to_string)))
            .ok_or_else(|| anyhow::anyhow!("{} was installed without a SHA-256", i.metadata.name))?;
        out.push(Package { name: i.metadata.name.to_lowercase(), version: i.metadata.version, sha256: sha });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Directories of the shared libraries the interpreter and its extension modules link,
/// outside the system's (macOS: `otool -L`). Both the path as linked and its real path.
fn linked_libraries(executable: &Path, dynload: Option<&Path>) -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut files = vec![executable.to_path_buf()];
    if let Some(d) = dynload {
        if let Ok(entries) = std::fs::read_dir(d) {
            files.extend(entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "so")));
        }
    }
    let mut dirs = std::collections::BTreeSet::new();
    for f in files {
        let Ok(out) = Command::new("/usr/bin/otool").arg("-L").arg(&f).output() else { continue };
        for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
            let lib = line.trim().split(" (").next().unwrap_or("").trim();
            if lib.is_empty() || lib.starts_with("/usr/lib") || lib.starts_with("/System") || lib.starts_with('@') {
                continue;
            }
            let path = PathBuf::from(lib);
            if let Some(parent) = path.parent() {
                dirs.insert(parent.to_path_buf());
            }
            if let Ok(real) = std::fs::canonicalize(&path) {
                if let Some(parent) = real.parent() {
                    dirs.insert(parent.to_path_buf());
                }
            }
        }
    }
    dirs.into_iter().collect()
}
