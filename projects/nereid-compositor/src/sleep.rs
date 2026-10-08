//! Powerd supplies constraints and acknowledges readiness; it never chooses display state.
#[cfg(not(test))]
use crate::sleep_client::Client;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub struct Bridge {
    shared: Arc<Mutex<(Value, Value, Instant)>>,
}
impl Bridge {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new((
            json!({"command":"ui","display":"interactive","ready":false,"revision":0}),
            json!({}),
            Instant::now(),
        )));
        #[cfg(not(test))]
        {
            let worker = shared.clone();
            std::thread::spawn(move || {
                let mut client = None;
                loop {
                    if client.is_none() {
                        client = Client::connect().ok();
                    }
                    let snapshot = worker.lock().unwrap().0.clone();
                    let response = client
                        .as_mut()
                        .and_then(|c| c.request(snapshot.clone()).ok());
                    let mut state = worker.lock().unwrap();
                    state.1 = match response {
                        Some(mut v) => {
                            v["_request"] = snapshot;
                            v
                        }
                        None => {
                            client = None;
                            json!({})
                        }
                    };
                    state.2 = Instant::now();
                    drop(state);
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        }
        Self { shared }
    }
    #[cfg(test)]
    pub fn set_reply_for_test(&self, reply: Value) {
        let mut state = self.shared.lock().unwrap();
        state.1 = reply;
        state.2 = Instant::now();
    }
    pub fn exchange(&self, snapshot: Value) -> Value {
        let mut state = self.shared.lock().unwrap();
        let mut reply = state.1.clone();
        let fresh = state.2.elapsed() < Duration::from_secs(2) && reply["ok"] == true;
        reply["_acknowledged"] = json!(fresh && reply["_request"] == snapshot);
        reply["_available"] = json!(fresh);
        state.0 = snapshot;
        reply
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_matching_fresh_physical_readiness_is_acknowledged() {
        let bridge = Bridge::new();
        let off = json!({"command":"ui","display":"off","ready":true,"revision":7});
        bridge.set_reply_for_test(json!({"ok":true,"_request":off}));
        assert_eq!(bridge.exchange(off.clone())["_acknowledged"], true);
        let mut newer = off.clone();
        newer["revision"] = json!(8);
        assert_eq!(bridge.exchange(newer)["_acknowledged"], false);
        let mut awake = off.clone();
        awake["display"] = json!("interactive");
        awake["ready"] = json!(false);
        assert_eq!(bridge.exchange(awake)["_acknowledged"], false);
        bridge.shared.lock().unwrap().2 = Instant::now() - Duration::from_secs(3);
        assert_eq!(bridge.exchange(off)["_acknowledged"], false);
        bridge.set_reply_for_test(json!({}));
        assert_eq!(bridge.exchange(json!({}))["_available"], false);
    }
}
