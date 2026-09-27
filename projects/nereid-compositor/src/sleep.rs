//! Keep coordinator IPC outside the Wayland/input thread.
use crate::sleep_client::Client;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
pub struct Bridge {
    shared: Arc<Mutex<(Value, Value)>>,
}
impl Bridge {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new((
            json!({"command":"ui","idle":0.,"foreground":true,
            "display":"interactive","generation":0,"manual_off":false}),
            json!({}),
        )));
        #[cfg(not(test))]
        let worker = shared.clone();
        #[cfg(not(test))]
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
                drop(state);
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        Self { shared }
    }
    #[cfg(test)]
    pub fn set_reply_for_test(&self, reply: Value) {
        self.shared.lock().unwrap().1 = reply;
    }

    pub fn exchange(&self, snapshot: Value) -> Value {
        let mut state = self.shared.lock().unwrap();
        let mut reply = state.1.clone();
        let previous = &reply["_request"];
        if previous["activity_revision"] != snapshot["activity_revision"]
            || previous["foreground"] != snapshot["foreground"]
            || snapshot["idle"].as_f64().unwrap_or(0.)
                < previous["idle"].as_f64().unwrap_or(f64::INFINITY)
            || previous["manual_off"] != snapshot["manual_off"]
        {
            // Discard stale policy without undoing the user's latest display intent.
            // In particular, manual screen-off must not flash on until the next reply.
            reply["display"] = json!(if snapshot["manual_off"] == true { "off" } else { "interactive" });
        }
        state.0 = snapshot;
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_and_foreground_changes_discard_old_idle_decisions() {
        let bridge = Bridge::new();
        bridge.shared.lock().unwrap().1 = json!({"display":"ambient","config":{"enabled":true},
            "_request":{"idle":40.,"foreground":false,"manual_off":false}});
        assert_eq!(
            bridge.exchange(json!({"idle":0.,"foreground":false,"manual_off":false}))["display"],
            "interactive"
        );
        assert_eq!(
            bridge.exchange(json!({"idle":42.,"foreground":true,"manual_off":false}))["display"],
            "interactive"
        );
        assert_eq!(
            bridge.exchange(json!({"idle":42.,"foreground":false,"manual_off":false}))["display"],
            "ambient"
        );
    }
    #[test]
    fn stale_replies_preserve_manual_off_and_physical_wake_intent() {
        let bridge = Bridge::new();
        for old_display in ["interactive", "ambient", "off"] {
            bridge.set_reply_for_test(json!({"display":old_display,
                "_request":{"idle":40.,"foreground":false,"manual_off":false}}));
            assert_eq!(bridge.exchange(json!({"idle":0.,"foreground":false,"manual_off":true}))["display"], "off");
        }
        bridge.set_reply_for_test(json!({"display":"off",
            "_request":{"idle":40.,"foreground":false,"manual_off":true}}));
        assert_eq!(bridge.exchange(json!({"idle":0.,"foreground":false,"manual_off":false}))["display"], "interactive");
        // A fresh coordinator decision, e.g. a display inhibitor, still applies.
        bridge.set_reply_for_test(json!({"display":"interactive",
            "_request":{"idle":0.,"foreground":false,"manual_off":true}}));
        assert_eq!(bridge.exchange(json!({"idle":1.,"foreground":false,"manual_off":true}))["display"], "interactive");
    }

    #[test]
    fn old_reply_never_becomes_current_again_as_idle_advances() {
        let bridge = Bridge::new();
        bridge.set_reply_for_test(json!({"display":"ambient",
            "_request":{"activity_revision":7,"idle":0.2,"foreground":false,"manual_off":false}}));
        for idle in [0., 1.1, 30., 300.] {
            assert_eq!(bridge.exchange(json!({"activity_revision":8,"idle":idle,
                "foreground":false,"manual_off":false}))["display"], "interactive");
        }
        bridge.set_reply_for_test(json!({"display":"ambient",
            "_request":{"activity_revision":8,"idle":0.2,"foreground":false,"manual_off":false}}));
        assert_eq!(bridge.exchange(json!({"activity_revision":8,"idle":1.1,
            "foreground":false,"manual_off":false}))["display"], "ambient");
    }

}
