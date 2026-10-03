//! wlr-output-management v1: immutable watch head and one reserved desktop head.
use crate::{Compositor, desktop::Config};
use std::sync::{Arc, Mutex};
use wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1 as configuration_head,
    zwlr_output_configuration_v1 as configuration, zwlr_output_head_v1 as head,
    zwlr_output_manager_v1 as manager, zwlr_output_mode_v1 as mode,
};
use wayland_server::protocol::wl_output;
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
type Manager = manager::ZwlrOutputManagerV1;
type Head = head::ZwlrOutputHeadV1;
type Mode = mode::ZwlrOutputModeV1;
type Configuration = configuration::ZwlrOutputConfigurationV1;
type ConfigurationHead = configuration_head::ZwlrOutputConfigurationHeadV1;
#[derive(Clone)]
pub struct HeadData {
    index: usize,
    manager: Manager,
}
#[derive(Clone)]
pub struct ModeData {
    head: Head,
    config: Config,
}
pub struct ConfigurationData {
    serial: u32,
    manager: Manager,
    used: bool,
    heads: [Option<Config>; 2],
}
pub type Transaction = Arc<Mutex<ConfigurationData>>;
pub struct HeadConfigurationData {
    transaction: Transaction,
    head: Head,
    set: Mutex<u8>,
}
struct Subscriber {
    manager: Manager,
    heads: [Head; 2],
    modes: [Mode; 2],
    configs: [Config; 2],
}
pub struct OutputManagement {
    serial: u32,
    subscribers: Vec<Subscriber>,
}
impl OutputManagement {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<Compositor, Manager, _>(1, ());
        Self {
            serial: 1,
            subscribers: vec![],
        }
    }
    fn configurations(state: &Compositor) -> [Config; 2] {
        [
            Config {
                enabled: true,
                width: state.display_width as i32,
                height: state.display_height as i32,
                refresh: 45000,
                ..Config::default()
            },
            state.wayland.desktop.config,
        ]
    }
    fn send_properties(head: &Head, mode: &Mode, config: Config) {
        head.enabled(config.enabled as i32);
        if config.enabled {
            head.current_mode(mode);
            head.position(config.position.0, config.position.1);
            head.transform(wl_output::Transform::Normal);
            head.scale(config.scale);
        }
    }
    fn new_mode(client: &Client, dh: &DisplayHandle, head: &Head, config: Config) -> Mode {
        let mode = client
            .create_resource::<Mode, _, Compositor>(
                dh,
                1,
                ModeData {
                    head: head.clone(),
                    config,
                },
            )
            .unwrap();
        head.mode(&mode);
        mode.size(config.width, config.height);
        mode.refresh(config.refresh);
        mode.preferred();
        mode
    }
    fn publish(state: &mut Compositor, dh: &DisplayHandle) {
        let configs = Self::configurations(state);
        let management = &mut state.wayland.output_management;
        management.serial = management.serial.wrapping_add(1);
        management.subscribers.retain(|s| s.manager.is_alive());
        for sub in &mut management.subscribers {
            let Some(client) = sub.manager.client() else {
                continue;
            };
            for i in 0..2 {
                if sub.configs[i].mode() != configs[i].mode() {
                    let old = sub.modes[i].clone();
                    sub.modes[i] = Self::new_mode(&client, dh, &sub.heads[i], configs[i]);
                    Self::send_properties(&sub.heads[i], &sub.modes[i], configs[i]);
                    old.finished();
                } else {
                    Self::send_properties(&sub.heads[i], &sub.modes[i], configs[i]);
                }
            }
            sub.configs = configs;
            sub.manager.done(management.serial);
        }
    }
}
impl GlobalDispatch<Manager, ()> for Compositor {
    fn bind(
        state: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        id: New<Manager>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let manager = init.init(id, ());
        let configs = OutputManagement::configurations(state);
        let heads: [Head; 2] = std::array::from_fn(|i| {
            let h = client
                .create_resource::<Head, _, Self>(
                    dh,
                    1,
                    HeadData {
                        index: i,
                        manager: manager.clone(),
                    },
                )
                .unwrap();
            manager.head(&h);
            h.name(
                if i == 0 {
                    "hoki-display"
                } else {
                    "hoki-desktop"
                }
                .into(),
            );
            h.description(
                if i == 0 {
                    "Watch display (fixed)"
                } else {
                    "Virtual desktop"
                }
                .into(),
            );
            if i == 0 {
                h.physical_size(33, 33);
            }
            h
        });
        let modes =
            std::array::from_fn(|i| OutputManagement::new_mode(client, dh, &heads[i], configs[i]));
        for i in 0..2 {
            OutputManagement::send_properties(&heads[i], &modes[i], configs[i]);
        }
        manager.done(state.wayland.output_management.serial);
        state
            .wayland
            .output_management
            .subscribers
            .push(Subscriber {
                manager,
                heads,
                modes,
                configs,
            });
    }
}
impl Dispatch<Manager, ()> for Compositor {
    fn destroyed(
        state: &mut Self,
        _: wayland_server::backend::ClientId,
        resource: &Manager,
        _: &(),
    ) {
        state
            .wayland
            .output_management
            .subscribers
            .retain(|s| &s.manager != resource);
    }
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &Manager,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            manager::Request::CreateConfiguration { id, serial } => {
                init.init(
                    id,
                    Arc::new(Mutex::new(ConfigurationData {
                        serial,
                        manager: resource.clone(),
                        used: false,
                        heads: [None, None],
                    })),
                );
            }
            manager::Request::Stop => {
                state
                    .wayland
                    .output_management
                    .subscribers
                    .retain(|s| &s.manager != resource);
                resource.finished();
            }
            _ => {}
        }
    }
}
impl Dispatch<Head, HeadData> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &Head,
        _: head::Request,
        _: &HeadData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
impl Dispatch<Mode, ModeData> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &Mode,
        _: mode::Request,
        _: &ModeData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
impl Dispatch<Configuration, Transaction> for Compositor {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &Configuration,
        request: configuration::Request,
        data: &Transaction,
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let mut tx = data.lock().unwrap();
        if matches!(request, configuration::Request::Destroy) {
            tx.used = true;
            return;
        }
        if tx.used {
            resource.post_error(
                configuration::Error::AlreadyUsed,
                "configuration already used",
            );
            return;
        }
        match request {
            configuration::Request::EnableHead { id, head } => {
                let info = head.data::<HeadData>().unwrap();
                if info.manager != tx.manager || tx.heads[info.index].is_some() {
                    resource.post_error(
                        configuration::Error::AlreadyConfiguredHead,
                        "head from wrong manager or configured twice",
                    );
                    return;
                }
                let mut config = OutputManagement::configurations(state)[info.index];
                config.enabled = true;
                tx.heads[info.index] = Some(config);
                init.init(
                    id,
                    HeadConfigurationData {
                        transaction: data.clone(),
                        head,
                        set: Mutex::new(0),
                    },
                );
            }
            configuration::Request::DisableHead { head } => {
                let info = head.data::<HeadData>().unwrap();
                if info.manager != tx.manager || tx.heads[info.index].is_some() {
                    resource.post_error(
                        configuration::Error::AlreadyConfiguredHead,
                        "head from wrong manager or configured twice",
                    );
                    return;
                }
                let mut config = OutputManagement::configurations(state)[info.index];
                config.enabled = false;
                tx.heads[info.index] = Some(config);
            }
            configuration::Request::Apply | configuration::Request::Test => {
                tx.used = true;
                if tx.serial != state.wayland.output_management.serial {
                    resource.cancelled();
                    return;
                }
                let [Some(watch), Some(desktop)] = tx.heads else {
                    resource.post_error(
                        configuration::Error::UnconfiguredHead,
                        "configure both heads",
                    );
                    return;
                };
                if watch != OutputManagement::configurations(state)[0] || !desktop.valid() {
                    resource.failed();
                    return;
                }
                if matches!(request, configuration::Request::Apply)
                    && desktop != state.wayland.desktop.config
                {
                    state.apply_desktop_config(desktop);
                    OutputManagement::publish(state, dh);
                }
                resource.succeeded();
            }
            _ => {}
        }
    }
}
impl Dispatch<ConfigurationHead, HeadConfigurationData> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        resource: &ConfigurationHead,
        request: configuration_head::Request,
        data: &HeadConfigurationData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let mut tx = data.transaction.lock().unwrap();
        if tx.used {
            return;
        } // child is inert after its configuration is used/destroyed
        let index = data.head.data::<HeadData>().unwrap().index;
        let config = tx.heads[index].as_mut().unwrap();
        let flag = match request {
            configuration_head::Request::SetMode { .. }
            | configuration_head::Request::SetCustomMode { .. } => 1,
            configuration_head::Request::SetPosition { .. } => 2,
            configuration_head::Request::SetTransform { .. } => 4,
            configuration_head::Request::SetScale { .. } => 8,
            _ => return,
        };
        let mut set = data.set.lock().unwrap();
        if *set & flag != 0 {
            resource.post_error(configuration_head::Error::AlreadySet, "property set twice");
            return;
        }
        *set |= flag;
        match request {
            configuration_head::Request::SetMode { mode } => {
                let info = mode.data::<ModeData>().unwrap();
                if info.head != data.head {
                    resource.post_error(
                        configuration_head::Error::InvalidMode,
                        "mode belongs to another head",
                    );
                    return;
                }
                config.width = info.config.width;
                config.height = info.config.height;
                config.refresh = info.config.refresh;
            }
            configuration_head::Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                if width <= 0 || height <= 0 || refresh < 0 {
                    resource.post_error(
                        configuration_head::Error::InvalidCustomMode,
                        "invalid dimensions or refresh",
                    );
                    return;
                }
                config.width = width;
                config.height = height;
                config.refresh = if refresh == 0 { 30000 } else { refresh };
            }
            configuration_head::Request::SetPosition { x, y } => config.position = (x, y),
            configuration_head::Request::SetTransform { transform } => match transform {
                WEnum::Value(t) => config.transform = t as i32,
                _ => {
                    resource.post_error(
                        configuration_head::Error::InvalidTransform,
                        "unknown transform",
                    );
                }
            },
            configuration_head::Request::SetScale { scale } => {
                if scale <= 0. {
                    resource.post_error(
                        configuration_head::Error::InvalidScale,
                        "scale must be positive",
                    );
                    return;
                }
                config.scale = scale;
            }
            _ => {}
        }
    }
}
