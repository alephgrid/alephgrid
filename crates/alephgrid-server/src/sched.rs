//! Per-workbook job scheduling (3.01).
//!
//! Every request is a job in one of two lanes:
//! * **write**: one slot (the writer connection); changes run one at a time.
//! * **read**: one slot per reader connection. Bulk reads (sorts, queries, exports) may
//!   use all slots but one, so an interactive read (a fetch) always finds a free reader
//!   even while long reads run, and no read ever waits for a write.
//!
//! Within a lane, interactive jobs go first; bulk jobs that waited longer than [`AGING`]
//! go before them, so neither class starves. Within a class, sessions take turns
//! (round-robin), so one session flooding the queue cannot hold back another.
//!
//! The scheduler only grants slots; the caller runs the job and releases its slot.
//! Queues are bounded per workbook and per session. A job can be withdrawn while it waits
//! (cancel, deadline, disconnect) and interrupted while it runs; interrupting is done
//! under the scheduler lock and only while the slot still runs that job, so it can never
//! hit another job.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::state::duckdb_handles::InterruptHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Interactive,
    Bulk,
}

/// A bulk job waiting this long goes before interactive ones.
pub const AGING: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Jobs waiting or running in one workbook.
    pub per_workbook: usize,
    /// Jobs waiting or running for one session.
    pub per_session: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { per_workbook: 256, per_session: 32 }
    }
}

/// `(session, request id)`; sessions number their own requests.
pub type Key = (u64, u64);

/// Why a job did not run (or stopped).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    #[error("{0}")]
    Overloaded(String),
    #[error("the request did not finish within its deadline")]
    DeadlineExceeded,
    #[error("cancelled")]
    Cancelled,
}

impl JobError {
    pub fn code(&self) -> &'static str {
        match self {
            JobError::Overloaded(_) => "overloaded",
            JobError::DeadlineExceeded => "deadline_exceeded",
            JobError::Cancelled => "cancelled",
        }
    }
}

struct Waiting {
    key: Key,
    class: Class,
    since: Instant,
    grant: oneshot::Sender<usize>,
}

struct LaneState {
    /// Waiting jobs per class, per session, in arrival order.
    queues: [BTreeMap<u64, VecDeque<Waiting>>; 2],
    /// Session served last per class (round-robin cursor).
    last: [u64; 2],
    /// What each slot runs now.
    slots: Vec<Option<(Key, Class)>>,
    interrupts: Vec<Arc<InterruptHandle>>,
    /// Slots bulk jobs may hold at once.
    bulk_slots: usize,
}

impl LaneState {
    fn new(interrupts: Vec<Arc<InterruptHandle>>) -> LaneState {
        let n = interrupts.len();
        LaneState {
            queues: [BTreeMap::new(), BTreeMap::new()],
            last: [0, 0],
            slots: vec![None; n],
            interrupts,
            bulk_slots: if n > 1 { n - 1 } else { n },
        }
    }

    fn waiting(&self) -> usize {
        self.queues.iter().flat_map(|q| q.values()).map(|d| d.len()).sum()
    }

    fn running_bulk(&self) -> usize {
        self.slots.iter().filter(|s| matches!(s, Some((_, Class::Bulk)))).count()
    }

    /// Next job by the rules above, removed from its queue.
    fn next(&mut self) -> Option<Waiting> {
        let bulk_ok = self.running_bulk() < self.bulk_slots;
        let oldest_bulk = self.queues[1].values().filter_map(|d| d.front()).map(|w| w.since).min();
        let aged = oldest_bulk.is_some_and(|t| t.elapsed() >= AGING);
        let order: &[usize] = if aged && bulk_ok { &[1, 0] } else if bulk_ok { &[0, 1] } else { &[0] };
        for &c in order {
            if let Some(w) = self.pop_round_robin(c) {
                return Some(w);
            }
        }
        None
    }

    fn pop_round_robin(&mut self, class: usize) -> Option<Waiting> {
        let q = &mut self.queues[class];
        let after = self.last[class];
        let session = q.range(after + 1..).find(|(_, d)| !d.is_empty()).or_else(|| q.iter().find(|(_, d)| !d.is_empty())).map(|(s, _)| *s)?;
        let d = q.get_mut(&session)?;
        let w = d.pop_front();
        if d.is_empty() {
            q.remove(&session);
        }
        self.last[class] = session;
        w
    }
}

struct Inner {
    read: LaneState,
    write: LaneState,
    per_session: BTreeMap<u64, usize>,
    total: usize,
}

impl Inner {
    fn lane(&mut self, lane: Lane) -> &mut LaneState {
        match lane {
            Lane::Read => &mut self.read,
            Lane::Write => &mut self.write,
        }
    }
}

pub struct Scheduler {
    inner: Mutex<Inner>,
    limits: Limits,
    /// Stops a sheet evaluation running on the writer lane (SEC.07); DuckDB's interrupt
    /// does not reach it.
    sheet_cancel: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

/// A job's place in the scheduler. Dropping it withdraws the job or frees its slot.
pub struct Ticket {
    sched: Arc<Scheduler>,
    key: Key,
    lane: Lane,
    grant: Option<oneshot::Receiver<usize>>,
    slot: Option<usize>,
    done: bool,
}

impl Scheduler {
    pub fn new(writer: Arc<InterruptHandle>, readers: Vec<Arc<InterruptHandle>>, limits: Limits) -> Arc<Scheduler> {
        Arc::new(Scheduler {
            inner: Mutex::new(Inner {
                read: LaneState::new(readers),
                write: LaneState::new(vec![writer]),
                per_session: BTreeMap::new(),
                total: 0,
            }),
            limits,
            sheet_cancel: Mutex::new(None),
        })
    }

    /// Interrupting the writer also sets `flag` (see [`Scheduler::interrupt`]).
    pub fn set_sheet_cancel(&self, flag: Arc<std::sync::atomic::AtomicBool>) {
        *self.sheet_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a job. Fails at once when the workbook's or the session's queue is full.
    pub fn submit(self: &Arc<Self>, key: Key, lane: Lane, class: Class) -> Result<Ticket, JobError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut inner = self.lock();
            if inner.total >= self.limits.per_workbook {
                return Err(JobError::Overloaded(format!(
                    "this workbook is busy ({} requests waiting or running); try again shortly",
                    inner.total
                )));
            }
            let mine = inner.per_session.get(&key.0).copied().unwrap_or(0);
            if mine >= self.limits.per_session {
                return Err(JobError::Overloaded(format!(
                    "too many requests in flight from this connection ({mine}); wait for replies before sending more"
                )));
            }
            inner.total += 1;
            *inner.per_session.entry(key.0).or_default() += 1;
            let ci = class as usize;
            inner.lane(lane).queues[ci].entry(key.0).or_default().push_back(Waiting { key, class, since: Instant::now(), grant: tx });
            Self::pump(&mut inner, lane);
        }
        Ok(Ticket { sched: self.clone(), key, lane, grant: Some(rx), slot: None, done: false })
    }

    fn pump(inner: &mut Inner, lane: Lane) {
        let l = inner.lane(lane);
        while let Some(free) = l.slots.iter().position(|s| s.is_none()) {
            let Some(w) = l.next() else { break };
            l.slots[free] = Some((w.key, w.class));
            if w.grant.send(free).is_err() {
                // Withdrawn just now: give the slot to the next job.
                l.slots[free] = None;
            }
        }
    }

    fn finish(&self, key: Key, lane: Lane, slot: Option<usize>) {
        let mut inner = self.lock();
        let l = inner.lane(lane);
        let mut found = false;
        if let Some(s) = slot {
            if l.slots[s].is_some_and(|(k, _)| k == key) {
                l.slots[s] = None;
                found = true;
            }
        }
        if !found {
            // Still waiting (withdrawn), or granted a slot nobody claimed.
            for q in l.queues.iter_mut() {
                if let Some(d) = q.get_mut(&key.0) {
                    d.retain(|w| w.key != key);
                }
                q.retain(|_, d| !d.is_empty());
            }
            for s in l.slots.iter_mut() {
                if s.is_some_and(|(k, _)| k == key) {
                    *s = None;
                }
            }
        }
        inner.total = inner.total.saturating_sub(1);
        if let Some(n) = inner.per_session.get_mut(&key.0) {
            *n -= 1;
            if *n == 0 {
                inner.per_session.remove(&key.0);
            }
        }
        Self::pump(&mut inner, lane);
    }

    /// Interrupt `key` if, and only if, a slot is running it right now.
    pub fn interrupt(&self, key: Key) -> bool {
        let inner = self.lock();
        for l in [&inner.read, &inner.write] {
            if let Some(s) = l.slots.iter().position(|s| s.is_some_and(|(k, _)| k == key)) {
                l.interrupts[s].interrupt();
                if std::ptr::eq(l, &inner.write) {
                    if let Some(f) = self.sheet_cancel.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                        f.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                return true;
            }
        }
        false
    }

    /// Jobs waiting or running.
    pub fn load(&self) -> usize {
        self.lock().total
    }

    pub fn waiting(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.read.waiting(), inner.write.waiting())
    }
}

impl Ticket {
    /// Wait for a slot. Returns its index (0 for the writer, the reader index otherwise).
    pub async fn granted(&mut self) -> usize {
        if let Some(s) = self.slot {
            return s;
        }
        let rx = self.grant.take().expect("granted twice");
        // The sender lives in the queue until a slot is granted or the ticket is dropped.
        let slot = rx.await.expect("scheduler dropped a waiting job");
        self.slot = Some(slot);
        slot
    }

    pub fn key(&self) -> Key {
        self.key
    }

    pub fn lane(&self) -> Lane {
        self.lane
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            // A grant may have been sent but not received; claim it so the slot is freed.
            if self.slot.is_none() {
                if let Some(mut rx) = self.grant.take() {
                    if let Ok(s) = rx.try_recv() {
                        self.slot = Some(s);
                    }
                }
            }
            self.sched.finish(self.key, self.lane, self.slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handles(n: usize) -> Vec<Arc<InterruptHandle>> {
        // Interrupt handles of real (in-memory) connections.
        (0..n).map(|_| duckdb::Connection::open_in_memory().unwrap().interrupt_handle()).collect()
    }

    fn sched(readers: usize, limits: Limits) -> Arc<Scheduler> {
        Scheduler::new(handles(1).pop().unwrap(), handles(readers), limits)
    }

    fn try_granted(t: &mut Ticket) -> Option<usize> {
        if t.slot.is_some() {
            return t.slot;
        }
        match t.grant.as_mut().unwrap().try_recv() {
            Ok(s) => {
                t.slot = Some(s);
                t.grant = None;
                Some(s)
            }
            Err(_) => None,
        }
    }

    #[test]
    fn writes_run_one_at_a_time_in_order_per_session() {
        let s = sched(2, Limits::default());
        let mut a = s.submit((1, 1), Lane::Write, Class::Interactive).unwrap();
        let mut b = s.submit((1, 2), Lane::Write, Class::Interactive).unwrap();
        assert_eq!(try_granted(&mut a), Some(0));
        assert_eq!(try_granted(&mut b), None);
        drop(a);
        assert_eq!(try_granted(&mut b), Some(0));
    }

    #[test]
    fn interactive_reads_always_find_a_reader_while_bulk_reads_run() {
        let s = sched(2, Limits::default());
        let mut bulk1 = s.submit((1, 1), Lane::Read, Class::Bulk).unwrap();
        let mut bulk2 = s.submit((2, 1), Lane::Read, Class::Bulk).unwrap();
        let mut fetch = s.submit((3, 1), Lane::Read, Class::Interactive).unwrap();
        assert!(try_granted(&mut bulk1).is_some());
        assert_eq!(try_granted(&mut bulk2), None, "bulk reads may hold all readers but one");
        assert!(try_granted(&mut fetch).is_some(), "the reserved reader serves the fetch");
        // And a write never waits for reads.
        let mut w = s.submit((4, 1), Lane::Write, Class::Bulk).unwrap();
        assert_eq!(try_granted(&mut w), Some(0));
    }

    #[test]
    fn sessions_take_turns_and_bulk_jobs_age() {
        let s = sched(1, Limits::default());
        let mut running = s.submit((9, 0), Lane::Write, Class::Interactive).unwrap();
        assert!(try_granted(&mut running).is_some());
        // Session 1 floods; session 2 sends one request.
        let mut flood: Vec<Ticket> = (1..=5).map(|i| s.submit((1, i), Lane::Write, Class::Interactive).unwrap()).collect();
        let mut light = s.submit((2, 1), Lane::Write, Class::Interactive).unwrap();
        drop(running);
        assert!(try_granted(&mut flood[0]).is_some());
        drop(flood.remove(0));
        assert!(try_granted(&mut light).is_some(), "session 2's request runs second, not sixth");
        drop(light);
        // A bulk job waiting longer than AGING goes before newer interactive jobs.
        let mut bulk = s.submit((3, 1), Lane::Write, Class::Bulk).unwrap();
        {
            let mut inner = s.lock();
            for d in inner.write.queues[1].values_mut() {
                for w in d.iter_mut() {
                    w.since -= AGING;
                }
            }
        }
        assert!(try_granted(&mut flood[0]).is_some());
        drop(flood.remove(0));
        assert!(try_granted(&mut bulk).is_some(), "aged bulk job goes next");
    }

    #[test]
    fn queues_are_bounded_per_session_and_per_workbook() {
        let s = sched(1, Limits { per_workbook: 5, per_session: 3 });
        let mine: Vec<Ticket> = (0..3).map(|i| s.submit((1, i), Lane::Read, Class::Interactive).unwrap()).collect();
        assert_eq!(s.submit((1, 9), Lane::Read, Class::Interactive).err().unwrap().code(), "overloaded");
        let other: Vec<Ticket> = (0..2).map(|i| s.submit((2, i), Lane::Read, Class::Interactive).unwrap()).collect();
        let err = s.submit((3, 0), Lane::Read, Class::Interactive).err().unwrap();
        assert!(matches!(err, JobError::Overloaded(m) if m.contains("busy")));
        drop(mine);
        assert_eq!(s.load(), 2);
        assert!(s.submit((3, 0), Lane::Read, Class::Interactive).is_ok());
        drop(other);
    }

    #[test]
    fn withdrawn_jobs_free_their_place_and_interrupts_only_hit_running_jobs() {
        let s = sched(1, Limits::default());
        let mut a = s.submit((1, 1), Lane::Write, Class::Interactive).unwrap();
        let b = s.submit((1, 2), Lane::Write, Class::Interactive).unwrap();
        let mut c = s.submit((1, 3), Lane::Write, Class::Interactive).unwrap();
        assert!(try_granted(&mut a).is_some());
        assert!(!s.interrupt((1, 2)), "a waiting job is not interrupted");
        drop(b); // withdrawn while waiting
        assert!(s.interrupt((1, 1)));
        drop(a);
        assert!(!s.interrupt((1, 1)), "finished: no longer interruptible");
        assert!(try_granted(&mut c).is_some(), "the withdrawn job's turn passed to the next");
        drop(c);
        assert_eq!(s.load(), 0);
    }
}
