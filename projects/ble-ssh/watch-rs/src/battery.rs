//! Standard read/notify Battery Service, independent of an SSH session.
use bluer::gatt::local::{Characteristic, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicRead, ReqError, Service};
use futures::FutureExt;
use std::time::Duration;
use uuid::Uuid;

fn parse(raw: &str) -> Option<u8> {
    raw.trim().parse::<u8>().ok().filter(|n| *n <= 100)
}
fn level() -> Option<u8> {
    parse(&std::fs::read_to_string("/sys/class/power_supply/battery/capacity").ok()?)
}

pub fn service() -> Service {
    Service {
        uuid: Uuid::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb),
        primary: true,
        characteristics: vec![Characteristic {
            uuid: Uuid::from_u128(0x00002a19_0000_1000_8000_00805f9b34fb),
            read: Some(CharacteristicRead {
                read: true,
                fun: Box::new(|request| async move {
                    if request.offset != 0 { return Err(ReqError::InvalidOffset); }
                    level().map(|n| vec![n]).ok_or(ReqError::Failed)
                }.boxed()),
                ..Default::default()
            }),
            notify: Some(CharacteristicNotify {
                notify: true,
                method: CharacteristicNotifyMethod::Fun(Box::new(|mut notifier| async move {
                    tokio::spawn(async move {
                        let mut previous = None;
                        loop {
                            if let Some(n) = level() {
                                if previous != Some(n) {
                                    if notifier.notify(vec![n]).await.is_err() { break; }
                                    previous = Some(n);
                                }
                            }
                            tokio::select! {
                                _ = notifier.stopped() => break,
                                _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                            }
                        }
                    });
                }.boxed())),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_readings_are_not_invented_or_clamped() {
        assert_eq!(parse("42\n"), Some(42));
        assert_eq!(parse("0"), Some(0));
        assert_eq!(parse("100"), Some(100));
        for raw in ["", "-1", "101", "256", "unknown"] { assert_eq!(parse(raw), None); }
    }
}
