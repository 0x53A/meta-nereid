//! Fail-closed monitor for system Auth1 credential and lock state.
//!
//! The property is read only after resolving the well-known name to its current
//! unique owner. The property request targets that unique owner and the name
//! owner is checked again before accepting the result, so an owner replacement
//! cannot supply a stale read.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};

use crate::{CtlMessage, wakeup};

const SERVICE: &str = "io.Nereid.Auth1";
const PATH: &str = "/io/Nereid/Auth1";
const INTERFACE: &str = "io.Nereid.Auth1";
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthState {
    /// Unique D-Bus owner that supplied the verified property, if present.
    pub owner: Option<String>,
    /// Whether the current service owner reports an enrolled PIN.
    pub enrolled: bool,
    /// Whether the current service owner reports the device locked.
    pub locked: bool,
}

impl AuthState {
    /// A verified no-PIN state is authoritative even on the first sample from
    /// a new service owner. A contradictory `(enrolled=false, locked=true)`
    /// sample remains locked.
    fn known_no_pin(&self) -> bool {
        self.owner.is_some() && !self.enrolled && !self.locked
    }

    fn fail_closed() -> Self {
        Self {
            owner: None,
            enrolled: true,
            locked: true,
        }
    }
}

/// Poll Auth1 without blocking the compositor event loop. When a service owner
/// first appears or changes, require one further stable sample before accepting
/// an unlocked enrolled state. A verified no-PIN state is already safe to
/// accept because it explicitly reports both `enrolled=false` and `locked=false`.
pub fn start_monitor(
    tx: mpsc::Sender<CtlMessage>,
    wake: Arc<wakeup::Wakeup>,
    active: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("auth1-lock-monitor".into())
        .spawn(move || {
            let mut last_owner: Option<String> = None;
            let mut last_sent: Option<AuthState> = None;
            let mut connection: Option<Connection> = None;
            let mut consecutive_errors = 0u8;
            loop {
                if !active.load(Ordering::Acquire) {
                    connection = None;
                    last_owner = None;
                    last_sent = None;
                    consecutive_errors = 0;
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
                if connection.is_none() {
                    connection = zbus::blocking::connection::Builder::system()
                        .and_then(|builder| builder.method_timeout(Duration::from_millis(500)).build())
                        .ok();
                }
                let result = connection.as_ref().map(read_auth_state);
                let observed = match result {
                    Some(Ok(state)) => {
                        consecutive_errors = 0;
                        state
                    }
                    Some(Err(error)) => {
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        if consecutive_errors >= 4 {
                            connection = None;
                            consecutive_errors = 0;
                        }
                        if last_sent.as_ref().is_none_or(|last| last.owner.is_some()) {
                            tracing::warn!(%error, "Auth1 state unavailable; keeping compositor locked");
                        }
                        AuthState::fail_closed()
                    }
                    None => AuthState::fail_closed(),
                };
                let state = stabilize_owner_state(observed, &last_owner);
                let next_owner = state.owner.clone();
                last_owner = next_owner;
                if last_sent.as_ref() != Some(&state) {
                    if tx.send(CtlMessage::AuthState(state.clone())).is_err() {
                        break;
                    }
                    wake.notify();
                    last_sent = Some(state);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .ok();
}

fn read_auth_state(connection: &Connection) -> zbus::Result<AuthState> {
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    let service: zbus::names::BusName<'static> = SERVICE.try_into()?;
    let owner = bus.get_name_owner(service.clone())?.to_string();

    // Address the unique owner rather than the well-known name. D-Bus system
    // policy controls who may own io.Nereid.Auth1; the compositor accepts no
    // renderer-originated unlock command or app-id claim.
    let (enrolled, locked) = {
        let auth = Proxy::new(connection, owner.as_str(), PATH, INTERFACE)?;
        let (enrolled, locked, _busy): (bool, bool, bool) = auth.call("GetState", &())?;
        (enrolled, locked)
    };
    let current_owner = bus.get_name_owner(service)?.to_string();
    if owner != current_owner {
        return Ok(AuthState::fail_closed());
    }

    Ok(AuthState {
        owner: Some(owner),
        enrolled,
        locked,
    })
}

fn stabilize_owner_state(observed: AuthState, last_owner: &Option<String>) -> AuthState {
    let owner_changed = observed.owner.is_some() && observed.owner != *last_owner;
    let known_no_pin = observed.known_no_pin();
    AuthState {
        owner: observed.owner,
        enrolled: observed.enrolled,
        locked: observed.locked || (owner_changed && !known_no_pin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_no_pin_is_unlocked_on_first_sample_from_owner() {
        let state = stabilize_owner_state(
            AuthState {
                owner: Some(":1.10".into()),
                enrolled: false,
                locked: false,
            },
            &None,
        );

        assert!(!state.locked);
        assert!(!state.enrolled);
    }

    #[test]
    fn enrolled_state_requires_stable_owner_before_accepting_unlocked() {
        let first = stabilize_owner_state(
            AuthState {
                owner: Some(":1.10".into()),
                enrolled: true,
                locked: false,
            },
            &None,
        );
        assert!(first.locked);

        let second = stabilize_owner_state(
            AuthState {
                owner: Some(":1.10".into()),
                enrolled: true,
                locked: false,
            },
            &first.owner,
        );
        assert!(!second.locked);
    }

    #[test]
    fn contradictory_no_enrollment_locked_state_stays_locked() {
        let state = stabilize_owner_state(
            AuthState {
                owner: Some(":1.10".into()),
                enrolled: false,
                locked: true,
            },
            &None,
        );

        assert!(state.locked);
    }
}
