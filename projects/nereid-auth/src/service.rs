use crate::{backend::Backend, protocol::Exchange, Outcome};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zbus::{interface, message::Header, object_server::SignalEmitter};

struct State {
    exchange: Exchange,
    enrolled: bool,
    locked: bool,
    busy: bool,
    closing: bool,
    fault: bool,
    cooldown: Instant,
    generation: u64,
}
pub struct AuthService {
    state: Mutex<State>,
    backend: Arc<dyn Backend>,
    transaction: tokio::sync::Mutex<()>,
}
fn failed() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed("Authentication unavailable".into())
}
fn sender(header: &Header<'_>) -> zbus::fdo::Result<String> {
    header.sender().map(ToString::to_string).ok_or_else(failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Cleanup(AtomicBool);
    struct UnknownEnrollment;
    impl Backend for UnknownEnrollment {
        fn enrolled(&self) -> Result<bool, crate::Error> {
            Err("Unreadable credential state".into())
        }
        fn authenticate(&self, _: &[u8]) -> Result<Outcome, crate::Error> {
            panic!("unknown enrollment must not authenticate")
        }
    }
    #[test]
    fn unreadable_enrollment_is_not_no_pin() {
        let service = AuthService::new(Arc::new(UnknownEnrollment));
        assert!(service.get_state().is_err());
        assert!(service.locked());
    }
    impl Backend for Cleanup {
        fn enrolled(&self) -> Result<bool, crate::Error> {
            Ok(true)
        }
        fn authenticate(&self, _: &[u8]) -> Result<Outcome, crate::Error> {
            panic!("shutdown must not authenticate")
        }
        fn lock(&self) -> Result<(), crate::Error> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn shutdown_blocks_submissions_before_waiting_for_transaction() {
        let backend = Arc::new(Cleanup(AtomicBool::new(false)));
        let service = Arc::new(AuthService::new(backend.clone()));
        let transaction = service.transaction.lock().await;
        let copy = service.clone();
        let shutdown = tokio::spawn(async move { copy.shutdown().await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !service.state.lock().unwrap().fault {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(service.state.lock().unwrap().locked);
        assert!(service.state.lock().unwrap().closing);
        assert!(!backend.0.load(Ordering::SeqCst));
        assert!(!shutdown.is_finished());
        drop(transaction);
        shutdown.await.unwrap().unwrap();
        assert!(backend.0.load(Ordering::SeqCst));
        assert!(service.state.lock().unwrap().fault);
    }
}
impl AuthService {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let enrollment = backend.enrolled();
        Self {
            state: Mutex::new(State {
                exchange: Exchange::default(),
                enrolled: enrollment.as_ref().copied().unwrap_or(false),
                locked: enrollment.as_ref().copied().unwrap_or(true),
                busy: false,
                closing: false,
                fault: enrollment.is_err(),
                cooldown: Instant::now(),
                generation: 0,
            }),
            backend,
            transaction: tokio::sync::Mutex::new(()),
        }
    }
    pub fn expire(&self) {
        self.state.lock().unwrap().exchange.expire(Instant::now());
    }
    pub async fn shutdown(&self) -> Result<(), crate::Error> {
        {
            let mut s = self.state.lock().unwrap();
            s.fault = true;
            s.closing = true;
            s.locked = true;
            s.generation = s.generation.wrapping_add(1);
            s.exchange.clear();
        }
        // Keep the public interface alive until the entire pending request has
        // completed. The native mutex alone cannot order a queued worker.
        let _transaction = self.transaction.lock().await;
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || backend.lock()).await?
    }
    async fn submit(
        &self,
        id: Vec<u8>,
        ciphertext: Vec<u8>,
        header: Header<'_>,
        emitter: SignalEmitter<'_>,
        enrollment: bool,
    ) -> zbus::fdo::Result<(String, u32)> {
        let owner = sender(&header)?;
        let _transaction = self.transaction.lock().await;
        let (pin, generation) = {
            let mut s = self.state.lock().unwrap();
            let now = Instant::now();
            if s.busy || s.closing || s.fault {
                return Ok(("unavailable".into(), 0));
            }
            // Even invalid ciphertext consumes a matching attempt's private key.
            let pin = s
                .exchange
                .open(&owner, &id, &ciphertext, now)
                .map_err(|_| failed())?;
            if enrollment == s.enrolled || (!enrollment && !s.locked) {
                return Ok(("unavailable".into(), 0));
            }
            if now < s.cooldown {
                return Ok(("retry".into(), (s.cooldown - now).as_millis() as u32));
            }
            s.busy = true;
            (pin, s.generation)
        };
        let _ = self.busy_changed(&emitter).await;
        let backend = self.backend.clone();
        let result = tokio::task::spawn_blocking(move || {
            if enrollment {
                backend.enroll(&pin)
            } else {
                backend.authenticate(&pin)
            }
        })
        .await;
        let reply = {
            let mut s = self.state.lock().unwrap();
            s.busy = false;
            match result {
                Ok(Ok(Outcome::Unlocked)) if !enrollment && generation == s.generation => {
                    s.locked = false;
                    ("unlocked", 0)
                }
                Ok(Ok(Outcome::Enrolled)) if enrollment => {
                    s.enrolled = true;
                    ("enrolled", 0)
                }
                Ok(Ok(Outcome::Rejected)) => {
                    s.cooldown = Instant::now() + Duration::from_secs(2);
                    ("rejected", 2000)
                }
                Ok(Ok(Outcome::Retry { after_ms })) => {
                    s.cooldown = Instant::now() + Duration::from_millis(after_ms as u64);
                    ("retry", after_ms)
                }
                Ok(Ok(Outcome::Unlocked)) => ("unavailable", 0), // Lock superseded the pending request.
                _ => {
                    s.fault = true;
                    s.locked = true;
                    ("unavailable", 0)
                }
            }
        };
        let _ = self.locked_changed(&emitter).await;
        let _ = self.enrolled_changed(&emitter).await;
        let _ = self.busy_changed(&emitter).await;
        Ok((reply.0.into(), reply.1))
    }
}
#[interface(name = "io.Nereid.Auth1")]
impl AuthService {
    fn get_state(&self) -> zbus::fdo::Result<(bool, bool, bool)> {
        let s = self.state.lock().unwrap();
        if s.fault {
            return Err(failed());
        }
        Ok((s.enrolled, s.locked, s.busy || s.closing))
    }
    #[zbus(property)]
    fn locked(&self) -> bool {
        self.state.lock().unwrap().locked
    }
    #[zbus(property)]
    fn enrolled(&self) -> bool {
        self.state.lock().unwrap().enrolled
    }
    #[zbus(property)]
    fn busy(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.busy || s.closing
    }
    fn begin_attempt(
        &self,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(Vec<u8>, Vec<u8>)> {
        let owner = sender(&header)?;
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        if s.busy || s.closing || s.fault || (s.enrolled && !s.locked) || now < s.cooldown {
            return Err(failed());
        }
        s.exchange.begin(&owner, now).map_err(|_| failed())
    }
    fn begin_management(
        &self,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(Vec<u8>, Vec<u8>)> {
        let owner = sender(&header)?;
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        if s.busy || s.closing || s.fault || s.locked || !s.enrolled || now < s.cooldown {
            return Err(failed());
        }
        s.exchange
            .begin_management(&owner, now)
            .map_err(|_| failed())
    }
    async fn manage_pin(
        &self,
        id: Vec<u8>,
        ciphertext: Vec<u8>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(String, u32)> {
        let owner = sender(&header)?;
        let _transaction = self.transaction.lock().await;
        let request = {
            let mut s = self.state.lock().unwrap();
            let now = Instant::now();
            if s.busy || s.closing || s.fault {
                return Ok(("unavailable".into(), 0));
            }
            let request = s
                .exchange
                .open_management(&owner, &id, &ciphertext, now)
                .map_err(|_| failed())?;
            if s.locked || !s.enrolled {
                return Ok(("unavailable".into(), 0));
            }
            if now < s.cooldown {
                return Ok(("retry".into(), (s.cooldown - now).as_millis() as u32));
            }
            s.busy = true;
            request
        };
        let changing = request.new_pin.is_some();
        let _ = self.busy_changed(&emitter).await;
        let backend = self.backend.clone();
        let result = tokio::task::spawn_blocking(move || match request.new_pin.as_ref() {
            Some(new) => backend.change_pin(&request.current, new),
            None => backend.clear_pin(&request.current),
        })
        .await;
        let reply = {
            let mut s = self.state.lock().unwrap();
            s.busy = false;
            match result {
                Ok(Ok(Outcome::PinChanged)) if changing => ("pin-changed", 0),
                Ok(Ok(Outcome::PinCleared)) if !changing => {
                    s.enrolled = false;
                    if !s.closing && !s.fault {
                        s.locked = false;
                    }
                    s.exchange.clear();
                    ("pin-cleared", 0)
                }
                Ok(Ok(Outcome::StorageProtected)) if !changing => ("storage-protected", 0),
                Ok(Ok(Outcome::Rejected)) => {
                    s.cooldown = Instant::now() + Duration::from_secs(2);
                    ("rejected", 2000)
                }
                _ => {
                    s.fault = true;
                    s.locked = true;
                    ("unavailable", 0)
                }
            }
        };
        let _ = self.locked_changed(&emitter).await;
        let _ = self.enrolled_changed(&emitter).await;
        let _ = self.busy_changed(&emitter).await;
        Ok((reply.0.into(), reply.1))
    }
    async fn submit_pin(
        &self,
        id: Vec<u8>,
        ciphertext: Vec<u8>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(String, u32)> {
        self.submit(id, ciphertext, header, emitter, false).await
    }
    async fn submit_enrollment(
        &self,
        id: Vec<u8>,
        ciphertext: Vec<u8>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(String, u32)> {
        self.submit(id, ciphertext, header, emitter, true).await
    }
    async fn lock(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        {
            let mut s = self.state.lock().unwrap();
            if s.closing {
                return Err(failed());
            }
            s.closing = true;
            s.locked = s.enrolled || s.fault;
            s.generation = s.generation.wrapping_add(1);
            s.exchange.clear();
        }
        // Screen locks immediately. Storage cleanup waits for an in-flight
        // authentication and prevents any new submission until it completes.
        let _ = self.locked_changed(&emitter).await;
        let _ = self.busy_changed(&emitter).await;
        let _transaction = self.transaction.lock().await;
        let backend = self.backend.clone();
        let result = tokio::task::spawn_blocking(move || backend.lock()).await;
        let success = matches!(result, Ok(Ok(())));
        {
            let mut s = self.state.lock().unwrap();
            s.closing = false;
            if !success {
                s.fault = true;
            }
            s.locked = s.enrolled || s.fault;
        }
        let _ = self.locked_changed(&emitter).await;
        let _ = self.busy_changed(&emitter).await;
        if success {
            Ok(())
        } else {
            Err(failed())
        }
    }
}
