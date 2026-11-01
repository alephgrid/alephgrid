//! Host-wide resource budget (3.03).
//!
//! The budget is split between live allocations, recomputed whenever one starts or stops.
//! An allocation with a fixed profile (7.13) gets exactly its memory and threads; the
//! others (`standard`) get equal shares of what is left, never below the policy's minimum.
//! Temporary disk is split equally between all of them. An allocation is in the ledger
//! from the moment it is admitted, so concurrent starts plan against each other. Shares are applied
//! as DuckDB settings (`memory_limit`, `threads`, `max_temp_directory_size`) of each
//! workbook's database instance, shrinking existing ones before a new one opens, so the
//! sum never exceeds the budget. Half of the temporary-disk budget is kept for staged
//! uploads, which reserve their bytes as they arrive.
//!
//! What this enforces, precisely: DuckDB's buffer-manager limit and worker threads per
//! instance, DuckDB's spill files, and upload bytes. It does not bound the process's
//! resident memory (DuckDB allocations outside its buffer manager, IronCalc sheets, the
//! server itself), which an OS-level limit (cgroup, container) must cap. Sheet memory is
//! estimated from the stored model size and subtracted from DuckDB's share.

use std::collections::HashMap;

use serde::Serialize;
use ts_rs::TS;

use super::{FailureKind, Reservation, RuntimeFailure};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
pub struct HostBudget {
    pub memory_bytes: u64,
    pub threads: u64,
    pub temp_disk_bytes: u64,
}

impl HostBudget {
    pub fn upload_bytes(&self) -> u64 {
        self.temp_disk_bytes / 2
    }

    fn allocation_temp(&self) -> u64 {
        self.temp_disk_bytes - self.upload_bytes()
    }
}

/// DuckDB needs some memory to run at all; shares below this are refused.
pub const MIN_DUCKDB_MEMORY: u64 = 64 << 20;

#[derive(Debug, Default)]
pub struct Ledger {
    allocations: HashMap<String, Reservation>,
    /// Allocations with a fixed profile, and its reservation.
    fixed: HashMap<String, Reservation>,
    /// Estimated sheet-model bytes per workbook, subtracted from DuckDB's share.
    sheets: HashMap<String, u64>,
    uploads: u64,
}

/// Usage report (`GET /api/admin/resources`).
#[derive(Debug, Clone, Serialize, TS)]
pub struct ResourceUsage {
    pub budget: HostBudget,
    pub allocations: Vec<AllocationUsage>,
    pub reserved: Reservation,
    pub upload_bytes: u64,
    pub upload_budget: u64,
    /// Largest a workbook's database may grow, if limited (SEC.11).
    pub workbook_size_limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct AllocationUsage {
    pub workbook: String,
    pub reservation: Reservation,
    /// Settings read back from the workbook's DuckDB instance.
    pub duckdb_memory_limit: String,
    pub duckdb_threads: String,
    /// Jobs waiting or running.
    pub jobs: u64,
    /// Bytes of the workbook's database file and log.
    pub database_bytes: u64,
}

impl Ledger {
    pub fn live(&self) -> usize {
        self.allocations.len()
    }

    pub fn contains(&self, workbook: &str) -> bool {
        self.allocations.contains_key(workbook)
    }

    /// Equal shares for `n` allocations, or why `n` do not fit.
    pub fn shares(budget: &HostBudget, n: usize, min_memory: u64) -> Result<Reservation, RuntimeFailure> {
        let n = n.max(1) as u64;
        let memory = budget.memory_bytes / n;
        let floor = min_memory.max(MIN_DUCKDB_MEMORY);
        if memory < floor {
            return Err(RuntimeFailure::new(
                FailureKind::Capacity,
                format!(
                    "the server is at capacity: {n} open workbooks would get {} MiB each, below the minimum of {} MiB; close a workbook or raise --memory-budget",
                    memory >> 20,
                    floor >> 20
                ),
            ));
        }
        if budget.threads < n {
            return Err(RuntimeFailure::new(
                FailureKind::Capacity,
                format!("the server is at capacity: {n} open workbooks need at least {n} threads, the budget is {}", budget.threads),
            ));
        }
        Ok(Reservation { memory_bytes: memory, threads: budget.threads / n, temp_disk_bytes: budget.allocation_temp() / n })
    }

    /// The reservations every allocation in the ledger would have with `change` applied:
    /// `(workbook, fixed)` adds the workbook (or replaces its demand) with a fixed
    /// reservation or, with `None`, an equal share.
    pub fn plan(&self, budget: &HostBudget, change: Option<(&str, Option<Reservation>)>, min_memory: u64) -> Result<HashMap<String, Reservation>, RuntimeFailure> {
        let mut demands: Vec<(String, Option<Reservation>)> =
            self.allocations.keys().map(|k| (k.clone(), self.fixed.get(k).copied())).collect();
        if let Some((wb, fixed)) = change {
            demands.retain(|(k, _)| k != wb);
            demands.push((wb.to_string(), fixed));
        }
        let fixed_memory: u64 = demands.iter().filter_map(|(_, f)| f.map(|r| r.memory_bytes)).sum();
        let fixed_threads: u64 = demands.iter().filter_map(|(_, f)| f.map(|r| r.threads)).sum();
        let elastic = demands.iter().filter(|(_, f)| f.is_none()).count();
        if fixed_memory > budget.memory_bytes || fixed_threads > budget.threads {
            return Err(RuntimeFailure::new(
                FailureKind::Capacity,
                format!(
                    "the server is at capacity: workbooks with fixed profiles would reserve {} MiB and {fixed_threads} threads of {} MiB and {}; close a workbook or wait",
                    fixed_memory >> 20,
                    budget.memory_bytes >> 20,
                    budget.threads
                ),
            ));
        }
        let rest = HostBudget { memory_bytes: budget.memory_bytes - fixed_memory, threads: budget.threads - fixed_threads, temp_disk_bytes: budget.temp_disk_bytes };
        let share = if elastic > 0 { Some(Self::shares(&rest, elastic, min_memory)?) } else { None };
        let temp = budget.allocation_temp() / demands.len().max(1) as u64;
        Ok(demands
            .into_iter()
            .map(|(k, f)| {
                let r = f.or(share).expect("an elastic share exists when there are elastic demands");
                (k, Reservation { temp_disk_bytes: temp, ..r })
            })
            .collect())
    }

    /// The reservation recorded for `workbook`.
    pub fn get(&self, workbook: &str) -> Option<Reservation> {
        self.allocations.get(workbook).copied()
    }

    /// Record `workbook`'s demand (fixed reservation or equal share).
    pub fn set_fixed(&mut self, workbook: &str, fixed: Option<Reservation>) {
        match fixed {
            Some(r) => self.fixed.insert(workbook.to_string(), r),
            None => self.fixed.remove(workbook),
        };
    }

    pub fn set(&mut self, workbook: &str, r: Reservation) {
        self.allocations.insert(workbook.to_string(), r);
    }

    pub fn release(&mut self, workbook: &str) {
        self.allocations.remove(workbook);
        self.fixed.remove(workbook);
        self.sheets.remove(workbook);
    }

    pub fn set_sheet_estimate(&mut self, workbook: &str, bytes: u64) {
        self.sheets.insert(workbook.to_string(), bytes);
    }

    /// DuckDB's part of a reservation: the share minus the sheet estimate, at least
    /// [`MIN_DUCKDB_MEMORY`].
    pub fn duckdb_memory(&self, workbook: &str, r: &Reservation) -> u64 {
        r.memory_bytes.saturating_sub(self.sheets.get(workbook).copied().unwrap_or(0)).max(MIN_DUCKDB_MEMORY.min(r.memory_bytes))
    }

    pub fn reservations(&self) -> Vec<(String, Reservation)> {
        let mut v: Vec<_> = self.allocations.iter().map(|(k, r)| (k.clone(), *r)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    pub fn total(&self) -> Reservation {
        self.allocations.values().fold(Reservation::default(), |a, r| Reservation {
            memory_bytes: a.memory_bytes + r.memory_bytes,
            threads: a.threads + r.threads,
            temp_disk_bytes: a.temp_disk_bytes + r.temp_disk_bytes,
        })
    }

    /// Reserve `bytes` of upload space, or say how much is left.
    pub fn reserve_upload(&mut self, budget: &HostBudget, bytes: u64) -> Result<(), u64> {
        let left = budget.upload_bytes().saturating_sub(self.uploads);
        if bytes > left {
            return Err(left);
        }
        self.uploads += bytes;
        Ok(())
    }

    pub fn release_upload(&mut self, bytes: u64) {
        self.uploads = self.uploads.saturating_sub(bytes);
    }

    pub fn uploads(&self) -> u64 {
        self.uploads
    }
}

/// Total physical memory, honouring a cgroup (container) limit when there is one.
pub fn host_memory() -> u64 {
    let physical = physical_memory().unwrap_or(8 << 30);
    match cgroup_memory_limit() {
        Some(limit) if limit < physical => limit,
        _ => physical,
    }
}

fn cgroup_memory_limit() -> Option<u64> {
    for path in ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(n) = text.trim().parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn physical_memory() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(target_os = "macos")]
fn physical_memory() -> Option<u64> {
    let out = std::process::Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn physical_memory() -> Option<u64> {
    None
}

/// Parse sizes such as `8GB`, `512MiB`, `1.5 GB` or `1048576`.
pub fn parse_bytes(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let num: f64 = num.parse().map_err(|_| format!("invalid size {text:?}"))?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1000,
        "kib" | "k" => 1 << 10,
        "mb" => 1000 * 1000,
        "mib" | "m" => 1 << 20,
        "gb" => 1000 * 1000 * 1000,
        "gib" | "g" => 1 << 30,
        "tb" => 1000u64.pow(4),
        "tib" | "t" => 1 << 40,
        other => return Err(format!("unknown size unit {other:?} in {text:?}")),
    };
    if !num.is_finite() || num < 0.0 {
        return Err(format!("invalid size {text:?}"));
    }
    Ok((num * mult as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> HostBudget {
        HostBudget { memory_bytes: 4 << 30, threads: 8, temp_disk_bytes: 10 << 30 }
    }

    #[test]
    fn shares_split_the_budget_and_refuse_below_the_floor() {
        let b = budget();
        let one = Ledger::shares(&b, 1, 256 << 20).unwrap();
        assert_eq!(one, Reservation { memory_bytes: 4 << 30, threads: 8, temp_disk_bytes: 5 << 30 });
        let four = Ledger::shares(&b, 4, 256 << 20).unwrap();
        assert_eq!(four.memory_bytes * 4, 4 << 30);
        assert_eq!(four.threads, 2);
        // 16 × 256 MiB = 4 GiB fits exactly; 17 does not; 9 exceed the 8 threads.
        assert!(Ledger::shares(&HostBudget { threads: 64, ..b }, 16, 256 << 20).is_ok());
        let err = Ledger::shares(&HostBudget { threads: 64, ..b }, 17, 256 << 20).unwrap_err();
        assert_eq!(err.kind, FailureKind::Capacity);
        assert_eq!(Ledger::shares(&b, 9, 1).unwrap_err().kind, FailureKind::Capacity);
    }

    #[test]
    fn plans_never_exceed_the_budget() {
        let b = budget();
        let mut l = Ledger::default();
        for i in 0..8 {
            let plan = l.plan(&b, Some((&format!("w{i}"), None)), 64 << 20).unwrap();
            for (k, r) in &plan {
                l.set(k, *r);
            }
            let t = l.total();
            assert!(t.memory_bytes <= b.memory_bytes && t.threads <= b.threads && t.temp_disk_bytes <= b.temp_disk_bytes - b.upload_bytes());
        }
        assert!(l.plan(&b, Some(("w8", None)), 64 << 20).is_err(), "the 9th exceeds 8 threads");
        l.release("w0");
        assert!(l.plan(&b, Some(("w8", None)), 64 << 20).is_ok());
    }

    #[test]
    fn fixed_profiles_get_exactly_theirs_and_standard_shares_the_rest() {
        let b = budget(); // 4 GiB, 8 threads
        let large = Reservation { memory_bytes: 2 << 30, threads: 4, temp_disk_bytes: 0 };
        let mut l = Ledger::default();
        let apply = |l: &mut Ledger, plan: &HashMap<String, Reservation>| {
            for (k, r) in plan {
                l.set(k, *r);
            }
        };
        let plan = l.plan(&b, Some(("big", Some(large))), 256 << 20).unwrap();
        assert_eq!(plan["big"].memory_bytes, 2 << 30);
        l.set_fixed("big", Some(large));
        apply(&mut l, &plan);
        for w in ["a", "b"] {
            let plan = l.plan(&b, Some((w, None)), 256 << 20).unwrap();
            apply(&mut l, &plan);
        }
        let plan = l.plan(&b, None, 256 << 20).unwrap();
        assert_eq!(plan["big"], Reservation { temp_disk_bytes: plan["big"].temp_disk_bytes, ..large }, "never shrunk by others");
        assert_eq!(plan["a"].memory_bytes, 1 << 30, "the other 2 GiB split between the two");
        assert_eq!(plan["a"].threads, 2);
        let t: u64 = plan.values().map(|r| r.memory_bytes).sum();
        assert_eq!(t, 4 << 30);
        // A second fixed 2 GiB does not fit beside two standard minimums.
        assert_eq!(l.plan(&b, Some(("big2", Some(large))), 256 << 20).unwrap_err().kind, FailureKind::Capacity);
        // Resizing an existing one replaces its demand rather than adding to it.
        let smaller = Reservation { memory_bytes: 1 << 30, threads: 2, temp_disk_bytes: 0 };
        let plan = l.plan(&b, Some(("big", Some(smaller))), 256 << 20).unwrap();
        assert_eq!(plan.len(), 3);
        assert_eq!(plan["a"].memory_bytes, 3 << 29);
    }

    #[test]
    fn uploads_have_their_own_half_of_the_temp_budget() {
        let b = budget();
        let mut l = Ledger::default();
        assert!(l.reserve_upload(&b, 5 << 30).is_ok());
        assert_eq!(l.reserve_upload(&b, 1), Err(0));
        l.release_upload(1 << 30);
        assert_eq!(l.reserve_upload(&b, 2 << 30), Err(1 << 30));
    }

    #[test]
    fn sheet_memory_comes_out_of_duckdbs_share() {
        let mut l = Ledger::default();
        let r = Reservation { memory_bytes: 1 << 30, threads: 1, temp_disk_bytes: 0 };
        assert_eq!(l.duckdb_memory("a", &r), 1 << 30);
        l.set_sheet_estimate("a", 256 << 20);
        assert_eq!(l.duckdb_memory("a", &r), 768 << 20);
        l.set_sheet_estimate("a", 2 << 30);
        assert_eq!(l.duckdb_memory("a", &r), MIN_DUCKDB_MEMORY);
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_bytes("8GB").unwrap(), 8_000_000_000);
        assert_eq!(parse_bytes("512MiB").unwrap(), 512 << 20);
        assert_eq!(parse_bytes("1.5 GiB").unwrap(), 3 << 29);
        assert_eq!(parse_bytes("1048576").unwrap(), 1 << 20);
        assert!(parse_bytes("lots").is_err());
        assert!(parse_bytes("5 parsecs").is_err());
    }
}
