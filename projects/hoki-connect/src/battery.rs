use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Reading {
    pub charge: i64,
    pub charging: bool,
}

impl Reading {
    pub fn parse(body: &Value) -> Option<Self> {
        let charge = body["currentCharge"].as_i64()?;
        (-1..=100).contains(&charge).then_some(Self {
            charge,
            charging: body["isCharging"].as_bool()?,
        })
    }
    pub fn packet(self, previous: Option<Self>) -> Value {
        let low = |r: Self| (0..=15).contains(&r.charge) && !r.charging;
        json!({"currentCharge":self.charge,"isCharging":self.charging,
            "thresholdEvent": if low(self) && !previous.is_some_and(low) { 1 } else { 0 }})
    }
}
pub(super) fn local() -> Option<Reading> {
    let root = std::env::var_os("HOKI_CONNECT_BATTERY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/sys/class/power_supply/battery".into());
    read(&root)
}
fn read(root: &Path) -> Option<Reading> {
    let charge = fs::read_to_string(root.join("capacity"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if !(0..=100).contains(&charge) {
        return None;
    }
    let status = fs::read_to_string(root.join("status")).ok()?;
    let charging = match status.trim() {
        "Charging" | "Full" => true,
        "Discharging" | "Not charging" => false,
        _ => return None,
    };
    Some(Reading { charge, charging })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_values_and_low_charge_transition() {
        assert!(Reading::parse(&json!({"currentCharge":101,"isCharging":false})).is_none());
        assert!(Reading::parse(&json!({"currentCharge":50,"isCharging":"false"})).is_none());
        let low = Reading {
            charge: 15,
            charging: false,
        };
        assert_eq!(low.packet(None)["thresholdEvent"], 1);
        assert_eq!(low.packet(Some(low))["thresholdEvent"], 0);
        assert_eq!(
            Reading {
                charging: true,
                ..low
            }
            .packet(Some(low))["thresholdEvent"],
            0
        );
        let root = std::env::temp_dir().join(format!("hoki-battery-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("capacity"), "83\n").unwrap();
        fs::write(root.join("status"), "Full\n").unwrap();
        assert_eq!(
            read(&root),
            Some(Reading {
                charge: 83,
                charging: true
            })
        );
        fs::write(root.join("capacity"), "unknown").unwrap();
        assert!(read(&root).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
