use std::sync::mpsc::{self, Sender};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Run mixer operations in submission order without blocking the UI thread.
#[derive(Clone)]
pub struct Worker {
    sender: Sender<Job>,
}

impl Worker {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in receiver {
                job();
            }
        });
        Self { sender }
    }

    pub fn submit(&self, job: impl FnOnce() + Send + 'static) {
        if self.sender.send(Box::new(job)).is_err() {
            eprintln!("audio command worker unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn queued_changes_finish_in_submission_order_even_when_first_is_delayed() {
        let worker = Worker::new();
        let (started, start) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (updates, results) = mpsc::channel();
        let first = updates.clone();
        worker.submit(move || {
            started.send(()).unwrap();
            gate.recv().unwrap();
            first.send(25).unwrap();
        });
        start.recv_timeout(Duration::from_secs(5)).unwrap();
        let another_callback = worker.clone();
        another_callback.submit(move || updates.send(80).unwrap());
        // Dropping UI callback handles must still allow already queued work.
        drop(worker);
        drop(another_callback);
        release.send(()).unwrap();
        assert_eq!(results.recv_timeout(Duration::from_secs(5)).unwrap(), 25);
        assert_eq!(results.recv_timeout(Duration::from_secs(5)).unwrap(), 80);
        assert!(matches!(
            results.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
