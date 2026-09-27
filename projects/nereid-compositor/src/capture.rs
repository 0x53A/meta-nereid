//! Output capture into client SHM buffers. No encoder, transport, or FPS timer.
//!
//! The trusted local Wayland socket is the access boundary, as for other shell
//! globals. Capture never wakes the display or inhibits sleep. Sessions stop
//! when normal display rendering stops; clients can create a new session on wake.
use std::sync::{Arc, Mutex};

use smithay::output::Output;
use smithay::wayland::shm::{BufferData, with_buffer_contents_mut};
use wayland_protocols::ext::image_capture_source::v1::server::{
    ext_image_capture_source_v1 as source,
    ext_output_image_capture_source_manager_v1 as source_manager,
};
use wayland_protocols::ext::image_copy_capture::v1::server::{
    ext_image_copy_capture_cursor_session_v1 as cursor, ext_image_copy_capture_frame_v1 as frame,
    ext_image_copy_capture_manager_v1 as manager, ext_image_copy_capture_session_v1 as session,
};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
    backend::{ClientId, ObjectId},
    protocol::{wl_buffer::WlBuffer, wl_output, wl_shm},
};

pub type SourceManager = source_manager::ExtOutputImageCaptureSourceManagerV1;
pub type Source = source::ExtImageCaptureSourceV1;
pub type Manager = manager::ExtImageCopyCaptureManagerV1;
pub type Session = session::ExtImageCopyCaptureSessionV1;
pub type Frame = frame::ExtImageCopyCaptureFrameV1;
pub type Cursor = cursor::ExtImageCopyCaptureCursorSessionV1;
pub type SessionData = Arc<Mutex<SessionInner>>;
pub type FrameData = Mutex<FrameInner>;
pub type CursorData = Mutex<bool>;

pub trait CaptureHandler:
    GlobalDispatch<SourceManager, ()>
    + GlobalDispatch<Manager, ()>
    + Dispatch<SourceManager, ()>
    + Dispatch<Source, bool>
    + Dispatch<Manager, ()>
    + Dispatch<Session, SessionData>
    + Dispatch<Frame, FrameData>
    + Dispatch<Cursor, CursorData>
    + Sized
    + 'static
{
    fn capture_state(&mut self) -> &mut CaptureState;
}

#[derive(Default)]
pub struct SessionInner {
    stopped: bool,
    generation: u64,
    frame: Option<ObjectId>,
    last_sequence: Option<u64>,
}
pub struct FrameInner {
    session: SessionData,
    buffer: Option<WlBuffer>,
    submitted: bool,
}

pub struct CaptureState {
    output: Output,
    width: u32,
    height: u32,
    active: bool,
    generation: u64,
    sequence: u64,
    timestamp: Option<std::time::Duration>,
    sessions: Vec<Session>,
    pending: Vec<Frame>,
}

impl CaptureState {
    pub fn new<D: CaptureHandler>(
        dh: &DisplayHandle,
        output: Output,
        width: u32,
        height: u32,
    ) -> Self {
        dh.create_global::<D, SourceManager, _>(1, ());
        dh.create_global::<D, Manager, _>(1, ());
        Self {
            output,
            width,
            height,
            active: true,
            generation: 0,
            sequence: 0,
            timestamp: None,
            sessions: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub fn set_active(&mut self, active: bool) {
        if self.active && !active {
            self.generation = self.generation.wrapping_add(1);
        }
        self.active = active;
        if active {
            return;
        }
        self.timestamp = None;
        self.sessions.retain(Resource::is_alive);
        for session in &self.sessions {
            let mut data = session.data::<SessionData>().unwrap().lock().unwrap();
            if !data.stopped {
                data.stopped = true;
                session.stopped();
            }
        }
        // Also covers frames whose session object has already been destroyed.
        for frame in self.pending.drain(..) {
            frame
                .data::<FrameData>()
                .unwrap()
                .lock()
                .unwrap()
                .session
                .lock()
                .unwrap()
                .stopped = true;
            if frame.is_alive() {
                frame.failed(frame::FailureReason::Stopped);
            }
        }
    }

    /// Called only after successful HWC submission. Timestamp is monotonic
    /// submission time: the proxy does not expose hardware presentation fences.
    pub fn presented(&mut self) {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0 {
            self.sequence = self.sequence.wrapping_add(1);
            self.timestamp = Some(std::time::Duration::new(
                ts.tv_sec as u64,
                ts.tv_nsec as u32,
            ));
        }
    }

    /// Serve first captures from the latest frame, even on a static screen;
    /// subsequent captures wait for the next composed frame. Never wait on clients.
    pub fn copy_pending(&mut self, rgba: &[u8]) {
        let Some(timestamp) = self.timestamp else {
            return;
        };
        let sequence = self.sequence;
        let (width, height) = (self.width, self.height);
        self.pending.retain(|frame| {
            if !frame.is_alive() {
                return false;
            }
            let data = frame.data::<FrameData>().unwrap().lock().unwrap();
            let mut session = data.session.lock().unwrap();
            if session.stopped {
                frame.failed(frame::FailureReason::Stopped);
                return false;
            }
            if session.last_sequence == Some(sequence) {
                return true;
            }
            let result = data
                .buffer
                .as_ref()
                .filter(|b| b.is_alive())
                .ok_or(())
                .and_then(|buffer| {
                    with_buffer_contents_mut(buffer, |ptr, len, spec| {
                        // Raw writes only: a client can mutate its SHM concurrently.
                        unsafe { copy_rgba(rgba, width, height, ptr, len, spec) }
                    })
                    .map_err(|_| ())
                    .and_then(|r| r)
                });
            match result {
                Ok(()) => {
                    frame.transform(wl_output::Transform::Normal);
                    // Conservative full damage is valid and includes all client damage.
                    frame.damage(0, 0, width as i32, height as i32);
                    frame.presentation_time(
                        (timestamp.as_secs() >> 32) as u32,
                        timestamp.as_secs() as u32,
                        timestamp.subsec_nanos(),
                    );
                    frame.ready();
                    session.last_sequence = Some(sequence);
                }
                Err(()) => frame.failed(frame::FailureReason::BufferConstraints),
            }
            false
        });
    }

    fn new_session<D: CaptureHandler>(
        &mut self,
        id: New<Session>,
        init: &mut DataInit<'_, D>,
        valid: bool,
    ) {
        let stopped = !valid || !self.active;
        let session = init.init(
            id,
            Arc::new(Mutex::new(SessionInner {
                stopped,
                generation: self.generation,
                ..Default::default()
            })),
        );
        if stopped {
            session.stopped();
        } else {
            session.buffer_size(self.width, self.height);
            session.shm_format(wl_shm::Format::Xrgb8888);
            session.done();
        }
        self.sessions.retain(Resource::is_alive);
        self.sessions.push(session);
    }
}

/// Write B,G,R,255 bytes for native-endian XRGB8888. Both supported targets are
/// little endian. Validate all arithmetic before touching the client's mapping.
unsafe fn copy_rgba(
    src: &[u8],
    width: u32,
    height: u32,
    dst: *mut u8,
    len: usize,
    spec: BufferData,
) -> Result<(), ()> {
    if spec.format != wl_shm::Format::Xrgb8888
        || spec.width != width as i32
        || spec.height != height as i32
        || spec.offset < 0
        || spec.stride < 0
        || width == 0
        || height == 0
    {
        return Err(());
    }
    let row = (width as usize).checked_mul(4).ok_or(())?;
    let stride = spec.stride as usize;
    let offset = spec.offset as usize;
    let required = (height as usize - 1)
        .checked_mul(stride)
        .and_then(|v| v.checked_add(offset))
        .and_then(|v| v.checked_add(row))
        .ok_or(())?;
    if stride < row || required > len || src.len() < row.checked_mul(height as usize).ok_or(())? {
        return Err(());
    }
    for y in 0..height as usize {
        for x in 0..width as usize {
            let i = y * row + x * 4;
            let pixel = u32::from_ne_bytes([src[i + 2], src[i + 1], src[i], 255]);
            unsafe {
                dst.add(offset + y * stride + x * 4)
                    .cast::<u32>()
                    .write_unaligned(pixel);
            }
        }
    }
    Ok(())
}

impl<D: CaptureHandler> GlobalDispatch<SourceManager, (), D> for CaptureState {
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        id: New<SourceManager>,
        _: &(),
        init: &mut DataInit<'_, D>,
    ) {
        init.init(id, ());
    }
}
impl<D: CaptureHandler> GlobalDispatch<Manager, (), D> for CaptureState {
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        id: New<Manager>,
        _: &(),
        init: &mut DataInit<'_, D>,
    ) {
        init.init(id, ());
    }
}
impl<D: CaptureHandler> Dispatch<SourceManager, (), D> for CaptureState {
    fn request(
        state: &mut D,
        _: &Client,
        _: &SourceManager,
        req: source_manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, D>,
    ) {
        if let source_manager::Request::CreateSource { source, output } = req {
            let valid =
                Output::from_resource(&output).as_ref() == Some(&state.capture_state().output);
            init.init(source, valid);
        }
    }
}
impl<D: CaptureHandler> Dispatch<Source, bool, D> for CaptureState {
    fn request(
        _: &mut D,
        _: &Client,
        _: &Source,
        _: source::Request,
        _: &bool,
        _: &DisplayHandle,
        _: &mut DataInit<'_, D>,
    ) {
    }
}
impl<D: CaptureHandler> Dispatch<Manager, (), D> for CaptureState {
    fn request(
        state: &mut D,
        _: &Client,
        resource: &Manager,
        req: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, D>,
    ) {
        match req {
            manager::Request::CreateSession {
                session,
                source,
                options,
            } => {
                let bits = match options {
                    WEnum::Value(v) => v.bits(),
                    WEnum::Unknown(v) => v,
                };
                if bits & !manager::Options::PaintCursors.bits() != 0 {
                    resource.post_error(manager::Error::InvalidOption, "unknown capture option");
                    return;
                }
                // No software pointer cursor is rendered by this compositor.
                state.capture_state().new_session(
                    session,
                    init,
                    source.data::<bool>().copied().unwrap_or(false),
                );
            }
            manager::Request::CreatePointerCursorSession { session, .. } => {
                init.init(session, Mutex::new(false));
            }
            _ => {}
        }
    }
}
impl<D: CaptureHandler> Dispatch<Cursor, CursorData, D> for CaptureState {
    fn request(
        state: &mut D,
        _: &Client,
        resource: &Cursor,
        req: cursor::Request,
        data: &CursorData,
        _: &DisplayHandle,
        init: &mut DataInit<'_, D>,
    ) {
        if let cursor::Request::GetCaptureSession { session } = req {
            let mut created = data.lock().unwrap();
            if *created {
                resource.post_error(
                    cursor::Error::DuplicateSession,
                    "cursor capture session already created",
                );
                return;
            }
            *created = true;
            // Separate cursor capture is unavailable. Return a stopped session.
            state.capture_state().new_session(session, init, false);
        }
    }
}
impl<D: CaptureHandler> Dispatch<Session, SessionData, D> for CaptureState {
    fn request(
        _: &mut D,
        _: &Client,
        resource: &Session,
        req: session::Request,
        data: &SessionData,
        _: &DisplayHandle,
        init: &mut DataInit<'_, D>,
    ) {
        if let session::Request::CreateFrame { frame } = req {
            let mut session = data.lock().unwrap();
            if session.frame.is_some() {
                resource.post_error(
                    session::Error::DuplicateFrame,
                    "destroy previous frame first",
                );
                return;
            }
            let frame = init.init(
                frame,
                Mutex::new(FrameInner {
                    session: data.clone(),
                    buffer: None,
                    submitted: false,
                }),
            );
            session.frame = Some(frame.id());
        }
    }
    fn destroyed(state: &mut D, _: ClientId, resource: &Session, _: &SessionData) {
        state.capture_state().sessions.retain(|s| s != resource);
    }
}
impl<D: CaptureHandler> Dispatch<Frame, FrameData, D> for CaptureState {
    fn request(
        state: &mut D,
        _: &Client,
        resource: &Frame,
        req: frame::Request,
        data: &FrameData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, D>,
    ) {
        let mut data = data.lock().unwrap();
        if matches!(req, frame::Request::Destroy) {
            return;
        }
        if data.submitted {
            resource.post_error(frame::Error::AlreadyCaptured, "frame already submitted");
            return;
        }
        match req {
            frame::Request::AttachBuffer { buffer } => data.buffer = Some(buffer),
            frame::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    resource.post_error(
                        frame::Error::InvalidBufferDamage,
                        "invalid damage rectangle",
                    );
                }
            }
            frame::Request::Capture => {
                if data.buffer.is_none() {
                    resource.post_error(frame::Error::NoBuffer, "attach a buffer before capture");
                    return;
                }
                data.submitted = true;
                let session = data.session.lock().unwrap();
                let capture = state.capture_state();
                if session.stopped || session.generation != capture.generation || !capture.active {
                    resource.failed(frame::FailureReason::Stopped);
                } else {
                    state.capture_state().pending.push(resource.clone());
                }
            }
            _ => {}
        }
    }
    fn destroyed(state: &mut D, _: ClientId, resource: &Frame, data: &FrameData) {
        let data = data.lock().unwrap();
        data.session.lock().unwrap().frame = None;
        state.capture_state().pending.retain(|f| f != resource);
    }
}

macro_rules! delegate_capture {
    ($ty:ty) => {
        wayland_server::delegate_global_dispatch!($ty: [$crate::capture::SourceManager: ()] => $crate::capture::CaptureState);
        wayland_server::delegate_global_dispatch!($ty: [$crate::capture::Manager: ()] => $crate::capture::CaptureState);
        wayland_server::delegate_dispatch!($ty: [$crate::capture::SourceManager: ()] => $crate::capture::CaptureState);
        wayland_server::delegate_dispatch!($ty: [$crate::capture::Source: bool] => $crate::capture::CaptureState);
        wayland_server::delegate_dispatch!($ty: [$crate::capture::Manager: ()] => $crate::capture::CaptureState);
        wayland_server::delegate_dispatch!($ty: [$crate::capture::Session: $crate::capture::SessionData] => $crate::capture::CaptureState);
        wayland_server::delegate_dispatch!($ty: [$crate::capture::Frame: $crate::capture::FrameData] => $crate::capture::CaptureState);
        wayland_server::delegate_dispatch!($ty: [$crate::capture::Cursor: $crate::capture::CursorData] => $crate::capture::CaptureState);
    };
}
pub(crate) use delegate_capture;

#[cfg(test)]
mod tests;
