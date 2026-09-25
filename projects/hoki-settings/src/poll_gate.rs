//! Block hidden settings polling; each reveal requests an immediate refresh.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

struct State {
    visible: bool,
    generation: u64,
}
pub struct PollGate {
    state: Mutex<State>,
    changed: Condvar,
}
impl PollGate {
    pub fn new(visible: bool) -> Self {
        Self {
            state: Mutex::new(State {
                visible,
                generation: 0,
            }),
            changed: Condvar::new(),
        }
    }
    pub fn set_visible(&self, visible: bool) {
        let mut state = self.state.lock().unwrap();
        if state.visible != visible {
            state.visible = visible;
            state.generation = state.generation.wrapping_add(1);
            self.changed.notify_all();
        }
    }
    pub fn wait_for_poll(&self, previous: Option<(u64, Instant)>, period: Duration) -> u64 {
        let mut state = self.state.lock().unwrap();
        loop {
            if !state.visible {
                state = self.changed.wait(state).unwrap();
                continue;
            }
            let remaining = match previous {
                Some((generation, completed)) if generation == state.generation => {
                    period.saturating_sub(completed.elapsed())
                }
                _ => Duration::ZERO,
            };
            if remaining.is_zero() {
                return state.generation;
            }
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    #[test]
    fn hidden_polling_blocks_and_reveal_refreshes_immediately() {
        let gate = Arc::new(PollGate::new(false));
        let worker_gate = gate.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(worker_gate.wait_for_poll(None, Duration::from_secs(5)))
                .unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        gate.set_visible(true);
        let generation = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
        // Hide/reveal during an in-flight read must not lose the refresh request.
        gate.set_visible(false);
        gate.set_visible(true);
        assert_ne!(
            gate.wait_for_poll(
                Some((generation, Instant::now())),
                Duration::from_secs(3600)
            ),
            generation
        );
    }
    #[test]
    fn visible_polling_keeps_its_interval() {
        let gate = PollGate::new(true);
        let generation = gate.wait_for_poll(None, Duration::from_secs(5));
        let now = Instant::now();
        gate.set_visible(true); // redundant notifications must not cause a poll
        assert_eq!(
            gate.wait_for_poll(Some((generation, now)), Duration::from_millis(25)),
            generation
        );
        assert!(now.elapsed() >= Duration::from_millis(25));
    }
}
