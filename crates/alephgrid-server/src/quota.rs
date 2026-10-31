//! Per-space quotas (7.07): storage, workbooks, concurrent bulk jobs and open workbooks.
//!
//! Each limit is the space's override (set by an organization admin) or the server default
//! (`--space-*` flags); none means unlimited. Usage is measured, not trusted from clients:
//!
//! * **storage**: bytes of the space's workbook directories, the trash included. Refused when
//!   it would pass the limit: creating, duplicating or moving a workbook in, uploads (counted as
//!   they stream) and bulk changes (imports, query tables, copies). Small interactive edits
//!   still work over the limit, so people can delete data to get under it.
//! * **workbooks**: workbooks of the space outside the trash.
//! * **jobs**: bulk work (queries, exports, imports, copies) running at once for the space.
//! * **open workbooks**: allocations live for the space's workbooks at once (compute).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Limits of one space; `None` is no limit (or, for an override, "use the default").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct QuotaLimits {
    pub storage_bytes: Option<u64>,
    pub workbooks: Option<u64>,
    pub jobs: Option<u64>,
    pub open_workbooks: Option<u64>,
}

impl QuotaLimits {
    /// `self` where set, else `defaults`.
    pub fn over(self, defaults: QuotaLimits) -> QuotaLimits {
        QuotaLimits {
            storage_bytes: self.storage_bytes.or(defaults.storage_bytes),
            workbooks: self.workbooks.or(defaults.workbooks),
            jobs: self.jobs.or(defaults.jobs),
            open_workbooks: self.open_workbooks.or(defaults.open_workbooks),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum QuotaKind {
    Storage,
    Workbooks,
    Jobs,
    OpenWorkbooks,
}

/// A request refused because a space reached a limit.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct QuotaExceeded {
    pub kind: QuotaKind,
    pub message: String,
}

fn exceeded(kind: QuotaKind, message: String) -> anyhow::Error {
    anyhow::Error::new(QuotaExceeded { kind, message })
}

/// What a space uses, with its limits (`GET /api/orgs/{id}/usage`).
#[derive(Debug, Clone, Serialize, TS)]
pub struct SpaceUsage {
    pub space: crate::auth::Space,
    /// Bytes of its workbooks, the trash included.
    pub storage_bytes: u64,
    pub trash_bytes: u64,
    pub workbooks: u64,
    pub open_workbooks: u64,
    pub jobs: u64,
    /// The limits that apply.
    pub limits: QuotaLimits,
    /// The space's own overrides of the server defaults.
    pub overrides: QuotaLimits,
}

/// Counts that change while the server runs: bulk jobs and allocation starts per space.
#[derive(Default)]
pub struct Counters {
    jobs: Mutex<HashMap<String, u64>>,
    opening: Mutex<HashMap<String, u64>>,
    storage: Mutex<HashMap<String, (Instant, u64)>>,
}

/// How long a measured storage total is reused.
const STORAGE_TTL: Duration = Duration::from_secs(2);

/// Holds one of a space's job slots until dropped.
pub struct JobSlot {
    state: crate::state::Shared,
    space: String,
}

impl Drop for JobSlot {
    fn drop(&mut self) {
        self.state.quota.release_job(&self.space);
    }
}

/// A start counted against a space's open workbooks until dropped.
pub struct Opening<'a> {
    counters: &'a Counters,
    space: String,
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        let mut o = self.counters.opening.lock().unwrap();
        if let Some(n) = o.get_mut(&self.space) {
            *n = n.saturating_sub(1);
        }
    }
}

fn mib(n: u64) -> String {
    format!("{} MiB", n.div_ceil(1 << 20))
}

impl Counters {
    pub fn jobs_of(&self, space: &str) -> u64 {
        self.jobs.lock().unwrap().get(space).copied().unwrap_or(0)
    }

    /// Count a job of `space`, or refuse at its limit.
    fn take_job(&self, space: &str, limit: Option<u64>) -> anyhow::Result<()> {
        let mut j = self.jobs.lock().unwrap();
        let n = j.entry(space.to_string()).or_insert(0);
        if let Some(max) = limit {
            if *n >= max {
                return Err(exceeded(QuotaKind::Jobs, format!("this space is already running {n} bulk job(s), its limit; wait for one to finish or ask an admin to raise it")));
            }
        }
        *n += 1;
        Ok(())
    }

    fn release_job(&self, space: &str) {
        let mut j = self.jobs.lock().unwrap();
        if let Some(n) = j.get_mut(space) {
            *n = n.saturating_sub(1);
        }
    }

    /// Count a start against `space`'s open workbooks, `live` being those open now.
    pub fn opening(&self, space: &str, live: u64, limit: Option<u64>) -> anyhow::Result<Opening<'_>> {
        let mut o = self.opening.lock().unwrap();
        let n = o.entry(space.to_string()).or_insert(0);
        if let Some(max) = limit {
            if live + *n >= max {
                return Err(exceeded(
                    QuotaKind::OpenWorkbooks,
                    format!("this space already has {} workbook(s) open, its limit; close one (it closes by itself when idle) or ask an admin to raise it", live + *n),
                ));
            }
        }
        *n += 1;
        Ok(Opening { counters: self, space: space.to_string() })
    }

    /// A cached storage total of `space`, or `measure` it.
    pub fn storage(&self, space: &str, measure: impl FnOnce() -> anyhow::Result<u64>) -> anyhow::Result<u64> {
        if let Some((at, v)) = self.storage.lock().unwrap().get(space) {
            if at.elapsed() < STORAGE_TTL {
                return Ok(*v);
            }
        }
        let v = measure()?;
        self.storage.lock().unwrap().insert(space.to_string(), (Instant::now(), v));
        Ok(v)
    }

    /// Forget measured totals (after something grew or shrank).
    pub fn forget(&self, space: &str) {
        self.storage.lock().unwrap().remove(space);
    }
}

/// Refuse when `used + adding` passes `limit`.
pub fn check_storage(used: u64, adding: u64, limit: Option<u64>) -> anyhow::Result<()> {
    match limit {
        Some(max) if used.saturating_add(adding) > max || (adding == 0 && used >= max) => Err(exceeded(
            QuotaKind::Storage,
            format!("this space would use {} of its {} storage; delete data or empty its trash, or ask an admin to raise the limit", mib(used + adding), mib(max)),
        )),
        _ => Ok(()),
    }
}

pub fn check_workbooks(count: u64, limit: Option<u64>) -> anyhow::Result<()> {
    match limit {
        Some(max) if count >= max => {
            Err(exceeded(QuotaKind::Workbooks, format!("this space already holds {count} workbook(s), its limit; delete one or ask an admin to raise it")))
        }
        _ => Ok(()),
    }
}

impl crate::state::AppState {
    /// The limits that apply to `space`.
    pub fn limits(&self, space: &str) -> anyhow::Result<QuotaLimits> {
        Ok(self.auth.store.space_quota(space)?.over(self.config.quota))
    }

    /// Bytes of the space's workbooks (live and trashed), and of the trashed ones alone.
    fn measure(&self, space: &str) -> anyhow::Result<(u64, u64, u64)> {
        let live: Vec<_> = self.list()?.into_iter().filter(|w| w.meta.space == space).collect();
        let trash: u64 = self.trash()?.into_iter().filter(|w| w.meta.space == space).map(|w| w.size_bytes).sum();
        Ok((live.iter().map(|w| w.size_bytes).sum::<u64>() + trash, trash, live.len() as u64))
    }

    pub fn storage_used(&self, space: &str) -> anyhow::Result<u64> {
        self.quota.storage(space, || Ok(self.measure(space)?.0))
    }

    /// Refuse if adding `bytes` to `space` would pass its storage limit.
    pub fn check_storage(&self, space: &str, bytes: u64) -> anyhow::Result<()> {
        let limit = self.limits(space)?.storage_bytes;
        if limit.is_none() {
            return Ok(());
        }
        check_storage(self.storage_used(space)?, bytes, limit)
    }

    /// Refuse a new workbook (`bytes` big) in `space` at its workbook or storage limit.
    pub fn check_new_workbook(&self, space: &str, bytes: u64) -> anyhow::Result<()> {
        let limits = self.limits(space)?;
        if limits.workbooks.is_some() {
            let count = self.list()?.into_iter().filter(|w| w.meta.space == space).count() as u64;
            check_workbooks(count, limits.workbooks)?;
        }
        if limits.storage_bytes.is_some() {
            self.quota.forget(space);
            check_storage(self.storage_used(space)?, bytes, limits.storage_bytes)?;
        }
        Ok(())
    }

    /// A job slot of `space` for one bulk request, held until dropped.
    pub fn job_slot(self: &std::sync::Arc<Self>, space: &str) -> anyhow::Result<JobSlot> {
        let limit = self.limits(space)?.jobs;
        self.quota.take_job(space, limit)?;
        Ok(JobSlot { state: self.clone(), space: space.to_string() })
    }

    /// Workbooks of `space` with a live allocation now.
    pub fn open_in(&self, space: &str) -> u64 {
        self.controller.live().iter().filter(|id| self.workbook_space(id).ok().as_deref() == Some(space)).count() as u64
    }

    /// Usage and limits of every space of `org` (or of every space with `None`).
    pub fn usage(&self, org: Option<&str>) -> anyhow::Result<Vec<SpaceUsage>> {
        let mut out = Vec::new();
        for s in self.auth.store.spaces()?.into_iter().filter(|s| org.is_none_or(|o| s.org_id == o)) {
            let (storage, trash, workbooks) = self.measure(&s.id)?;
            let overrides = self.auth.store.space_quota(&s.id)?;
            out.push(SpaceUsage {
                storage_bytes: storage,
                trash_bytes: trash,
                workbooks,
                open_workbooks: self.open_in(&s.id),
                jobs: self.quota.jobs_of(&s.id),
                limits: overrides.over(self.config.quota),
                overrides,
                space: s,
            });
        }
        Ok(out)
    }
}
