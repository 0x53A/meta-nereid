use std::{env, io};

#[derive(Debug)]
pub struct Config {
    pub ble: bool,
    pub classic: bool,
    pub name: String,
    pub ssh_port: u16,
    pub gatt_handle: u16,
}

impl Config {
    pub fn from_env() -> io::Result<Self> {
        Self::parse(|key| env::var(key).ok())
    }

    fn parse(get: impl Fn(&str) -> Option<String>) -> io::Result<Self> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        let (ble, classic) = match get("BLE_SSH_TRANSPORT").as_deref().unwrap_or("ble") {
            "ble" => (true, false),
            "classic" => (false, true),
            "both" => (true, true),
            _ => return Err(invalid("BLE_SSH_TRANSPORT must be ble, classic, or both")),
        };
        let name = get("BLE_SSH_NAME").unwrap_or_else(|| "AsteroidOS-SSH".into());
        if name.is_empty() || name.len() > 20 {
            return Err(invalid("BLE_SSH_NAME must contain 1–20 UTF-8 bytes"));
        }
        let ssh_port = get("BLE_SSH_PORT")
            .unwrap_or_else(|| "22".into())
            .parse::<u16>()
            .map_err(|_| invalid("BLE_SSH_PORT must be 1–65535"))?;
        if ssh_port == 0 {
            return Err(invalid("BLE_SSH_PORT must be 1–65535"));
        }
        let handle = get("BLE_SSH_GATT_HANDLE").unwrap_or_else(|| "0x1000".into());
        let gatt_handle = match handle
            .strip_prefix("0x")
            .or_else(|| handle.strip_prefix("0X"))
        {
            Some(hex) => u16::from_str_radix(hex, 16),
            None => handle.parse::<u16>(),
        }
        .map_err(|_| invalid("BLE_SSH_GATT_HANDLE must be 1–65528 (decimal or 0x hex)"))?;
        if gatt_handle == 0 || gatt_handle > 0xfff8 {
            return Err(invalid(
                "BLE_SSH_GATT_HANDLE must reserve eight handles within 1–65535",
            ));
        }
        Ok(Self {
            ble,
            classic,
            name,
            ssh_port,
            gatt_handle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_transports() {
        let default = Config::parse(|_| None).unwrap();
        assert!(default.ble && !default.classic);
        assert_eq!(default.ssh_port, 22);
        assert_eq!(default.gatt_handle, 0x1000);
        for (value, expected) in [
            ("ble", (true, false)),
            ("classic", (false, true)),
            ("both", (true, true)),
        ] {
            let config =
                Config::parse(|key| (key == "BLE_SSH_TRANSPORT").then(|| value.into())).unwrap();
            assert_eq!((config.ble, config.classic), expected);
        }
    }

    #[test]
    fn accepts_handle_bounds_and_hex_or_decimal() {
        for (value, expected) in [
            ("1", 1),
            ("65528", 0xfff8),
            ("0xfff8", 0xfff8),
            ("0X1000", 4096),
        ] {
            let config =
                Config::parse(|key| (key == "BLE_SSH_GATT_HANDLE").then(|| value.into())).unwrap();
            assert_eq!(config.gatt_handle, expected);
        }
    }

    #[test]
    fn rejects_invalid_configuration() {
        for (key, value) in [
            ("BLE_SSH_GATT_HANDLE", "0"),
            ("BLE_SSH_GATT_HANDLE", "0xfff9"),
            ("BLE_SSH_GATT_HANDLE", "65536"),
            ("BLE_SSH_GATT_HANDLE", "garbage"),
            ("BLE_SSH_TRANSPORT", "auto"),
            ("BLE_SSH_PORT", "0"),
            ("BLE_SSH_PORT", "65536"),
            ("BLE_SSH_PORT", "host:22"),
            ("BLE_SSH_NAME", ""),
            ("BLE_SSH_NAME", "123456789012345678901"),
        ] {
            assert!(Config::parse(|k| (k == key).then(|| value.into())).is_err());
        }
    }
}
