// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
//! Replaceable Slint front end for the compositor-managed lock-screen role.
//!
//! This process collects a PIN and delegates authentication to `nereid-auth`.
//! It has no unlock command and never reports PIN contents.

use nereid_auth::{Client, Outcome};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::RefCell,
    io::{BufRead, BufReader},
    ptr::NonNull,
    rc::Rc,
    slice,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

slint::include_modules!();

const MIN_PIN_LEN: usize = 4;
const MAX_PIN_LEN: usize = 12;
const STATE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// A page-aligned, mlocked PIN buffer. The mapping moves by ownership to the
/// worker during submission, so UI and worker never share plaintext storage.
struct LockedPin {
    base: NonNull<u8>,
    map_len: usize,
    current_len: usize,
    secondary_len: usize,
    saved_current_len: usize,
}

// SAFETY: ownership is exclusive and moved through the channel; no raw pointer
// is shared with the UI while the worker uses this mapping.
unsafe impl Send for LockedPin {}

impl LockedPin {
    fn new() -> Option<Self> {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        let map_len = page_size as usize;
        // A dedicated mapping avoids sharing a locked page with unrelated heap
        // allocations that could be unlocked by a separate munlock call.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return None;
        }
        if unsafe { libc::mlock(raw, map_len) } != 0 {
            unsafe { libc::munmap(raw, map_len) };
            return None;
        }
        let Some(base) = NonNull::new(raw.cast()) else {
            unsafe {
                libc::munlock(raw, map_len);
                libc::munmap(raw, map_len);
            }
            return None;
        };
        Some(Self {
            base,
            map_len,
            current_len: 0,
            secondary_len: 0,
            saved_current_len: 0,
        })
    }

    fn current(&self) -> &[u8] {
        // SAFETY: the leading bytes are initialized through `push` and bounded
        // by `current_len`.
        unsafe { slice::from_raw_parts(self.base.as_ptr(), self.current_len) }
    }

    fn secondary(&self) -> &[u8] {
        // SAFETY: this slot starts after the current slot and is bounded by
        // `secondary_len`.
        unsafe { slice::from_raw_parts(self.base.as_ptr().add(MAX_PIN_LEN), self.secondary_len) }
    }

    fn saved_current(&self) -> &[u8] {
        // SAFETY: this slot starts after the secondary slot and is bounded by
        // `saved_current_len`.
        unsafe {
            slice::from_raw_parts(
                self.base.as_ptr().add(MAX_PIN_LEN * 2),
                self.saved_current_len,
            )
        }
    }

    fn push(&mut self, byte: u8) {
        if self.current_len < MAX_PIN_LEN {
            unsafe { std::ptr::write_volatile(self.base.as_ptr().add(self.current_len), byte) };
            self.current_len += 1;
        }
    }

    fn pop(&mut self) {
        if let Some(index) = self.current_len.checked_sub(1) {
            unsafe { std::ptr::write_volatile(self.base.as_ptr().add(index), 0) };
            self.current_len = index;
        }
    }

    fn clear_current(&mut self) {
        wipe(self.current_mut());
        self.current_len = 0;
    }

    fn clear_secondary(&mut self) {
        // SAFETY: the secondary bytes are within this mapped page.
        let bytes = unsafe { slice::from_raw_parts_mut(self.base.as_ptr().add(MAX_PIN_LEN), self.secondary_len) };
        wipe(bytes);
        self.secondary_len = 0;
    }

    fn clear_saved_current(&mut self) {
        // SAFETY: the saved current PIN bytes are within this mapped page.
        let bytes = unsafe {
            slice::from_raw_parts_mut(
                self.base.as_ptr().add(MAX_PIN_LEN * 2),
                self.saved_current_len,
            )
        };
        wipe(bytes);
        self.saved_current_len = 0;
    }

    fn clear_all(&mut self) {
        self.clear_current();
        self.clear_secondary();
        self.clear_saved_current();
    }

    fn current_mut(&mut self) -> &mut [u8] {
        // SAFETY: this exclusive borrow covers only the initialized current slot.
        unsafe { slice::from_raw_parts_mut(self.base.as_ptr(), self.current_len) }
    }

    fn copy_current_to_secondary(&mut self) {
        self.clear_secondary();
        self.secondary_len = self.current_len;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.base.as_ptr(),
                self.base.as_ptr().add(MAX_PIN_LEN),
                self.current_len,
            );
        }
    }

    fn copy_current_to_saved_current(&mut self) {
        self.clear_saved_current();
        self.saved_current_len = self.current_len;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.base.as_ptr(),
                self.base.as_ptr().add(MAX_PIN_LEN * 2),
                self.current_len,
            );
        }
    }

    fn matches_secondary(&self) -> bool {
        constant_time_equal(self.current(), self.secondary())
    }

    fn current_len(&self) -> usize {
        self.current_len
    }
}

impl Drop for LockedPin {
    fn drop(&mut self) {
        // Wipe the entire locked page before unlocking or returning it to the
        // kernel, including bytes outside the active PIN slots.
        let page = unsafe { slice::from_raw_parts_mut(self.base.as_ptr(), self.map_len) };
        wipe(page);
        unsafe {
            libc::munlock(self.base.as_ptr().cast(), self.map_len);
            libc::munmap(self.base.as_ptr().cast(), self.map_len);
        }
    }
}

/// Use volatile writes so an optimizer cannot remove the best-effort wipe.
fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: `byte` is a valid, uniquely borrowed byte in this slice.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Checking,
    Enroll,
    Confirm,
    Verify,
    ManagementMenu,
    ChangeCurrent,
    ChangeNew,
    ConfirmNew,
    ClearCurrent,
    ClearConfirmation,
    ManagementDone,
    Authenticated,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchMode {
    ManagedLock,
    VerificationApp,
    PinManagementApp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Enrollment,
    Verification,
    Change,
    Clear,
}

struct ScreenState {
    mode: LaunchMode,
    phase: Phase,
    pins: Option<LockedPin>,
    protection_ready: bool,
    request: Option<RequestKind>,
    operation_faulted: bool,
    service_busy: bool,
    management_ready: bool,
    management_enrolled: bool,
    retry_until: Option<Instant>,
    feedback: Option<&'static str>,
    preview_pin_count: Option<usize>,
    pin_cleared: bool,
    exit_requested: bool,
}

impl ScreenState {
    fn new(process_protected: bool, mode: LaunchMode) -> Self {
        let pins = LockedPin::new();
        let protection_ready = process_protected && pins.is_some();
        Self {
            mode,
            phase: if protection_ready {
                Phase::Checking
            } else {
                Phase::Unavailable
            },
            pins,
            protection_ready,
            request: None,
            operation_faulted: false,
            service_busy: false,
            management_ready: false,
            management_enrolled: false,
            retry_until: None,
            feedback: None,
            preview_pin_count: None,
            pin_cleared: false,
            exit_requested: false,
        }
    }

    fn clear_inputs(&mut self) {
        if let Some(pins) = self.pins.as_mut() {
            pins.clear_all();
        }
    }

    fn clear_active_entry(&mut self) {
        if let Some(pins) = self.pins.as_mut() {
            pins.clear_current();
        }
    }

    fn pending(&self, now: Instant) -> bool {
        self.request.is_some()
            || self.service_busy
            || self.retry_until.is_some_and(|until| now < until)
    }

    fn accepts_input(&self, now: Instant) -> bool {
        self.protection_ready
            && !self.operation_faulted
            && !self.pending(now)
            && matches!(
                self.phase,
                Phase::Enroll
                    | Phase::Confirm
                    | Phase::Verify
                    | Phase::ChangeCurrent
                    | Phase::ChangeNew
                    | Phase::ConfirmNew
                    | Phase::ClearCurrent
            )
    }

    fn pin_len(&self) -> usize {
        self.preview_pin_count
            .unwrap_or_else(|| self.pins.as_ref().map_or(0, LockedPin::current_len))
    }
}

fn management_destination(state: &ScreenState) -> Phase {
    if !state.management_ready {
        Phase::Unavailable
    } else if state.management_enrolled {
        Phase::ManagementMenu
    } else {
        Phase::Enroll
    }
}

fn leave_management(state: &mut ScreenState) {
    if state.mode != LaunchMode::PinManagementApp || state.request.is_some() {
        return;
    }
    if matches!(
        state.phase,
        Phase::ManagementMenu | Phase::ManagementDone | Phase::Enroll | Phase::Confirm
    ) || !state.management_ready
    {
        state.clear_inputs();
        state.exit_requested = true;
    } else {
        state.clear_inputs();
        state.phase = management_destination(state);
        state.feedback = None;
    }
}

enum WorkerCommand {
    Refresh,
    Submit { pin: LockedPin, kind: RequestKind },
    Stop,
}

enum WorkerEvent {
    State {
        enrolled: bool,
        locked: bool,
        busy: bool,
    },
    StateUnavailable,
    Attempt {
        kind: RequestKind,
        outcome: Result<Outcome, ()>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process_protected = protect_process();
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--preview") {
        let mode = args.get(index + 1).map(String::as_str).unwrap_or("verify");
        let capture = args
            .iter()
            .position(|arg| arg == "--capture")
            .and_then(|index| args.get(index + 1))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "--preview requires --capture <path>",
                )
            })?;
        return run_preview(mode, capture, process_protected);
    }
    // PIN management is available only to an ordinary app explicitly launched
    // from Settings. Ordinary direct launches retain verification behavior.
    let mode = if std::env::var_os("HOKI_MANAGED_ROLE").is_some() {
        LaunchMode::ManagedLock
    } else if args
        .iter()
        .any(|arg| arg == "--manage-pin" || arg == "--enroll")
    {
        LaunchMode::PinManagementApp
    } else {
        LaunchMode::VerificationApp
    };
    let window = LockWindow::new()?;
    let state = Rc::new(RefCell::new(ScreenState::new(process_protected, mode)));
    let (command_tx, command_rx) = mpsc::sync_channel(4);
    let (event_tx, event_rx) = mpsc::sync_channel(16);
    if !spawn_auth_worker(command_rx, event_tx) {
        let mut state = state.borrow_mut();
        state.phase = Phase::Unavailable;
        state.feedback = Some("Authentication worker unavailable.");
    }
    install_ui_callbacks(&window, state.clone(), command_tx.clone());
    if mode == LaunchMode::ManagedLock && !start_role_stdin(window.as_weak(), command_tx.clone()) {
        let mut state = state.borrow_mut();
        state.protection_ready = false;
        state.phase = Phase::Unavailable;
        state.feedback = Some("Managed lock-screen input unavailable.");
    }

    let weak = window.as_weak();
    let tick_state = state.clone();
    let event_timer = Timer::default();
    event_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        drain_worker_events(&event_rx, &tick_state);
        if tick_state.borrow().exit_requested {
            let _ = slint::quit_event_loop();
            return;
        }
        if let Some(window) = weak.upgrade() {
            let state = tick_state.borrow();
            render(&window, &state, Instant::now());
        }
    });

    render(&window, &state.borrow(), Instant::now());
    window.run()?;
    let _ = command_tx.try_send(WorkerCommand::Stop);
    Ok(())
}

fn install_ui_callbacks(
    window: &LockWindow,
    state: Rc<RefCell<ScreenState>>,
    command_tx: SyncSender<WorkerCommand>,
) {
    let weak = window.as_weak();
    let digit_state = state.clone();
    window.on_digit(move |digit| {
        let Some(&byte) = digit.as_bytes().first().filter(|_| digit.len() == 1) else {
            return;
        };
        if !byte.is_ascii_digit() {
            return;
        }
        let mut state = digit_state.borrow_mut();
        if state.accepts_input(Instant::now()) {
            if let Some(pins) = state.pins.as_mut() {
                pins.push(byte);
            }
            state.feedback = None;
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let delete_state = state.clone();
    window.on_delete_digit(move || {
        let mut state = delete_state.borrow_mut();
        if state.accepts_input(Instant::now()) {
            if let Some(pins) = state.pins.as_mut() {
                pins.pop();
            }
            state.feedback = None;
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let clear_state = state.clone();
    window.on_clear_entry(move || {
        let mut state = clear_state.borrow_mut();
        if state.mode == LaunchMode::PinManagementApp {
            state.clear_active_entry();
        } else {
            state.clear_inputs();
        }
        state.feedback = None;
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let change_state = state.clone();
    window.on_change_pin(move || {
        let mut state = change_state.borrow_mut();
        if state.mode == LaunchMode::PinManagementApp
            && state.management_ready
            && state.management_enrolled
            && state.phase == Phase::ManagementMenu
            && !state.pending(Instant::now())
        {
            state.clear_inputs();
            state.phase = Phase::ChangeCurrent;
            state.feedback = None;
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let clear_state = state.clone();
    window.on_clear_pin(move || {
        let mut state = clear_state.borrow_mut();
        if state.mode == LaunchMode::PinManagementApp
            && state.management_ready
            && state.management_enrolled
            && state.phase == Phase::ManagementMenu
            && !state.pending(Instant::now())
        {
            state.clear_inputs();
            state.phase = Phase::ClearCurrent;
            state.feedback = None;
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let confirm_clear_state = state.clone();
    let confirm_clear_tx = command_tx.clone();
    window.on_confirm_clear(move || {
        let mut state = confirm_clear_state.borrow_mut();
        if state.mode == LaunchMode::PinManagementApp
            && state.management_ready
            && state.management_enrolled
            && state.phase == Phase::ClearConfirmation
            && !state.pending(Instant::now())
        {
            start_request(&confirm_clear_tx, &mut state, RequestKind::Clear);
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let cancel_clear_state = state.clone();
    window.on_cancel_clear(move || {
        let mut state = cancel_clear_state.borrow_mut();
        if state.mode == LaunchMode::PinManagementApp
            && state.phase == Phase::ClearConfirmation
            && state.request.is_none()
        {
            state.clear_inputs();
            state.phase = management_destination(&state);
            state.feedback = None;
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let cancel_state = state.clone();
    window.on_cancel_management(move || {
        let mut state = cancel_state.borrow_mut();
        leave_management(&mut state);
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });

    let weak = window.as_weak();
    let finish_state = state.clone();
    let finish_tx = command_tx.clone();
    window.on_finish_entry(move || {
        let mut state = finish_state.borrow_mut();
        if state.phase == Phase::ManagementDone && state.request.is_none() {
            state.exit_requested = true;
            return;
        }
        if state.phase == Phase::Unavailable && state.protection_ready && !state.operation_faulted {
            state.feedback = Some("Checking authentication service.");
            let _ = finish_tx.try_send(WorkerCommand::Refresh);
            state.phase = Phase::Checking;
            state.management_ready = false;
            if let Some(window) = weak.upgrade() {
                render(&window, &state, Instant::now());
            }
            return;
        }
        if !state.accepts_input(Instant::now())
            || !(MIN_PIN_LEN..=MAX_PIN_LEN)
                .contains(&state.pins.as_ref().map_or(0, LockedPin::current_len))
        {
            return;
        }
        match state.phase {
            Phase::Enroll => {
                if let Some(pins) = state.pins.as_mut() {
                    pins.copy_current_to_secondary();
                    pins.clear_current();
                }
                state.phase = Phase::Confirm;
                state.feedback = None;
            }
            Phase::Confirm => {
                if state.pins.as_ref().is_some_and(LockedPin::matches_secondary) {
                    start_request(&finish_tx, &mut state, RequestKind::Enrollment);
                } else {
                    state.clear_inputs();
                    state.phase = Phase::Enroll;
                    state.feedback = Some("PINs did not match. Start again.");
                }
            }
            Phase::ChangeCurrent => {
                if let Some(pins) = state.pins.as_mut() {
                    pins.copy_current_to_saved_current();
                    pins.clear_current();
                }
                state.phase = Phase::ChangeNew;
                state.feedback = Some("Enter a new PIN.");
            }
            Phase::ChangeNew => {
                if let Some(pins) = state.pins.as_mut() {
                    pins.copy_current_to_secondary();
                    pins.clear_current();
                }
                state.phase = Phase::ConfirmNew;
                state.feedback = Some("Enter the new PIN again.");
            }
            Phase::ConfirmNew => {
                if state.pins.as_ref().is_some_and(LockedPin::matches_secondary) {
                    if let Some(pins) = state.pins.as_mut() { pins.clear_current(); }
                    start_request(&finish_tx, &mut state, RequestKind::Change);
                } else {
                    state.clear_inputs();
                    state.phase = Phase::ChangeCurrent;
                    state.feedback = Some("PINs did not match. Enter your current PIN again.");
                }
            }
            Phase::ClearCurrent => {
                if let Some(pins) = state.pins.as_mut() {
                    pins.copy_current_to_saved_current();
                    pins.clear_current();
                }
                state.phase = Phase::ClearConfirmation;
                state.feedback = None;
            }
            Phase::Verify => {
                start_request(&finish_tx, &mut state, RequestKind::Verification);
            }
            _ => {}
        }
        if let Some(window) = weak.upgrade() {
            render(&window, &state, Instant::now());
        }
    });
}

fn start_request(
    command_tx: &SyncSender<WorkerCommand>,
    state: &mut ScreenState,
    kind: RequestKind,
) {
    if matches!(kind, RequestKind::Change | RequestKind::Clear)
        && (!state.management_ready || !state.management_enrolled)
    {
        state.clear_inputs();
        state.phase = management_destination(state);
        state.feedback = Some("PIN management needs an enrolled, unlocked session.");
        return;
    }
    let Some(replacement) = LockedPin::new() else {
        state.clear_inputs();
        state.protection_ready = false;
        state.phase = Phase::Unavailable;
        state.feedback = Some("Secure PIN memory unavailable.");
        return;
    };
    let Some(pin) = state.pins.replace(replacement) else {
        state.protection_ready = false;
        state.phase = Phase::Unavailable;
        state.feedback = Some("Secure PIN memory unavailable.");
        return;
    };
    state.request = Some(kind);
    state.service_busy = false;
    state.retry_until = None;
    state.feedback = Some("Checking PIN…");
    match command_tx.try_send(WorkerCommand::Submit { pin, kind }) {
        Ok(()) => {}
        Err(TrySendError::Full(WorkerCommand::Submit { pin: rejected, .. }))
        | Err(TrySendError::Disconnected(WorkerCommand::Submit { pin: rejected, .. })) => {
            drop(rejected);
            state.request = None;
            state.operation_faulted = true;
            state.phase = Phase::Unavailable;
            state.feedback = Some("Authentication service unavailable.");
        }
        Err(TrySendError::Full(WorkerCommand::Refresh | WorkerCommand::Stop))
        | Err(TrySendError::Disconnected(WorkerCommand::Refresh | WorkerCommand::Stop)) => {
            state.request = None;
            state.operation_faulted = true;
            state.phase = Phase::Unavailable;
            state.feedback = Some("Authentication service unavailable.");
        }
    }
}

fn spawn_auth_worker(
    command_rx: Receiver<WorkerCommand>,
    event_tx: SyncSender<WorkerEvent>,
) -> bool {
    thread::Builder::new()
        .name("nereid-auth-client".into())
        .spawn(move || auth_worker(command_rx, event_tx))
        .is_ok()
}

fn auth_worker(command_rx: Receiver<WorkerCommand>, event_tx: SyncSender<WorkerEvent>) {
    let mut client = Client::system().ok();
    let mut next_state_poll = Instant::now();
    loop {
        let wait = next_state_poll
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250));
        match command_rx.recv_timeout(wait) {
            Ok(WorkerCommand::Stop) => break,
            Ok(WorkerCommand::Refresh) => next_state_poll = Instant::now(),
            Ok(WorkerCommand::Submit { mut pin, kind }) => {
                let outcome = match client.as_ref() {
                    Some(client) => match kind {
                        RequestKind::Enrollment | RequestKind::Verification => {
                            client.begin_attempt().and_then(|attempt| match kind {
                                RequestKind::Enrollment => client.enroll_pin(attempt, pin.current()),
                                RequestKind::Verification => client.submit_pin(attempt, pin.current()),
                                _ => unreachable!(),
                            })
                        }
                        RequestKind::Change | RequestKind::Clear => client
                            .state()
                            .and_then(|service_state| {
                                if !service_state.enrolled || service_state.locked || service_state.busy {
                                    return Err("PIN management requires an enrolled, unlocked session".into());
                                }
                                client.begin_management()
                            })
                            .and_then(|attempt| match kind {
                                RequestKind::Change => client.change_pin(
                                    attempt,
                                    pin.saved_current(),
                                    pin.secondary(),
                                ),
                                RequestKind::Clear => {
                                    client.clear_pin(attempt, pin.saved_current())
                                }
                                _ => unreachable!(),
                            }),
                    }
                    None => Err("Authentication service unavailable".into()),
                }
                .map_err(|_| ());
                pin.clear_all();
                if event_tx
                    .send(WorkerEvent::Attempt { kind, outcome })
                    .is_err()
                {
                    break;
                }
                next_state_poll = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if Instant::now() >= next_state_poll {
            if client.is_none() {
                client = Client::system().ok();
            }
            let event = match client.as_ref().and_then(|client| client.state().ok()) {
                Some(state) => WorkerEvent::State {
                    enrolled: state.enrolled,
                    locked: state.locked,
                    busy: state.busy,
                },
                None => {
                    client = None;
                    WorkerEvent::StateUnavailable
                }
            };
            if event_tx.send(event).is_err() {
                break;
            }
            next_state_poll = Instant::now() + STATE_POLL_INTERVAL;
        }
    }
}

fn drain_worker_events(event_rx: &Receiver<WorkerEvent>, state: &Rc<RefCell<ScreenState>>) {
    while let Ok(event) = event_rx.try_recv() {
        let mut state = state.borrow_mut();
        match event {
            WorkerEvent::State {
                enrolled,
                locked,
                busy,
            } => {
                state.service_busy = busy;
                if state.mode == LaunchMode::PinManagementApp {
                    if locked {
                        state.management_ready = false;
                        state.management_enrolled = false;
                        if state.request.is_none() && state.phase != Phase::ManagementDone {
                            state.clear_inputs();
                            state.phase = Phase::Unavailable;
                            state.feedback = Some("PIN management needs an unlocked session.");
                        }
                        continue;
                    }
                    if state.operation_faulted {
                        state.management_ready = false;
                        state.management_enrolled = false;
                        state.phase = Phase::Unavailable;
                        state.feedback = Some("Authentication service unavailable.");
                        continue;
                    }
                    if !state.protection_ready {
                        state.management_ready = false;
                        state.management_enrolled = false;
                        state.phase = Phase::Unavailable;
                        state.feedback = Some("Secure PIN memory unavailable.");
                        continue;
                    }
                    state.management_ready = true;
                    state.management_enrolled = enrolled;
                    if state.phase == Phase::Checking && state.request.is_none() {
                        state.clear_inputs();
                        state.phase = if enrolled {
                            Phase::ManagementMenu
                        } else {
                            Phase::Enroll
                        };
                        state.feedback = None;
                    } else if state.request.is_none()
                        && enrolled
                        && matches!(state.phase, Phase::Enroll | Phase::Confirm)
                    {
                        state.clear_inputs();
                        state.phase = Phase::ManagementMenu;
                        state.feedback = Some("A PIN was set in another session.");
                    } else if state.request.is_none() && !enrolled && state.phase == Phase::ManagementMenu {
                        state.clear_inputs();
                        state.phase = Phase::Enroll;
                        state.feedback = Some("No PIN is configured. Create one now.");
                    }
                    continue;
                }
                if state.mode != LaunchMode::PinManagementApp && !enrolled && !locked {
                    // This positive service reply is the only unlocked state
                    // that lets the normal renderer leave. Bus errors never do.
                    state.clear_inputs();
                    state.exit_requested = true;
                    continue;
                }
                if state.mode == LaunchMode::VerificationApp && enrolled && !locked {
                    state.clear_inputs();
                    state.exit_requested = true;
                    continue;
                }
                if state.operation_faulted {
                    state.phase = Phase::Unavailable;
                    state.feedback = Some("Authentication service unavailable.");
                    continue;
                }
                if !state.protection_ready {
                    state.phase = Phase::Unavailable;
                    state.feedback = Some("Secure PIN memory unavailable.");
                    continue;
                }
                if locked && state.phase == Phase::Authenticated && state.request.is_none() {
                    state.clear_inputs();
                    state.phase = Phase::Checking;
                    state.feedback = None;
                }
                if enrolled {
                    if state.request.is_none()
                        && matches!(
                            state.phase,
                            Phase::Checking | Phase::Enroll | Phase::Confirm | Phase::Unavailable
                        )
                    {
                        state.clear_inputs();
                        state.phase = Phase::Verify;
                        state.feedback = None;
                    }
                } else if !enrolled && locked && state.request.is_none() {
                    state.clear_inputs();
                    state.phase = Phase::Unavailable;
                    state.feedback = Some("No PIN is configured while the device is locked.");
                }
                if enrolled && !locked && state.request.is_none() {
                    state.clear_inputs();
                    state.phase = Phase::Authenticated;
                    state.feedback = Some("Authenticated. Waiting for compositor.");
                }
            }
            WorkerEvent::StateUnavailable => {
                if state.mode == LaunchMode::PinManagementApp {
                    state.management_ready = false;
                    state.management_enrolled = false;
                }
                if state.request.is_none() {
                    state.phase = Phase::Unavailable;
                    state.service_busy = false;
                    state.clear_inputs();
                    state.feedback = Some(if state.protection_ready {
                        "Authentication service unavailable."
                    } else {
                        "Secure PIN memory unavailable."
                    });
                }
            }
            WorkerEvent::Attempt { kind, outcome } => {
                state.request = None;
                state.service_busy = false;
                state.clear_inputs();
                state.retry_until = None;
                match outcome {
                    Ok(Outcome::Unlocked) if kind == RequestKind::Verification => {
                        state.phase = Phase::Authenticated;
                        if state.mode == LaunchMode::VerificationApp {
                            state.exit_requested = true;
                            state.feedback = Some("Authenticated. Returning.");
                        } else {
                            state.feedback = Some("Authenticated. Waiting for compositor.");
                        }
                    }
                    Ok(Outcome::Enrolled) if kind == RequestKind::Enrollment => {
                        state.phase = Phase::Authenticated;
                        state.feedback = Some("PIN saved. Returning to Settings.");
                        state.exit_requested = true;
                    }
                    Ok(Outcome::PinChanged) if kind == RequestKind::Change => {
                        state.management_ready = true;
                        state.management_enrolled = true;
                        state.pin_cleared = false;
                        state.phase = Phase::ManagementDone;
                        state.feedback = Some("PIN changed successfully.");
                    }
                    Ok(Outcome::PinCleared) if kind == RequestKind::Clear => {
                        state.management_ready = true;
                        state.management_enrolled = false;
                        state.pin_cleared = true;
                        state.phase = Phase::ManagementDone;
                        state.feedback = Some("PIN cleared. Screen locking is disabled.");
                    }
                    Ok(Outcome::StorageProtected) if kind == RequestKind::Clear => {
                        state.management_ready = true;
                        state.management_enrolled = true;
                        state.phase = Phase::ManagementMenu;
                        state.feedback = Some("PIN cannot be cleared while encrypted storage exists.");
                    }
                    Ok(
                        Outcome::Unlocked
                        | Outcome::Enrolled
                        | Outcome::PinChanged
                        | Outcome::PinCleared
                        | Outcome::StorageProtected,
                    ) => {
                        state.operation_faulted = true;
                        state.phase = Phase::Unavailable;
                        state.feedback = Some("Unexpected authentication result.");
                    }
                    Ok(Outcome::Rejected) => {
                        state.phase = match kind {
                            RequestKind::Enrollment => Phase::Enroll,
                            RequestKind::Verification => Phase::Verify,
                            RequestKind::Change => Phase::ChangeCurrent,
                            RequestKind::Clear => Phase::ClearCurrent,
                        };
                        state.retry_until = Some(Instant::now() + Duration::from_secs(2));
                        state.feedback = Some(if kind == RequestKind::Enrollment {
                            "Could not create PIN. Wait before retrying."
                        } else {
                            "PIN not accepted. Please wait."
                        });
                    }
                    Ok(Outcome::Retry { after_ms }) => {
                        state.phase = match kind {
                            RequestKind::Enrollment => Phase::Enroll,
                            RequestKind::Verification => Phase::Verify,
                            RequestKind::Change => Phase::ChangeCurrent,
                            RequestKind::Clear => Phase::ClearCurrent,
                        };
                        state.retry_until =
                            Some(Instant::now() + Duration::from_millis(u64::from(after_ms)));
                        state.feedback = Some("Please wait before trying again.");
                    }
                    Ok(Outcome::Unavailable) | Err(()) => {
                        state.operation_faulted = true;
                        state.phase = Phase::Unavailable;
                        state.feedback = Some("Authentication service unavailable.");
                    }
                }
            }
        }
    }
}

fn render(window: &LockWindow, state: &ScreenState, now: Instant) {
    let (headline, explanation, action_label) = match state.phase {
        Phase::Checking => (
            "Checking lock",
            "Checking authentication service",
            "Continue",
        ),
        Phase::Enroll => ("Create PIN", "Choose 6 digits (4–12 allowed)", "Next"),
        Phase::Confirm => ("Confirm PIN", "Enter the same PIN again", "Save"),
        Phase::Verify => ("Enter PIN", "Enter your PIN to unlock", "Unlock"),
        Phase::ManagementMenu => ("PIN Management", "Choose an action.", "Done"),
        Phase::ChangeCurrent => ("Current PIN", "Enter the PIN you use now.", "Next"),
        Phase::ChangeNew => ("New PIN", "Choose 6 digits (4–12 allowed)", "Next"),
        Phase::ConfirmNew => ("Confirm new PIN", "Enter the new PIN again.", "Save"),
        Phase::ClearCurrent => ("Clear PIN", "Enter your current PIN.", "Next"),
        Phase::ClearConfirmation => (
            "Confirm clear",
            "Screen locking will be disabled.",
            "Continue",
        ),
        Phase::ManagementDone if state.pin_cleared => (
            "PIN cleared",
            "Screen locking is disabled.",
            "Back",
        ),
        Phase::ManagementDone => (
            "PIN changed",
            "Your new PIN is ready.",
            "Back",
        ),
        Phase::Authenticated => ("Unlocked", "Ready", "Done"),
        Phase::Unavailable => ("Locked", "Unlock is temporarily unavailable", "Retry"),
    };
    let is_retrying = state.retry_until.is_some_and(|until| now < until);
    let status = if state.phase == Phase::ManagementDone {
        String::new()
    } else if state.phase == Phase::Unavailable {
        if state.operation_faulted || !state.protection_ready {
            "Recovery required".to_owned()
        } else {
            state.feedback.unwrap_or_default().to_owned()
        }
    } else if let Some(until) = state.retry_until.filter(|until| now < *until) {
        let seconds = until
            .saturating_duration_since(now)
            .as_secs()
            .saturating_add(1);
        format!("Try again in {seconds}s")
    } else if state.retry_until.is_some() {
        String::new()
    } else if state.service_busy && state.request.is_none() {
        "Authentication is busy".to_owned()
    } else {
        state.feedback.unwrap_or_default().to_owned()
    };
    let accepts_input = state.accepts_input(now) && !is_retrying;
    let pin_len = state.pin_len();
    let valid_length = (MIN_PIN_LEN..=MAX_PIN_LEN).contains(&pin_len);

    window.set_headline(headline.into());
    window.set_explanation(explanation.into());
    window.set_action_label(action_label.into());
    window.set_status(status.into());
    window.set_pin_length(pin_len as i32);
    window.set_keypad_enabled(accepts_input && pin_len < MAX_PIN_LEN);
    window.set_delete_enabled(accepts_input && pin_len > 0);
    window.set_action_enabled(
        (accepts_input && valid_length)
            || (state.phase == Phase::Unavailable
                && state.protection_ready
                && !state.operation_faulted
                && !state.service_busy),
    );
    window.set_pending(state.request.is_some() || state.service_busy);
    window.set_authenticated(state.phase == Phase::Authenticated);
    window.set_management_mode(state.mode == LaunchMode::PinManagementApp);
    window.set_management_menu(state.phase == Phase::ManagementMenu);
    window.set_clear_confirmation(state.phase == Phase::ClearConfirmation);
    window.set_management_done(state.phase == Phase::ManagementDone);
        window.set_management_enabled(
            state.mode == LaunchMode::PinManagementApp
            && state.management_ready
            && state.management_enrolled
            && !state.operation_faulted
            && !state.pending(now),
    );
    window.set_cancel_enabled(state.mode == LaunchMode::PinManagementApp && state.request.is_none());
}

fn start_role_stdin(
    window: slint::Weak<LockWindow>,
    command_tx: SyncSender<WorkerCommand>,
) -> bool {
    thread::Builder::new()
        .name("lock-role-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let reader = BufReader::new(stdin.lock());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                match line.as_str() {
                    "visibility:visible" => {
                        let _ = command_tx.try_send(WorkerCommand::Refresh);
                    }
                    "visibility:hidden" => {
                        let weak = window.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = weak.upgrade() {
                                window.invoke_clear_entry();
                            }
                        });
                    }
                    _ => {}
                }
            }
            let _ = slint::invoke_from_event_loop(|| {
                let _ = slint::quit_event_loop();
            });
        })
        .is_ok()
}

fn protect_process() -> bool {
    // Disable ptrace/core-dump collection before constructing any UI state or
    // spawning workers. This does not protect against root or all crash paths.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0 }
}

/// Render static states without constructing an authentication client. This
/// mode can capture UI only; it cannot submit a PIN or report unlock success.
fn run_preview(
    mode: &str,
    capture: &str,
    process_protected: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let window = LockWindow::new()?;
    let launch_mode = if matches!(
        mode,
        "enroll"
            | "confirm"
            | "already-set"
            | "manage"
            | "change-current"
            | "change-new"
            | "confirm-new"
            | "clear-current"
            | "clear-confirm"
            | "clear-protected"
            | "changed"
            | "cleared"
    ) {
        LaunchMode::PinManagementApp
    } else {
        LaunchMode::ManagedLock
    };
    let mut state = ScreenState::new(process_protected, launch_mode);
    match mode {
        "verify" => {
            state.phase = Phase::Verify;
            state.preview_pin_count = Some(6);
        }
        "enroll" => state.phase = Phase::Enroll,
        "already-set" | "manage" => {
            state.phase = Phase::ManagementMenu;
            state.management_ready = true;
        }
        "confirm" => {
            state.phase = Phase::Confirm;
            state.preview_pin_count = Some(6);
        }
        "pending" => {
            state.phase = Phase::Verify;
            state.preview_pin_count = Some(6);
            state.request = Some(RequestKind::Verification);
            state.feedback = Some("Checking PIN…");
        }
        "failure" => {
            state.phase = Phase::Verify;
            state.preview_pin_count = Some(6);
            state.retry_until = Some(Instant::now() + Duration::from_secs(2));
            state.feedback = Some("PIN not accepted. Please wait.");
        }
        "retry" => {
            state.phase = Phase::Verify;
            state.retry_until = Some(Instant::now() + Duration::from_secs(3));
            state.feedback = Some("Please wait before trying again.");
        }
        "unavailable" => {
            state.phase = Phase::Unavailable;
            state.feedback = Some("Authentication service unavailable.");
        }
        "change-current" => {
            state.phase = Phase::ChangeCurrent;
            state.management_ready = true;
        }
        "change-new" => {
            state.phase = Phase::ChangeNew;
            state.management_ready = true;
            state.preview_pin_count = Some(6);
        }
        "confirm-new" => {
            state.phase = Phase::ConfirmNew;
            state.management_ready = true;
            state.preview_pin_count = Some(6);
        }
        "clear-current" => {
            state.phase = Phase::ClearCurrent;
            state.management_ready = true;
        }
        "clear-confirm" => {
            state.phase = Phase::ClearConfirmation;
            state.management_ready = true;
        }
        "clear-protected" => {
            state.phase = Phase::ManagementMenu;
            state.management_ready = true;
            state.feedback = Some("PIN cannot be cleared while encrypted storage exists.");
        }
        "changed" => {
            state.phase = Phase::ManagementDone;
            state.management_ready = true;
            state.feedback = Some("PIN changed successfully.");
        }
        "cleared" => {
            state.phase = Phase::ManagementDone;
            state.management_ready = true;
            state.pin_cleared = true;
            state.feedback = Some("PIN cleared. Screen locking is disabled.");
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unknown preview state: {mode}"),
            )
            .into());
        }
    }
    if launch_mode == LaunchMode::PinManagementApp {
        state.management_ready = true;
        state.management_enrolled = !matches!(mode, "enroll" | "confirm" | "cleared");
    }
    render(&window, &state, Instant::now());

    let weak = window.as_weak();
    let path = capture.to_owned();
    let capture_timer = Timer::default();
    capture_timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(500),
        move || {
            if let Some(window) = weak.upgrade() {
                match window.window().take_snapshot() {
                    Ok(pixels) => {
                        let result = (|| -> std::io::Result<()> {
                            use std::io::Write;
                            let mut file = std::fs::File::create(&path)?;
                            write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                            for pixel in pixels.as_bytes().chunks_exact(4) {
                                file.write_all(&pixel[..3])?;
                            }
                            Ok(())
                        })();
                        if result.is_err() {
                            eprintln!("Preview capture failed.");
                        }
                    }
                    Err(_) => eprintln!("Preview capture failed."),
                }
            }
            let _ = slint::quit_event_loop();
        },
    );
    window.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        SyncSender<WorkerEvent>,
        Receiver<WorkerEvent>,
        Rc<RefCell<ScreenState>>,
    ) {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::ManagedLock)));
        assert!(state.borrow().protection_ready);
        (tx, rx, state)
    }

    #[test]
    fn relocking_restores_pin_entry() {
        let (tx, rx, state) = fixture();
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert!(state.borrow().phase == Phase::Authenticated);
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: true,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert!(state.borrow().phase == Phase::Verify);
        assert!(state.borrow().accepts_input(Instant::now()));
    }

    #[test]
    fn service_loss_clears_entry_and_disables_input() {
        let (tx, rx, state) = fixture();
        state.borrow_mut().pins.as_mut().unwrap().push(b'7');
        tx.send(WorkerEvent::StateUnavailable).unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().pin_len(), 0);
        assert!(!state.borrow().accepts_input(Instant::now()));
        assert_eq!(state.borrow().phase, Phase::Unavailable);
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn polling_cannot_clear_an_operation_failure() {
        let (tx, rx, state) = fixture();
        tx.send(WorkerEvent::Attempt {
            kind: RequestKind::Verification,
            outcome: Ok(Outcome::Unavailable),
        })
        .unwrap();
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: true,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert!(state.borrow().operation_faulted);
        assert!(state.borrow().phase == Phase::Unavailable);
        assert!(!state.borrow().accepts_input(Instant::now()));
    }

    #[test]
    fn trusted_unenrolled_unlocked_state_exits_even_without_pin_memory() {
        for mode in [LaunchMode::ManagedLock, LaunchMode::VerificationApp] {
            let (tx, rx) = mpsc::sync_channel(16);
            let state = Rc::new(RefCell::new(ScreenState::new(false, mode)));
            tx.send(WorkerEvent::State {
                enrolled: false,
                locked: false,
                busy: false,
            })
            .unwrap();
            drain_worker_events(&rx, &state);
            assert!(state.borrow().exit_requested);
        }
    }

    #[test]
    fn managed_role_never_offers_enrollment_for_unenrolled_locked_state() {
        for mode in [LaunchMode::ManagedLock, LaunchMode::VerificationApp] {
            let (tx, rx) = mpsc::sync_channel(16);
            let state = Rc::new(RefCell::new(ScreenState::new(true, mode)));
            tx.send(WorkerEvent::State {
                enrolled: false,
                locked: true,
                busy: false,
            })
            .unwrap();
            drain_worker_events(&rx, &state);
            assert_eq!(state.borrow().mode, mode);
            assert_eq!(state.borrow().phase, Phase::Unavailable);
            assert!(!state.borrow().accepts_input(Instant::now()));
            assert!(!state.borrow().exit_requested);
        }
    }

    #[test]
    fn management_app_offers_create_pin_only_for_healthy_unenrolled_unlocked_state() {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::PinManagementApp)));
        tx.send(WorkerEvent::State {
            enrolled: false,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::Enroll);
        assert!(state.borrow().accepts_input(Instant::now()));
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn back_from_create_pin_returns_to_settings_instead_of_an_empty_management_menu() {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::PinManagementApp)));
        tx.send(WorkerEvent::State {
            enrolled: false,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::Enroll);
        leave_management(&mut state.borrow_mut());
        assert!(state.borrow().exit_requested);
    }

    #[test]
    fn first_enrollment_keeps_confirmed_entry_in_submission_slot() {
        let mut pin = LockedPin::new().unwrap();
        for byte in [b'4', b'2', b'7', b'1', b'9'] {
            pin.push(byte);
        }
        pin.copy_current_to_secondary();
        assert!(pin.matches_secondary());
        assert_eq!(pin.current(), pin.secondary());
        pin.clear_all();
        assert!(pin.current().is_empty());
        assert!(pin.secondary().is_empty());
    }

    #[test]
    fn clearing_active_change_entry_preserves_saved_pin_and_phase() {
        let (_, _, state) = fixture_management();
        {
            let mut state = state.borrow_mut();
            let pins = state.pins.as_mut().unwrap();
            for byte in [b'2', b'6', b'8', b'4'] {
                pins.push(byte);
            }
            pins.copy_current_to_saved_current();
            pins.clear_current();
            for byte in [b'7', b'5', b'3', b'1'] {
                pins.push(byte);
            }
            pins.copy_current_to_secondary();
            pins.clear_current();
            pins.push(b'9');
            state.phase = Phase::ConfirmNew;
            state.clear_active_entry();
        }
        let state = state.borrow();
        assert_eq!(state.phase, Phase::ConfirmNew);
        let pins = state.pins.as_ref().unwrap();
        assert!(pins.current().is_empty());
        assert_eq!(pins.saved_current().len(), 4);
        assert_eq!(pins.secondary().len(), 4);
    }

    #[test]
    fn external_enrollment_changes_create_and_management_destinations() {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::PinManagementApp)));
        tx.send(WorkerEvent::State {
            enrolled: false,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::Enroll);
        state.borrow_mut().pins.as_mut().unwrap().push(b'7');
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::ManagementMenu);
        assert_eq!(state.borrow().pin_len(), 0);

        tx.send(WorkerEvent::State {
            enrolled: false,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::Enroll);
        assert!(!state.borrow().management_enrolled);
    }

    #[test]
    fn enrolled_management_starts_on_menu_and_source_updates_preserve_entry_phase() {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::PinManagementApp)));
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::ManagementMenu);
        assert!(state.borrow().management_ready);

        {
            let mut state = state.borrow_mut();
            state.phase = Phase::ChangeNew;
            state.pins.as_mut().unwrap().push(b'8');
        }
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::ChangeNew);
        assert_eq!(state.borrow().pin_len(), 1);
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn successful_first_enrollment_returns_to_settings_without_an_extra_verification() {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::PinManagementApp)));
        {
            let mut state = state.borrow_mut();
            state.phase = Phase::Confirm;
            state.request = Some(RequestKind::Enrollment);
        }
        tx.send(WorkerEvent::Attempt {
            kind: RequestKind::Enrollment,
            outcome: Ok(Outcome::Enrolled),
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert!(state.borrow().exit_requested);
        assert_eq!(state.borrow().phase, Phase::Authenticated);
    }

    #[test]
    fn management_state_errors_never_offer_create_pin() {
        let (tx, rx, state) = fixture_management();
        tx.send(WorkerEvent::StateUnavailable).unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::Unavailable);
        assert!(!state.borrow().management_ready);
        assert!(!state.borrow().accepts_input(Instant::now()));
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn locked_management_session_fails_closed_without_exiting() {
        let (tx, rx, state) = fixture_management();
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: true,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::Unavailable);
        assert!(!state.borrow().management_ready);
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn clear_blocked_by_encrypted_storage_returns_to_menu_with_plain_message() {
        let (tx, rx, state) = fixture_management();
        {
            let mut state = state.borrow_mut();
            state.phase = Phase::ClearConfirmation;
            state.request = Some(RequestKind::Clear);
        }
        tx.send(WorkerEvent::Attempt {
            kind: RequestKind::Clear,
            outcome: Ok(Outcome::StorageProtected),
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::ManagementMenu);
        assert_eq!(
            state.borrow().feedback,
            Some("PIN cannot be cleared while encrypted storage exists.")
        );
        assert!(state.borrow().management_ready);
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn successful_management_shows_result_and_polling_does_not_reset_it() {
        for (kind, outcome, cleared) in [
            (RequestKind::Change, Outcome::PinChanged, false),
            (RequestKind::Clear, Outcome::PinCleared, true),
        ] {
            let (tx, rx, state) = fixture_management();
            {
                let mut state = state.borrow_mut();
                state.phase = Phase::ChangeCurrent;
                state.request = Some(kind);
            }
            tx.send(WorkerEvent::Attempt {
                kind,
                outcome: Ok(outcome),
            })
            .unwrap();
            tx.send(WorkerEvent::State {
                enrolled: !cleared,
                locked: false,
                busy: false,
            })
            .unwrap();
            drain_worker_events(&rx, &state);
            assert_eq!(state.borrow().phase, Phase::ManagementDone);
            assert_eq!(state.borrow().pin_cleared, cleared);
            assert!(!state.borrow().exit_requested);
            assert!(state.borrow().management_ready);
        }
    }

    #[test]
    fn rejected_management_attempt_waits_for_manual_retry() {
        let (tx, rx, state) = fixture_management();
        {
            let mut state = state.borrow_mut();
            state.phase = Phase::ChangeCurrent;
            state.request = Some(RequestKind::Change);
        }
        tx.send(WorkerEvent::Attempt {
            kind: RequestKind::Change,
            outcome: Ok(Outcome::Rejected),
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::ChangeCurrent);
        assert!(state.borrow().retry_until.is_some());
        assert!(!state.borrow().accepts_input(Instant::now()));
        assert!(!state.borrow().exit_requested);
    }

    #[test]
    fn management_menu_and_clear_confirmation_use_real_headless_pointer_input() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{PointerEventButton, WindowEvent};

        struct TestPlatform(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for TestPlatform {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }

        let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
        renderer.set_size(slint::PhysicalSize::new(416, 416));
        let window = LockWindow::new().unwrap();
        window.set_management_mode(true);
        window.set_management_menu(true);
        window.set_management_enabled(true);
        window.set_cancel_enabled(true);
        let actions = Rc::new(RefCell::new(Vec::new()));
        let seen = actions.clone();
        window.on_change_pin(move || seen.borrow_mut().push("change"));
        let seen = actions.clone();
        window.on_clear_pin(move || seen.borrow_mut().push("clear"));
        let seen = actions.clone();
        window.on_confirm_clear(move || seen.borrow_mut().push("confirm-clear"));
        let seen = actions.clone();
        window.on_cancel_clear(move || seen.borrow_mut().push("cancel-clear"));
        window.show().unwrap();

        let draw = || {
            slint::platform::update_timers_and_animations();
            let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
            window.window().request_redraw();
            renderer.draw_if_needed(|r| {
                r.render(&mut pixels, 416);
            });
            pixels
        };
        let tap = |x: f32, y: f32| {
            let position = slint::LogicalPosition::new(x, y);
            draw();
            window.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };

        assert_eq!(draw().len(), 416 * 416);
        tap(208.0, 155.0);
        tap(208.0, 227.0);
        assert_eq!(*actions.borrow(), vec!["change", "clear"]);

        window.set_management_menu(false);
        window.set_clear_confirmation(true);
        draw();
        tap(208.0, 238.0);
        tap(208.0, 308.0);
        assert_eq!(
            *actions.borrow(),
            vec!["change", "clear", "confirm-clear", "cancel-clear"]
        );
    }

    fn fixture_management() -> (
        SyncSender<WorkerEvent>,
        Receiver<WorkerEvent>,
        Rc<RefCell<ScreenState>>,
    ) {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(true, LaunchMode::PinManagementApp)));
        tx.send(WorkerEvent::State {
            enrolled: true,
            locked: false,
            busy: false,
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert_eq!(state.borrow().phase, Phase::ManagementMenu);
        (tx, rx, state)
    }

    #[test]
    fn ordinary_verification_app_exits_after_a_successful_verification() {
        let (tx, rx) = mpsc::sync_channel(16);
        let state = Rc::new(RefCell::new(ScreenState::new(
            true,
            LaunchMode::VerificationApp,
        )));
        {
            let mut state = state.borrow_mut();
            state.phase = Phase::Verify;
            state.request = Some(RequestKind::Verification);
        }
        tx.send(WorkerEvent::Attempt {
            kind: RequestKind::Verification,
            outcome: Ok(Outcome::Unlocked),
        })
        .unwrap();
        drain_worker_events(&rx, &state);
        assert!(state.borrow().exit_requested);
    }
}
