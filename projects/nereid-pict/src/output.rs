//! Local Nereid output-management transactions. All other heads retain their state.
use anyhow::{Context, Result, ensure};
use std::{
    os::fd::AsRawFd,
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop, protocol::wl_registry,
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1 as configuration_head,
    zwlr_output_configuration_v1 as configuration, zwlr_output_head_v1 as head,
    zwlr_output_manager_v1 as manager, zwlr_output_mode_v1 as mode,
};
#[derive(Default)]
struct State {
    manager: Option<manager::ZwlrOutputManagerV1>,
    heads: Vec<(String, head::ZwlrOutputHeadV1, bool)>,
    serial: Option<u32>,
    result: Option<bool>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = e
        {
            if interface == "zwlr_output_manager_v1" {
                s.manager = Some(r.bind(name, 1, q, ()));
            }
        }
    }
}
impl Dispatch<manager::ZwlrOutputManagerV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &manager::ZwlrOutputManagerV1,
        e: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            manager::Event::Head { head } => s.heads.push((String::new(), head, false)),
            manager::Event::Done { serial } => s.serial = Some(serial),
            _ => {}
        }
    }
    wayland_client::event_created_child!(State, manager::ZwlrOutputManagerV1, [0 => (head::ZwlrOutputHeadV1, ())]);
}
impl Dispatch<head::ZwlrOutputHeadV1, ()> for State {
    fn event(
        s: &mut Self,
        h: &head::ZwlrOutputHeadV1,
        e: head::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(v) = s.heads.iter_mut().find(|v| &v.1 == h) {
            match e {
                head::Event::Name { name } => v.0 = name,
                head::Event::Enabled { enabled } => v.2 = enabled != 0,
                _ => {}
            }
        }
    }
    wayland_client::event_created_child!(State, head::ZwlrOutputHeadV1, [3 => (mode::ZwlrOutputModeV1, ())]);
}
delegate_noop!(State: ignore mode::ZwlrOutputModeV1);
delegate_noop!(State: ignore configuration_head::ZwlrOutputConfigurationHeadV1);
impl Dispatch<configuration::ZwlrOutputConfigurationV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &configuration::ZwlrOutputConfigurationV1,
        e: configuration::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        s.result = Some(matches!(e, configuration::Event::Succeeded));
    }
}
fn wait(q: &mut EventQueue<State>, s: &mut State, done: impl Fn(&State) -> bool) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        q.dispatch_pending(s)?;
        if done(s) {
            return Ok(());
        }
        ensure!(
            Instant::now() < until,
            "Nereid output transaction timed out"
        );
        q.flush()?;
        if let Some(read) = q.prepare_read() {
            let mut fd = libc::pollfd {
                fd: read.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut fd, 1, 50) };
            if n > 0 {
                read.read()?;
            } else if n < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
}
pub(crate) fn configure(
    enabled: bool,
    width: u32,
    height: u32,
    scale: u32,
    refresh: u32,
) -> Result<()> {
    let c = Connection::connect_to_env()?;
    let mut q = c.new_event_queue();
    let h = q.handle();
    let mut s = State::default();
    let _registry = c.display().get_registry(&h, ());
    wait(&mut q, &mut s, |s| s.serial.is_some())?;
    ensure!(
        s.heads.iter().any(|v| v.0 == "hoki-desktop"),
        "Nereid desktop output unavailable"
    );
    let tx = s
        .manager
        .as_ref()
        .context("Output manager unavailable")?
        .create_configuration(s.serial.unwrap(), &h, ());
    for (name, head, was_enabled) in &s.heads {
        if name == "hoki-desktop" {
            if enabled {
                let cfg = tx.enable_head(head, &h, ());
                cfg.set_custom_mode(width as i32, height as i32, refresh as i32);
                cfg.set_scale(scale as f64 / 100.0);
            } else {
                tx.disable_head(head);
            }
        } else if *was_enabled {
            tx.enable_head(head, &h, ());
        } else {
            tx.disable_head(head);
        }
    }
    tx.apply();
    wait(&mut q, &mut s, |s| s.result.is_some())?;
    tx.destroy();
    c.flush()?;
    ensure!(
        s.result == Some(true),
        "Nereid rejected output configuration"
    );
    Ok(())
}
