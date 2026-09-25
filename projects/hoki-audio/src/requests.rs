use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared by UI callbacks and background completions. Only the latest request
/// may publish its result; starting a new request invalidates older tickets.
#[derive(Clone, Default)]
pub struct Requests(Arc<AtomicU64>);

pub struct Ticket {
    requests: Requests,
    generation: u64,
}

impl Requests {
    pub fn begin(&self) -> Ticket {
        let generation = self.0.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        Ticket {
            requests: self.clone(),
            generation,
        }
    }
}

impl Ticket {
    pub fn is_current(&self) -> bool {
        self.requests.0.load(Ordering::SeqCst) == self.generation
    }

    /// Call on the UI thread, where both request creation and result application
    /// are serialized by the event loop.
    pub fn apply_if_current(self, apply: impl FnOnce()) {
        if self.is_current() {
            apply();
        }
    }
}
