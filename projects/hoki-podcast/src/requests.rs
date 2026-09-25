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
    /// Call on the UI thread, where both request creation and result application
    /// are serialized by the event loop.
    pub fn apply_if_current(self, apply: impl FnOnce()) {
        if self.requests.0.load(Ordering::SeqCst) == self.generation {
            apply();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_success_and_error_cannot_replace_latest_selection() {
        let requests = Requests::default();
        let first = requests.begin();
        let second = requests.begin();
        let selected_again = requests.begin();
        let mut applied = Vec::new();
        selected_again.apply_if_current(|| applied.push("latest selection"));
        first.apply_if_current(|| applied.push("old success"));
        second.apply_if_current(|| applied.push("old error"));
        assert_eq!(applied, ["latest selection"]);
    }

    #[test]
    fn cached_selection_invalidates_pending_refresh_or_download() {
        let requests = Requests::default();
        let pending = requests.begin();
        let _cached_selection = requests.begin();
        pending.apply_if_current(|| panic!("old request changed the current UI"));
    }

    #[test]
    fn background_completion_is_checked_when_applied_not_when_download_ends() {
        let requests = Requests::default();
        let pending = requests.begin();
        let completed = std::thread::spawn(move || pending).join().unwrap();
        let refresh = requests.begin();
        completed.apply_if_current(|| panic!("queued stale completion was applied"));
        let mut applied = false;
        refresh.apply_if_current(|| applied = true);
        assert!(applied);
    }
}
