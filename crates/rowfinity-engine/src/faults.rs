//! Fault injection for tests. Named points in persistence and mutation code call
//! [`check`]; a test arms a point with [`fail_next`] and the next check on the same
//! thread returns an error. Unarmed checks cost one thread-local lookup, and nothing
//! outside Rust code can arm a point.

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static ARMED: RefCell<HashMap<&'static str, u32>> = RefCell::new(HashMap::new());
}

/// Make the next `times` checks of `point` on this thread fail (default once).
pub fn fail_next(point: &'static str) {
    fail_times(point, 1);
}

pub fn fail_times(point: &'static str, times: u32) {
    ARMED.with(|a| {
        a.borrow_mut().insert(point, times);
    });
}

/// Disarm every point on this thread.
pub fn reset() {
    ARMED.with(|a| a.borrow_mut().clear());
}

pub(crate) fn check(point: &'static str) -> anyhow::Result<()> {
    let fire = ARMED.with(|a| {
        let mut a = a.borrow_mut();
        match a.get_mut(point) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    });
    if fire {
        anyhow::bail!("injected fault at {point}");
    }
    Ok(())
}
