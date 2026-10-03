//! Real output-management + ext-capture transactions on the headless harness.
use super::*;
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1 as configuration_head,
    zwlr_output_configuration_v1 as configuration, zwlr_output_head_v1 as head,
    zwlr_output_manager_v1 as manager, zwlr_output_mode_v1 as mode,
};
#[derive(Default)]
pub(super) struct ClientOutputs {
    pub manager: Option<manager::ZwlrOutputManagerV1>,
    heads: Vec<(String, head::ZwlrOutputHeadV1)>,
    serial: u32,
    results: Vec<&'static str>,
}
impl Dispatch<manager::ZwlrOutputManagerV1, ()> for Client {
    fn event(
        s: &mut Self,
        _: &manager::ZwlrOutputManagerV1,
        e: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            manager::Event::Head { head } => s.outputs.heads.push((String::new(), head)),
            manager::Event::Done { serial } => s.outputs.serial = serial,
            _ => {}
        }
    }
    wayland_client::event_created_child!(Client, manager::ZwlrOutputManagerV1, [0 => (head::ZwlrOutputHeadV1, ())]);
}
impl Dispatch<head::ZwlrOutputHeadV1, ()> for Client {
    fn event(
        s: &mut Self,
        h: &head::ZwlrOutputHeadV1,
        e: head::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let head::Event::Name { name } = e {
            s.outputs.heads.iter_mut().find(|(_, v)| v == h).unwrap().0 = name;
        }
    }
    wayland_client::event_created_child!(Client, head::ZwlrOutputHeadV1, [3 => (mode::ZwlrOutputModeV1, ())]);
}
delegate_noop!(Client: ignore mode::ZwlrOutputModeV1);
delegate_noop!(Client: ignore configuration_head::ZwlrOutputConfigurationHeadV1);
impl Dispatch<configuration::ZwlrOutputConfigurationV1, ()> for Client {
    fn event(
        s: &mut Self,
        _: &configuration::ZwlrOutputConfigurationV1,
        e: configuration::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        s.outputs.results.push(match e {
            configuration::Event::Succeeded => "succeeded",
            configuration::Event::Failed => "failed",
            configuration::Event::Cancelled => "cancelled",
            _ => panic!(),
        });
    }
}
fn transaction(
    h: &Harness,
    config: crate::desktop::Config,
    serial: u32,
) -> configuration::ZwlrOutputConfigurationV1 {
    let qh = h.queue.handle();
    let tx = h
        .client
        .outputs
        .manager
        .as_ref()
        .unwrap()
        .create_configuration(serial, &qh, ());
    for (name, head) in &h.client.outputs.heads {
        if name == "hoki-display" {
            tx.enable_head(head, &qh, ());
        } else if !config.enabled {
            tx.disable_head(head);
        } else {
            let head = tx.enable_head(head, &qh, ());
            head.set_custom_mode(config.width, config.height, config.refresh);
            head.set_scale(config.scale);
            head.set_position(config.position.0, config.position.1);
        }
    }
    tx
}
fn apply(h: &mut Harness, config: crate::desktop::Config) {
    let tx = transaction(h, config, h.client.outputs.serial);
    tx.apply();
    h.pump();
    h.pump();
    tx.destroy();
}
fn small() -> crate::desktop::Config {
    crate::desktop::Config {
        enabled: true,
        width: 128,
        height: 96,
        ..Default::default()
    }
}

#[test]
fn management_is_atomic_and_preserves_the_watch() {
    let mut h = Harness::new();
    assert_eq!(h.client.outputs.heads.len(), 2);
    assert!(!h.compositor.wayland.desktop.config.enabled);
    let serial = h.client.outputs.serial;
    let test = transaction(&h, small(), serial);
    test.test();
    h.pump();
    assert_eq!(h.client.outputs.results, ["succeeded"]);
    assert!(!h.compositor.wayland.desktop.config.enabled);
    test.destroy();
    apply(&mut h, small());
    assert_eq!(h.compositor.wayland.desktop.config, small());
    let stale = transaction(&h, crate::desktop::Config::default(), serial);
    stale.apply();
    h.pump();
    assert_eq!(h.client.outputs.results.last(), Some(&"cancelled"));
    assert!(h.compositor.wayland.desktop.config.enabled);
    stale.destroy();
    for invalid in [
        crate::desktop::Config {
            scale: 1.5,
            ..small()
        },
        crate::desktop::Config {
            width: 4096,
            height: 4096,
            ..small()
        },
        crate::desktop::Config {
            refresh: 121000,
            ..small()
        },
        crate::desktop::Config {
            position: (4, 0),
            ..small()
        },
    ] {
        apply(&mut h, invalid);
        assert_eq!(h.client.outputs.results.last(), Some(&"failed"));
        assert_eq!(h.compositor.wayland.desktop.config, small());
    }
    let tx = h
        .client
        .outputs
        .manager
        .as_ref()
        .unwrap()
        .create_configuration(h.client.outputs.serial, &h.queue.handle(), ());
    for (_, head) in &h.client.outputs.heads {
        tx.disable_head(head);
    }
    tx.apply();
    h.pump();
    assert_eq!(h.client.outputs.results.last(), Some(&"failed"));
    assert!(h.compositor.display_on);
    assert!(h.compositor.wayland.desktop.config.enabled);
    apply(
        &mut h,
        crate::desktop::Config {
            enabled: false,
            ..small()
        },
    );
    assert!(!h.compositor.wayland.desktop.config.enabled);
    assert!(h.compositor.wayland.desktop.global.is_none());
}

#[test]
fn desktop_is_capture_paced_and_independent_of_panel_power() {
    let mut h = Harness::with_domain(416, 416, true);
    apply(&mut h, small());
    let (surface, _, _) = h.window();
    h.map(&surface, 23);
    let focused = h.compositor.wayland.desktop.focus.clone();
    assert!(focused.is_some());
    assert!(h.compositor.focused_surface.is_none());
    assert!(h.compositor.app_surfaces.is_empty());
    surface.frame(&h.queue.handle(), true);
    surface.commit();
    h.pump();
    let now = std::time::Instant::now();
    assert_eq!(h.compositor.wayland.desktop.deadline(now), None);
    assert!(!h.compositor.wayland.desktop.render(now, 0));
    assert_eq!(h.client.frames, 0);
    let s = capture_protocol::session(&mut h);
    assert_eq!(h.client.capture.size, (128, 96));
    let (mut mem, buffer) = capture_protocol::buffer(&h, 128, 96);
    let f = capture_protocol::capture(&mut h, &s, &buffer);
    h.compositor.display_on = false;
    h.compositor.wayland.capture.set_active(false);
    assert!(h.compositor.wayland.desktop.render(now, 1));
    h.pump();
    assert_eq!(h.client.capture.stopped, 0);
    assert_eq!(h.client.capture.events.last(), Some(&"ready"));
    assert_eq!(h.client.frames, 1);
    assert_eq!(&mem.as_mut_slice()[..4], &[23, 23, 23, 255]);
    assert_eq!(h.compositor.wayland.desktop.deadline(now), None);
    f.destroy();
    h.pump();
    let f = capture_protocol::capture(&mut h, &s, &buffer);
    assert!(
        !h.compositor
            .wayland
            .desktop
            .render(now + std::time::Duration::from_millis(10), 2)
    );
    assert!(
        h.compositor
            .wayland
            .desktop
            .render(now + std::time::Duration::from_millis(34), 3)
    );
    f.destroy();
    h.pump();
    assert!(
        !h.compositor
            .wayland
            .desktop
            .render(now + std::time::Duration::from_secs(10), 4)
    );
    assert_eq!(h.compositor.wayland.desktop.focus, focused);
}

#[test]
fn resize_and_disable_stop_old_captures_without_crossing_output_lifetimes() {
    let mut h = Harness::with_domain(416, 416, true);
    apply(&mut h, small());
    let old_output = h.client.capture.output.clone().unwrap();
    let s = capture_protocol::session(&mut h);
    let (_mem, buffer) = capture_protocol::buffer(&h, 128, 96);
    let _f = capture_protocol::capture(&mut h, &s, &buffer);
    apply(
        &mut h,
        crate::desktop::Config {
            width: 256,
            scale: 2.,
            ..small()
        },
    );
    assert_eq!(h.client.capture.stopped, 1);
    assert_eq!(h.client.capture.failures.len(), 1);
    let _new = capture_protocol::session(&mut h);
    assert_eq!(h.client.capture.size, (256, 96));
    assert_eq!(
        h.compositor.wayland.desktop.config.logical_size(),
        (128, 48)
    );
    apply(
        &mut h,
        crate::desktop::Config {
            enabled: false,
            ..small()
        },
    );
    assert_eq!(h.client.capture.stopped, 2);
    apply(&mut h, small());
    let fresh_output = h.client.capture.output.replace(old_output).unwrap();
    let _stale = capture_protocol::session(&mut h);
    assert_eq!(h.client.capture.stopped, 3);
    h.client.capture.output = Some(fresh_output);
    let _fresh = capture_protocol::session(&mut h);
    assert_eq!(h.client.capture.stopped, 3);
}

#[test]
fn desktop_mouse_never_wakes_or_focuses_the_watch() {
    let mut h = Harness::with_domain(416, 416, true);
    apply(&mut h, small());
    let (surface, _, _) = h.window();
    h.map(&surface, 42);
    h.compositor.display_on = false;
    let activity = h.compositor.last_activity;
    h.compositor
        .handle_desktop_input(input::DesktopInput::Motion { dx: 40., dy: 20. }, 1);
    h.compositor.handle_desktop_input(
        input::DesktopInput::Button(input::ButtonEvent {
            code: 272,
            pressed: true,
        }),
        2,
    );
    h.compositor.handle_desktop_input(
        input::DesktopInput::Button(input::ButtonEvent {
            code: 272,
            pressed: false,
        }),
        3,
    );
    h.compositor.handle_desktop_input(
        input::DesktopInput::Key(input::ButtonEvent {
            code: KEY_POWER,
            pressed: true,
        }),
        4,
    );
    h.pump();
    assert_eq!(h.client.clicks, 2); // press and release
    assert!(!h.compositor.display_on);
    assert_eq!(h.compositor.last_activity, activity);
    assert!(h.compositor.focused_surface.is_none());
    assert_eq!(h.compositor.wayland.desktop.pointer, (40., 20.));
    surface.attach(None, 0, 0);
    surface.commit();
    h.pump();
    assert!(h.compositor.wayland.desktop.focus.is_none());
}

#[test]
fn omitted_head_is_a_protocol_error() {
    let h = Harness::new();
    let tx = h
        .client
        .outputs
        .manager
        .as_ref()
        .unwrap()
        .create_configuration(h.client.outputs.serial, &h.queue.handle(), ());
    tx.apply();
    capture_protocol::protocol_error(h, configuration::Error::UnconfiguredHead as u32);
}

#[test]
fn duplicate_head_and_duplicate_property_are_protocol_errors() {
    let h = Harness::new();
    let tx = h
        .client
        .outputs
        .manager
        .as_ref()
        .unwrap()
        .create_configuration(h.client.outputs.serial, &h.queue.handle(), ());
    let head = &h.client.outputs.heads[0].1;
    tx.enable_head(head, &h.queue.handle(), ());
    tx.disable_head(head);
    capture_protocol::protocol_error(h, configuration::Error::AlreadyConfiguredHead as u32);
    let h = Harness::new();
    let tx = h
        .client
        .outputs
        .manager
        .as_ref()
        .unwrap()
        .create_configuration(h.client.outputs.serial, &h.queue.handle(), ());
    let head = tx.enable_head(&h.client.outputs.heads[1].1, &h.queue.handle(), ());
    head.set_scale(1.);
    head.set_scale(2.);
    capture_protocol::protocol_error(h, configuration_head::Error::AlreadySet as u32);
}

#[test]
fn cancelled_capture_removes_demand_and_cursor_is_opt_in() {
    use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::Options;
    let mut h = Harness::with_domain(416, 416, true);
    apply(&mut h, small());
    let s = capture_protocol::session(&mut h);
    let (_mem, buffer) = capture_protocol::buffer(&h, 128, 96);
    let frame = capture_protocol::capture(&mut h, &s, &buffer);
    frame.destroy();
    h.pump();
    assert_eq!(
        h.compositor
            .wayland
            .desktop
            .deadline(std::time::Instant::now()),
        None
    );
    let source = h.client.capture.source.as_ref().unwrap().create_source(
        h.client.capture.output.as_ref().unwrap(),
        &h.queue.handle(),
        (),
    );
    let cursor = h.client.capture.manager.as_ref().unwrap().create_session(
        &source,
        Options::PaintCursors,
        &h.queue.handle(),
        (),
    );
    h.pump();
    let (mut plain_mem, plain_buffer) = capture_protocol::buffer(&h, 128, 96);
    let (mut cursor_mem, cursor_buffer) = capture_protocol::buffer(&h, 128, 96);
    let _f = capture_protocol::capture(&mut h, &s, &plain_buffer);
    let _f = capture_protocol::capture(&mut h, &cursor, &cursor_buffer);
    assert!(
        h.compositor
            .wayland
            .desktop
            .render(std::time::Instant::now(), 1)
    );
    h.pump();
    assert_eq!(&plain_mem.as_mut_slice()[..4], &[0, 0, 0, 255]);
    assert_eq!(&cursor_mem.as_mut_slice()[..4], &[255, 255, 255, 255]);
}

#[test]
fn disabling_desktop_releases_held_modifiers() {
    let mut h = Harness::with_domain(416, 416, true);
    apply(&mut h, small());
    let (surface, _, _) = h.window();
    h.map(&surface, 42);
    h.compositor.handle_desktop_input(
        input::DesktopInput::Key(input::ButtonEvent {
            code: 42,
            pressed: true,
        }),
        1,
    );
    let kb = h.compositor.wayland.desktop.seat.get_keyboard().unwrap();
    assert!(!kb.pressed_keys().is_empty());
    apply(
        &mut h,
        crate::desktop::Config {
            enabled: false,
            ..small()
        },
    );
    assert!(kb.pressed_keys().is_empty());
    assert!(kb.current_focus().is_none());
    apply(&mut h, small());
    assert!(kb.current_focus().is_some());
}

#[test]
fn client_disconnect_clears_desktop_focus_and_capture_demand() {
    let mut h = Harness::with_domain(416, 416, true);
    apply(&mut h, small());
    let (surface, _, _) = h.window();
    h.map(&surface, 12);
    let s = capture_protocol::session(&mut h);
    let (_mem, buffer) = capture_protocol::buffer(&h, 128, 96);
    let _f = capture_protocol::capture(&mut h, &s, &buffer);
    assert!(h.compositor.wayland.desktop.capture.has_demand());
    use std::os::fd::AsRawFd;
    assert_eq!(
        unsafe { libc::shutdown(h.conn.backend().poll_fd().as_raw_fd(), libc::SHUT_RDWR) },
        0
    );
    h.display.dispatch_clients(&mut h.compositor).unwrap();
    assert!(h.compositor.wayland.desktop.focus.is_none());
    assert!(h.compositor.wayland.desktop.surfaces.is_empty());
    assert!(!h.compositor.wayland.desktop.capture.has_demand());
    assert!(h.compositor.wayland.desktop.config.enabled); // no implicit lease
    assert_eq!(h.compositor.shell_mode, ShellMode::Launcher);
}

#[test]
fn absolute_desktop_pointer_uses_logical_dimensions_and_clamps() {
    let mut h = Harness::with_domain(416, 416, true);
    apply(
        &mut h,
        crate::desktop::Config {
            scale: 2.,
            ..small()
        },
    );
    h.compositor
        .handle_desktop_input(input::DesktopInput::Absolute { x: 1., y: 1. }, 1);
    assert_eq!(h.compositor.wayland.desktop.pointer, (63., 47.));
    h.compositor
        .handle_desktop_input(input::DesktopInput::Absolute { x: -1., y: 2. }, 2);
    assert_eq!(h.compositor.wayland.desktop.pointer, (0., 47.));
    h.compositor
        .handle_desktop_input(input::DesktopInput::Absolute { x: f64::NAN, y: 0. }, 3);
    assert_eq!(h.compositor.wayland.desktop.pointer, (0., 47.));
    assert!(h.compositor.focused_surface.is_none());
}
