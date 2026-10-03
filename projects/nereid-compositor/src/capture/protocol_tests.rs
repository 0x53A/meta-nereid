//! Actual Wayland requests over a socket pair, using the compositor test harness.
use super::*;
use std::os::fd::BorrowedFd;
use wayland_client::protocol::wl_output;
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1 as source,
    ext_output_image_capture_source_manager_v1 as source_manager,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as manager,
    ext_image_copy_capture_session_v1 as session,
};

#[derive(Default)]
pub(super) struct ClientCapture {
    pub output: Option<wl_output::WlOutput>,
    pub source: Option<source_manager::ExtOutputImageCaptureSourceManagerV1>,
    pub manager: Option<manager::ExtImageCopyCaptureManagerV1>,
    pub(super) size: (u32, u32),
    constraints: Vec<&'static str>,
    pub(super) events: Vec<&'static str>,
    pub(super) failures: Vec<frame::FailureReason>,
    pub(super) stopped: usize,
    time: Option<(u32, u32, u32)>,
}
delegate_noop!(Client: ignore wl_output::WlOutput);
delegate_noop!(Client: ignore source_manager::ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Client: ignore source::ExtImageCaptureSourceV1);
delegate_noop!(Client: ignore manager::ExtImageCopyCaptureManagerV1);
impl Dispatch<session::ExtImageCopyCaptureSessionV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &session::ExtImageCopyCaptureSessionV1,
        event: session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            session::Event::BufferSize { width, height } => {
                state.capture.size = (width, height);
                state.capture.constraints.push("size");
            }
            session::Event::ShmFormat { format } => {
                assert_eq!(
                    format,
                    wayland_client::WEnum::Value(wl_shm::Format::Xrgb8888)
                );
                state.capture.constraints.push("format");
            }
            session::Event::Done => state.capture.constraints.push("done"),
            session::Event::Stopped => state.capture.stopped += 1,
            _ => panic!("unexpected capture constraints"),
        }
    }
}
impl Dispatch<frame::ExtImageCopyCaptureFrameV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &frame::ExtImageCopyCaptureFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Transform { transform } => {
                assert_eq!(
                    transform,
                    wayland_client::WEnum::Value(wl_output::Transform::Normal)
                );
                state.capture.events.push("transform");
            }
            frame::Event::Damage {
                x,
                y,
                width,
                height,
            } => {
                assert_eq!((x, y, width as u32, height as u32), (0, 0, state.capture.size.0, state.capture.size.1));
                state.capture.events.push("damage");
            }
            frame::Event::PresentationTime {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                assert!(tv_nsec < 1_000_000_000);
                state.capture.time = Some((tv_sec_hi, tv_sec_lo, tv_nsec));
                state.capture.events.push("time");
            }
            frame::Event::Ready => state.capture.events.push("ready"),
            frame::Event::Failed {
                reason: wayland_client::WEnum::Value(reason),
            } => {
                state.capture.failures.push(reason);
            }
            _ => panic!("unexpected capture event"),
        }
    }
}

pub(super) fn session(h: &mut Harness) -> session::ExtImageCopyCaptureSessionV1 {
    let qh = h.queue.handle();
    let source = h.client.capture.source.as_ref().unwrap().create_source(
        h.client.capture.output.as_ref().unwrap(),
        &qh,
        (),
    );
    let session = h.client.capture.manager.as_ref().unwrap().create_session(
        &source,
        manager::Options::empty(),
        &qh,
        (),
    );
    // Source destruction must not invalidate a session.
    source.destroy();
    h.pump();
    session
}
pub(super) fn buffer(h: &Harness, width: i32, height: i32) -> (compose::MemfdBuffer, wl_buffer::WlBuffer) {
    let mut mem = compose::MemfdBuffer::new(width as u32, height as u32).unwrap();
    mem.as_mut_slice().fill(0xcc);
    let qh = h.queue.handle();
    let pool = h.client.shm.as_ref().unwrap().create_pool(
        unsafe { BorrowedFd::borrow_raw(mem.fd) },
        width * height * 4,
        &qh,
        (),
    );
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        width * 4,
        wl_shm::Format::Xrgb8888,
        &qh,
        (),
    );
    pool.destroy();
    (mem, buffer)
}
pub(super) fn capture(
    h: &mut Harness,
    session: &session::ExtImageCopyCaptureSessionV1,
    buffer: &wl_buffer::WlBuffer,
) -> frame::ExtImageCopyCaptureFrameV1 {
    let frame = session.create_frame(&h.queue.handle(), ());
    frame.attach_buffer(buffer);
    frame.damage_buffer(0, 0, 416, 416);
    frame.capture();
    h.pump();
    frame
}
fn present(h: &mut Harness, color: [u8; 4]) {
    h.compositor.wayland.capture.presented();
    h.compositor
        .wayland
        .capture
        .copy_pending(&color.repeat(416 * 416));
    h.pump();
}
pub(super) fn protocol_error(mut h: Harness, code: u32) {
    h.conn.flush().unwrap();
    h.display.dispatch_clients(&mut h.compositor).unwrap();
    h.display.flush_clients().unwrap();
    assert!(h.queue.blocking_dispatch(&mut h.client).is_err());
    let wayland_client::backend::WaylandError::Protocol(error) =
        h.conn.backend().last_error().unwrap()
    else {
        panic!("expected protocol error");
    };
    assert_eq!(error.code, code);
}

#[test]
fn captures_static_first_frame_and_every_new_frame_without_a_timer() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    assert_eq!(h.client.capture.constraints, ["size", "format", "done"]);
    assert_eq!(h.client.capture.size, (416, 416));
    let (mut mem, buffer) = buffer(&h, 416, 416);
    present(&mut h, [1, 2, 3, 4]);
    let mut f = capture(&mut h, &s, &buffer);
    // Already-presented static image: no repaint is necessary for first capture.
    h.compositor
        .wayland
        .capture
        .copy_pending(&[10, 20, 30, 0].repeat(416 * 416));
    h.pump();
    assert_eq!(&mem.as_mut_slice()[..4], &[30, 20, 10, 255]);
    assert_eq!(
        h.client.capture.events,
        ["transform", "damage", "time", "ready"]
    );
    // Repeat faster than display cadence. No hidden 10/15 fps capture limiter.
    for n in 0..60 {
        f.destroy();
        h.client.capture.events.clear();
        f = capture(&mut h, &s, &buffer);
        h.compositor
            .wayland
            .capture
            .copy_pending(&[0; 416 * 416 * 4]);
        h.pump();
        assert!(
            h.client.capture.events.is_empty(),
            "unchanged image should wait"
        );
        present(&mut h, [n, 2, 3, 0]);
        assert_eq!(h.client.capture.events.last(), Some(&"ready"));
        assert_eq!(&mem.as_mut_slice()[..4], &[3, 2, n, 255]);
    }
}

#[test]
fn locking_stops_existing_watch_capture_frames() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (_mem, buffer) = buffer(&h, 416, 416);
    present(&mut h, [7, 8, 9, 255]);
    let _pending = capture(&mut h, &s, &buffer);

    h.compositor.lock_screen.command = vec!["/usr/lib/lock-screen".into()];
    h.compositor.lock_enabled = true;
    h.compositor.refresh_lock_state();
    h.pump();

    assert!(h.compositor.is_locked());
    assert_eq!(h.client.capture.stopped, 1);
    assert_eq!(h.client.capture.failures, [frame::FailureReason::Stopped]);
}

#[test]
fn wrong_buffer_can_be_retried_and_session_destruction_preserves_frame() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (_small, bad) = buffer(&h, 4, 4);
    let f = capture(&mut h, &s, &bad);
    present(&mut h, [1, 2, 3, 0]);
    assert_eq!(
        h.client.capture.failures,
        [frame::FailureReason::BufferConstraints]
    );
    f.destroy();
    let (mut mem, good) = buffer(&h, 416, 416);
    let _f = capture(&mut h, &s, &good);
    s.destroy();
    h.pump();
    present(&mut h, [4, 5, 6, 0]);
    assert_eq!(h.client.capture.events.last(), Some(&"ready"));
    assert_eq!(&mem.as_mut_slice()[..4], &[6, 5, 4, 255]);
}

#[test]
fn display_off_stops_pending_and_new_sessions_and_wake_allows_new_session() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (_mem, buf) = buffer(&h, 416, 416);
    let _f = capture(&mut h, &s, &buf);
    h.compositor.wayland.capture.set_active(false);
    h.pump();
    assert_eq!(h.client.capture.stopped, 1);
    assert_eq!(h.client.capture.failures, [frame::FailureReason::Stopped]);
    let _off = session(&mut h);
    assert_eq!(h.client.capture.stopped, 2);
    h.compositor.wayland.capture.set_active(true);
    let on = session(&mut h);
    let _f = capture(&mut h, &on, &buf);
    present(&mut h, [1, 2, 3, 0]);
    assert_eq!(h.client.capture.events.last(), Some(&"ready"));
}

#[test]
fn destroyed_frame_is_cancelled_and_can_be_replaced() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (mut mem, buf) = buffer(&h, 416, 416);
    let f = capture(&mut h, &s, &buf);
    f.destroy();
    h.pump();
    present(&mut h, [1, 2, 3, 0]);
    assert!(h.client.capture.events.is_empty());
    assert!(mem.as_mut_slice().iter().all(|b| *b == 0xcc));
    let _next = capture(&mut h, &s, &buf);
    present(&mut h, [4, 5, 6, 0]);
    assert_eq!(h.client.capture.events.last(), Some(&"ready"));
}

#[test]
fn missing_buffer_is_a_protocol_error() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    s.create_frame(&h.queue.handle(), ()).capture();
    protocol_error(h, frame::Error::NoBuffer as u32);
}
#[test]
fn duplicate_frame_is_a_protocol_error() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let _one = s.create_frame(&h.queue.handle(), ());
    let _two = s.create_frame(&h.queue.handle(), ());
    protocol_error(h, session::Error::DuplicateFrame as u32);
}
#[test]
fn invalid_damage_is_a_protocol_error() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    s.create_frame(&h.queue.handle(), ())
        .damage_buffer(-1, 0, 1, 1);
    protocol_error(h, frame::Error::InvalidBufferDamage as u32);
}
#[test]
fn double_capture_is_a_protocol_error() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (_mem, buf) = buffer(&h, 416, 416);
    let f = capture(&mut h, &s, &buf);
    f.capture();
    protocol_error(h, frame::Error::AlreadyCaptured as u32);
}

#[test]
fn a_slow_session_does_not_hold_up_another_session() {
    let mut h = Harness::with_size(416, 416);
    let slow = session(&mut h);
    let fast = session(&mut h);
    let (mut slow_mem, slow_buf) = buffer(&h, 416, 416);
    let (mut fast_mem, fast_buf) = buffer(&h, 416, 416);
    let _slow_frame = capture(&mut h, &slow, &slow_buf);
    let f = capture(&mut h, &fast, &fast_buf);
    present(&mut h, [1, 2, 3, 0]);
    f.destroy();
    let _f = capture(&mut h, &fast, &fast_buf);
    present(&mut h, [4, 5, 6, 0]);
    assert_eq!(&slow_mem.as_mut_slice()[..4], &[3, 2, 1, 255]);
    assert_eq!(&fast_mem.as_mut_slice()[..4], &[6, 5, 4, 255]);
    assert_eq!(
        h.client
            .capture
            .events
            .iter()
            .filter(|e| **e == "ready")
            .count(),
        3
    );
}

#[test]
fn destroying_attached_buffer_fails_without_writing_it() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (mut mem, buf) = buffer(&h, 416, 416);
    let _f = capture(&mut h, &s, &buf);
    buf.destroy();
    h.pump();
    present(&mut h, [1, 2, 3, 0]);
    assert_eq!(
        h.client.capture.failures,
        [frame::FailureReason::BufferConstraints]
    );
    assert!(mem.as_mut_slice().iter().all(|b| *b == 0xcc));
}

#[test]
fn capture_from_a_stopped_session_fails_after_wake() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    h.compositor.wayland.capture.set_active(false);
    h.compositor.wayland.capture.set_active(true);
    let (_mem, buf) = buffer(&h, 416, 416);
    let _f = capture(&mut h, &s, &buf);
    assert_eq!(h.client.capture.failures, [frame::FailureReason::Stopped]);
}

#[test]
fn unknown_capture_options_are_a_protocol_error() {
    let h = Harness::with_size(416, 416);
    let qh = h.queue.handle();
    let source = h.client.capture.source.as_ref().unwrap().create_source(
        h.client.capture.output.as_ref().unwrap(),
        &qh,
        (),
    );
    let _s = h.client.capture.manager.as_ref().unwrap().create_session(
        &source,
        manager::Options::from_bits_retain(2),
        &qh,
        (),
    );
    protocol_error(h, manager::Error::InvalidOption as u32);
}

#[test]
fn orphan_frame_cannot_cross_a_display_off_cycle() {
    let mut h = Harness::with_size(416, 416);
    let s = session(&mut h);
    let (_mem, buf) = buffer(&h, 416, 416);
    let f = s.create_frame(&h.queue.handle(), ());
    f.attach_buffer(&buf);
    s.destroy();
    h.pump();
    h.compositor.wayland.capture.set_active(false);
    h.compositor.wayland.capture.set_active(true);
    f.capture();
    h.pump();
    assert_eq!(h.client.capture.failures, [frame::FailureReason::Stopped]);
}
