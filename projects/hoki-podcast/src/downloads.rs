use crate::storage;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Queued,
    Downloading(Option<u8>),
    Ready,
    Failed,
}

impl State {
    pub fn label(&self) -> String {
        match self {
            Self::Queued => "QUEUED".into(),
            Self::Downloading(Some(percent)) => format!("DOWNLOADING {percent}%"),
            Self::Downloading(None) => "DOWNLOADING".into(),
            Self::Ready => "READY · TAP TO PLAY".into(),
            Self::Failed => "FAILED · TAP TO RETRY".into(),
        }
    }

    pub fn progress(&self) -> f32 {
        match self {
            Self::Downloading(Some(percent)) => *percent as f32 / 100.0,
            Self::Ready => 1.0,
            _ => 0.0,
        }
    }

    fn active(&self) -> bool {
        matches!(self, Self::Queued | Self::Downloading(_))
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub title: String,
    pub duration: String,
    pub state: State,
}

struct Job {
    key: String,
    url: String,
    path: PathBuf,
}

enum Event {
    Started(String),
    Progress(String, u8),
    Finished(String, Result<(), String>),
}

pub struct Queue {
    jobs: Sender<Job>,
    events: Receiver<Event>,
    entries: Vec<Entry>,
    by_key: HashMap<String, usize>,
}

impl Queue {
    pub fn start() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        std::thread::spawn(move || worker(jobs_rx, events_tx));
        Self::from_channels(jobs_tx, events_rx)
    }

    fn from_channels(jobs: Sender<Job>, events: Receiver<Event>) -> Self {
        Self { jobs, events, entries: Vec::new(), by_key: HashMap::new() }
    }

    pub fn enqueue(&mut self, key: String, title: String, duration: String, url: String, path: PathBuf) -> bool {
        if let Some(&index) = self.by_key.get(&key) {
            if self.entries[index].state.active() {
                return false;
            }
            self.entries[index].title = title;
            self.entries[index].duration = duration;
            self.entries[index].state = State::Queued;
        } else {
            self.by_key.insert(key.clone(), self.entries.len());
            self.entries.push(Entry { key: key.clone(), title, duration, state: State::Queued });
        }
        if self.jobs.send(Job { key: key.clone(), url, path }).is_err() {
            self.set_state(&key, State::Failed);
            return false;
        }
        true
    }

    pub fn state(&self, key: &str) -> Option<&State> {
        self.by_key.get(key).map(|&index| &self.entries[index].state)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn active_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.state.active()).count()
    }

    pub fn drain(&mut self) -> Vec<String> {
        let mut changed = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            let key = match event {
                Event::Started(key) => {
                    self.set_state(&key, State::Downloading(None));
                    key
                }
                Event::Progress(key, percent) => {
                    self.set_state(&key, State::Downloading(Some(percent)));
                    key
                }
                Event::Finished(key, result) => {
                    if let Err(error) = &result {
                        eprintln!("podcast download {key}: {error}");
                    }
                    self.set_state(&key, if result.is_ok() { State::Ready } else { State::Failed });
                    key
                }
            };
            if !changed.contains(&key) { changed.push(key); }
        }
        changed
    }

    fn set_state(&mut self, key: &str, state: State) {
        if let Some(&index) = self.by_key.get(key) {
            self.entries[index].state = state;
        }
    }
}

struct ProgressReader<R> {
    inner: R,
    total: Option<u64>,
    read: u64,
    last_percent: u8,
    key: String,
    events: Sender<Event>,
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.read += count as u64;
        if let Some(total) = self.total.filter(|total| *total > 0) {
            let percent = (self.read.saturating_mul(100) / total).min(100) as u8;
            if percent > self.last_percent {
                self.last_percent = percent;
                let _ = self.events.send(Event::Progress(self.key.clone(), percent));
            }
        }
        Ok(count)
    }
}

fn worker(jobs: Receiver<Job>, events: Sender<Event>) {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(45))
        .build();
    while let Ok(job) = jobs.recv() {
        let _ = events.send(Event::Started(job.key.clone()));
        let result = (|| {
            let response = agent.get(&job.url).call().map_err(|error| error.to_string())?;
            let total = response.header("Content-Length").and_then(|value| value.parse::<u64>().ok());
            let mut reader = ProgressReader {
                inner: response.into_reader(), total, read: 0, last_percent: 0,
                key: job.key.clone(), events: events.clone(),
            };
            storage::save_download(&mut reader, &job.path).map_err(|error| error.to_string())
        })();
        let _ = events.send(Event::Finished(job.key, result));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;

    #[test]
    fn queue_deduplicates_and_preserves_order_then_allows_retry() {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let mut queue = Queue::from_channels(jobs_tx, events_rx);
        let add = |queue: &mut Queue, key: &str| queue.enqueue(
            key.into(), key.into(), "1:00".into(), format!("https://example.org/{key}"), PathBuf::from(key)
        );
        assert!(add(&mut queue, "a"));
        assert!(!add(&mut queue, "a"));
        assert!(add(&mut queue, "b"));
        assert_eq!(jobs_rx.try_recv().unwrap().key, "a");
        assert_eq!(jobs_rx.try_recv().unwrap().key, "b");
        assert_eq!(queue.active_count(), 2);
        events_tx.send(Event::Started("a".into())).unwrap();
        events_tx.send(Event::Progress("a".into(), 40)).unwrap();
        events_tx.send(Event::Finished("a".into(), Err("network".into()))).unwrap();
        assert_eq!(queue.drain(), ["a"]);
        assert_eq!(queue.state("a"), Some(&State::Failed));
        assert!(add(&mut queue, "a"));
        assert_eq!(jobs_rx.try_recv().unwrap().key, "a");
        assert_eq!(queue.entries().iter().map(|entry| entry.key.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn progress_reader_reports_percent_without_publishing_partial_file() {
        let (tx, rx) = mpsc::channel();
        let mut reader = ProgressReader {
            inner: &b"abcd"[..], total: Some(4), read: 0, last_percent: 0,
            key: "a".into(), events: tx,
        };
        let mut buffer = [0; 2];
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        assert!(matches!(rx.try_recv(), Ok(Event::Progress(key, 50)) if key == "a"));
        assert!(matches!(rx.try_recv(), Ok(Event::Progress(key, 100)) if key == "a"));
    }

    #[test]
    fn worker_downloads_two_jobs_in_order_and_publishes_complete_files() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&stream).read_line(&mut line).unwrap();
                requests_tx.send(line.trim_end().to_string()).unwrap();
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata").unwrap();
            }
        });
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("hoki-podcast-queue-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let mut queue = Queue::start();
        for key in ["first", "second"] {
            assert!(queue.enqueue(key.into(), key.into(), "1:00".into(),
                format!("http://{address}/{key}"), directory.join(format!("{key}.mp3"))));
        }
        for _ in 0..500 {
            queue.drain();
            if queue.active_count() == 0 { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.state("first"), Some(&State::Ready));
        assert_eq!(queue.state("second"), Some(&State::Ready));
        assert_eq!(requests_rx.recv().unwrap(), "GET /first HTTP/1.1");
        assert_eq!(requests_rx.recv().unwrap(), "GET /second HTTP/1.1");
        assert_eq!(std::fs::read(directory.join("first.mp3")).unwrap(), b"data");
        assert_eq!(std::fs::read(directory.join("second.mp3")).unwrap(), b"data");
        server.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
