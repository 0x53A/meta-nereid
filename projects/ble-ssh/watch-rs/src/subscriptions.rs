// Pending GATT setup belongs to one peer. A different peer or duplicate half
// is rejected without replacing any resource belonging to the first client.
pub struct Pending<P, N, S> {
    peer: Option<P>,
    notifier: Option<N>,
    start: Option<S>,
}
impl<P: Copy + Eq, N, S> Pending<P, N, S> {
    pub fn new() -> Self {
        Self {
            peer: None,
            notifier: None,
            start: None,
        }
    }
    pub fn occupied(&self) -> bool {
        self.peer.is_some()
    }
    pub fn notifier(&self) -> Option<&N> {
        self.notifier.as_ref()
    }
    pub fn start(&self) -> Option<&S> {
        self.start.as_ref()
    }
    pub fn offer_notifier(&mut self, peer: P, notifier: N) -> Result<(), N> {
        if self.peer.is_some_and(|owner| owner != peer) || self.notifier.is_some() {
            return Err(notifier);
        }
        self.peer = Some(peer);
        self.notifier = Some(notifier);
        Ok(())
    }
    pub fn offer_start(&mut self, peer: P, start: S) -> Result<(), S> {
        if self.peer.is_some_and(|owner| owner != peer) || self.start.is_some() {
            return Err(start);
        }
        self.peer = Some(peer);
        self.start = Some(start);
        Ok(())
    }
    pub fn take_ready(&mut self) -> Option<(N, S)> {
        if self.notifier.is_none() || self.start.is_none() {
            return None;
        }
        self.peer = None;
        Some((self.notifier.take().unwrap(), self.start.take().unwrap()))
    }
    pub fn clear(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn another_peer_cannot_replace_either_half_of_pending_session() {
        let mut pending = Pending::new();
        pending.offer_notifier(1, "first-writer").unwrap();
        assert_eq!(
            pending.offer_notifier(2, "second-writer"),
            Err("second-writer")
        );
        assert_eq!(pending.offer_start(2, "second-start"), Err("second-start"));
        assert!(pending.take_ready().is_none());
        pending.offer_start(1, "first-start").unwrap();
        assert_eq!(pending.take_ready(), Some(("first-writer", "first-start")));
        assert!(!pending.occupied());
    }
    #[test]
    fn start_first_and_canceled_setup_allow_clean_reconnection() {
        let mut pending = Pending::new();
        pending.offer_start(1, "old-start").unwrap();
        assert!(pending.offer_notifier(2, "wrong-peer").is_err());
        pending.clear();
        pending.offer_start(2, "new-start").unwrap();
        pending.offer_notifier(2, "new-writer").unwrap();
        assert_eq!(pending.offer_notifier(2, "duplicate"), Err("duplicate"));
        assert_eq!(pending.take_ready(), Some(("new-writer", "new-start")));
    }
}
