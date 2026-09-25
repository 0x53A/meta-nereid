//! Serialize potentially blocking system actions away from the UI thread.
use std::sync::mpsc::{self, SyncSender};

pub struct ActionWorker(SyncSender<String>);
impl ActionWorker {
    pub fn new<T: 'static>(
        mut run: impl FnMut(&str) -> T + Send + 'static,
        mut complete: impl FnMut(T) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<String>(1);
        std::thread::Builder::new()
            .name("settings-actions".into())
            .spawn(move || {
                while let Ok(action) = rx.recv() {
                    complete(run(&action));
                }
            })?;
        Ok(Self(tx))
    }
    pub fn submit(&self, action: String) -> Result<(), String> {
        self.0
            .try_send(action)
            .map_err(|_| "Another action is still running; try again.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_action_does_not_block_submit_and_failure_reaches_completion() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = ActionWorker::new(
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err::<String, String>("radio unavailable".into())
            },
            move |result| {
                done_tx.send(result).unwrap();
            },
        )
        .unwrap();
        worker.submit("toggle-wifi".into()).unwrap();
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        // The worker is blocked; UI submission remains bounded and responsive.
        worker.submit("toggle-bt".into()).unwrap();
        assert!(worker.submit("third action".into()).is_err());
        release_tx.send(()).unwrap();
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            Err("radio unavailable".into())
        );
        release_tx.send(()).unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
    }
}
