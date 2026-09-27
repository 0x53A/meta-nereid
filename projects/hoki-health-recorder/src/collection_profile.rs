//! Collection selection is independent of Linux's power state.
use crate::{select, Result};
use serde_json::{json, Value};

pub const BUFFERED_FULL_LATENCY_STEPS_NS: [u64; 3] =
    [7_000_000_000, 20_000_000_000, 40_000_000_000];
pub const BUFFERED_FULL_FALLBACK_MARGIN_NS: u64 = 10_000_000_000;
pub const BUFFERED_FULL_TRIAL_DEFAULT_SECONDS: u64 = 300;
pub const BUFFERED_FULL_TRIAL_MAX_SECONDS: u64 = 1_800;

pub fn plan(inventory: &Value, profile: &str) -> Result<Vec<Value>> {
    if !matches!(profile, "daily" | "sleep" | "activity" | "full") {
        return Err("unknown collection profile".into());
    }
    // Ordinary profiles retain immediate delivery. Only the explicit trial
    // below accepts nonzero latency and records its FIFO risk classification.
    let mut selection = select(inventory, 0)?;
    selection.retain(|v| {
        let typ = v["sensor"]["type"].as_i64().unwrap_or(0);
        match profile {
            "daily" => matches!(typ, 18 | 19 | 21),
            "sleep" => matches!(typ, 1 | 18 | 19 | 21),
            "activity" => matches!(typ, 1 | 4 | 18 | 19 | 21),
            _ => true,
        }
    });
    if selection.is_empty() {
        return Err("profile has no available sensors".into());
    }
    if matches!(profile, "daily" | "sleep") {
        for v in &mut selection {
            let sensor = &v["sensor"];
            let mut period = if profile == "daily" {
                1_000_000_000u64
            } else {
                200_000_000u64
            };
            if let Some(min) = sensor["min_delay_us"].as_u64() {
                period = period.max(min.saturating_mul(1000));
            }
            if let Some(max) = sensor["max_delay_us"].as_u64().filter(|n| *n > 0) {
                period = period.min(max.saturating_mul(1000));
            }
            if period > 60_000_000_000 {
                return Err("profile period exceeds backend limit".into());
            }
            v["period_ns"] = period.into();
        }
    }
    Ok(selection)
}

/// Build the opt-in full plan from live HAL inventory. The selected latency
/// step is intentionally requested even when advertised FIFO reservation is
/// smaller or zero: this is a bounded experiment, not a capacity guarantee.
pub fn buffered_full_trial_plan(
    inventory: &Value,
    requested_latency_ns: u64,
) -> Result<Vec<Value>> {
    if !BUFFERED_FULL_LATENCY_STEPS_NS.contains(&requested_latency_ns) {
        return Err("trial latency must be 7, 20, or 40 seconds".into());
    }
    let fallback_ns = requested_latency_ns
        .checked_add(BUFFERED_FULL_FALLBACK_MARGIN_NS)
        .ok_or("trial fallback overflow")?;
    let mut selection = plan(inventory, "full")?;
    let mut buffered = 0usize;
    for item in &mut selection {
        let sensor = item
            .get("sensor")
            .ok_or("selected sensor lacks descriptor")?;
        let flags = sensor["flags"].as_u64().ok_or("missing sensor flags")?;
        if flags & 1 == 0 {
            return Err("buffered full trial requires wakeup descriptors".into());
        }
        let reserved = sensor["fifo_reserved"].as_u64();
        let maximum = sensor["fifo_max"].as_u64();
        let fifo_known = match (reserved, maximum) {
            (Some(reserved), Some(maximum)) if reserved <= maximum => true,
            (Some(_), Some(_)) => return Err("contradictory sensor FIFO capacity".into()),
            (None, None) if sensor["fifo_reserved"].is_null() && sensor["fifo_max"].is_null() => {
                false
            }
            _ => return Err("incomplete sensor FIFO metadata".into()),
        };
        let period = item["period_ns"]
            .as_u64()
            .filter(|p| *p > 0)
            .ok_or("invalid selected sensor period")?;
        let reporting_mode = (flags >> 1) & 3;
        let (latency, mode) = if reporting_mode == 0 {
            // Period-align the request. Do not clamp to FIFO metadata: the
            // 7/20/40 s steps exist to probe where delivery begins to fail.
            let events = requested_latency_ns / period;
            let latency = events.checked_mul(period).ok_or("batch latency overflow")?;
            if latency > 0 {
                buffered += 1;
            }
            let mode = if !fifo_known {
                "trial_fifo_metadata_unknown"
            } else if reserved == Some(0) {
                "trial_unreserved_fifo"
            } else if reserved
                .unwrap()
                .checked_mul(period)
                .ok_or("FIFO capacity overflow")?
                >= latency
            {
                "advertised_reserved_fifo_window"
            } else {
                "trial_over_reserved_fifo"
            };
            (latency, mode)
        } else {
            (0, "non_continuous_immediate")
        };
        item["latency_ns"] = json!(latency);
        item["batching_mode"] = json!(mode);
    }
    if buffered == 0 || !buffered_full_trial_safe(&selection, requested_latency_ns, fallback_ns) {
        return Err("full plan lacks a bounded wakeup trial batch".into());
    }
    Ok(selection)
}

/// Revalidate the exact experimental plan before it can release the powerd
/// inhibitor. FIFO metadata classifies uncertainty; it does not cap requests.
pub fn buffered_full_trial_safe(
    plan: &[Value],
    requested_latency_ns: u64,
    fallback_ns: u64,
) -> bool {
    !plan.is_empty()
        && BUFFERED_FULL_LATENCY_STEPS_NS.contains(&requested_latency_ns)
        && fallback_ns == requested_latency_ns.saturating_add(BUFFERED_FULL_FALLBACK_MARGIN_NS)
        && plan
            .iter()
            .any(|v| v["latency_ns"].as_u64().is_some_and(|n| n > 0))
        && plan.iter().all(|v| {
            let sensor = &v["sensor"];
            let flags = match sensor["flags"].as_u64() {
                Some(f) if f & 1 != 0 => f,
                _ => return false,
            };
            let reserved = match (
                sensor["fifo_reserved"].as_u64(),
                sensor["fifo_max"].as_u64(),
            ) {
                (Some(reserved), Some(maximum)) if reserved <= maximum => {
                    Some(reserved)
                }
                (None, None)
                    if sensor["fifo_reserved"].is_null() && sensor["fifo_max"].is_null() =>
                {
                    None
                }
                _ => return false,
            };
            let period = match v["period_ns"].as_u64() {
                Some(n) if n > 0 => n,
                _ => return false,
            };
            let latency = match v["latency_ns"].as_u64() {
                Some(n) => n,
                None => return false,
            };
            let mode = (flags >> 1) & 3;
            let expected_latency = (requested_latency_ns / period) * period;
            if fallback_ns > 60_000_000_000
                || latency > requested_latency_ns
                || latency >= fallback_ns
            {
                return false;
            }
            if mode == 0 {
                if latency != expected_latency || latency == 0 || latency % period != 0 {
                    return false;
                }
                match reserved {
                    None => v["batching_mode"] == "trial_fifo_metadata_unknown",
                    Some(0) => v["batching_mode"] == "trial_unreserved_fifo",
                    Some(reserved) => {
                        let capacity_ns = match reserved.checked_mul(period) {
                            Some(n) => n,
                            None => return false,
                        };
                        if latency <= capacity_ns {
                            v["batching_mode"] == "advertised_reserved_fifo_window"
                        } else {
                            v["batching_mode"] == "trial_over_reserved_fifo"
                        }
                    }
                }
            } else {
                latency == 0 && v["batching_mode"] == "non_continuous_immediate"
            }
        })
}

/// The legacy immediate-delivery condition. Kept for non-trial profiles.
pub fn wake_safe(plan: &[Value]) -> bool {
    !plan.is_empty()
        && plan.iter().all(|v| {
            v["sensor"]["flags"].as_u64().is_some_and(|f| f & 1 != 0) && v["latency_ns"] == 0
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sensor(typ: i64, handle: i64, flags: i64, reserved: u64, maximum: u64) -> Value {
        json!({"type":typ,"handle":handle,"flags":flags,"min_delay_us":0,
            "max_delay_us":0,"fifo_reserved":reserved,"fifo_max":maximum})
    }

    #[test]
    fn ordinary_profiles_stay_immediate_and_do_not_infer_fifo() {
        let inventory = json!({"sensors":[
            sensor(19,1,1,300,10000), sensor(1,2,0,2000,10000)]});
        let daily = plan(&inventory, "daily").unwrap();
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0]["latency_ns"], 0);
        assert!(wake_safe(&daily));
        let full = plan(&inventory, "full").unwrap();
        assert_eq!(full.len(), 2);
        assert!(!wake_safe(&full));
        assert!(plan(&inventory, "off").is_err());
    }

    #[test]
    fn trial_caps_continuous_batches_and_keeps_other_modes_immediate() {
        let inventory = json!({"sensors":[
            sensor(1,1,1,2000,10000), // 40 ms; seven seconds is below reserve capacity.
            sensor(4,2,1,0,10000),    // Trial-only request; no reserve guarantee.
            sensor(21,3,3,300,10000)]}); // On-change.
        let p = buffered_full_trial_plan(&inventory, 7_000_000_000).unwrap();
        assert_eq!(p[0]["latency_ns"], 7_000_000_000u64);
        assert_eq!(p[0]["batching_mode"], "advertised_reserved_fifo_window");
        assert_eq!(p[1]["latency_ns"], 7_000_000_000u64);
        assert_eq!(p[1]["batching_mode"], "trial_unreserved_fifo");
        assert_eq!(p[2]["latency_ns"], 0);
        assert_eq!(p[2]["batching_mode"], "non_continuous_immediate");
        assert!(buffered_full_trial_safe(&p, 7_000_000_000, 17_000_000_000));
    }

    #[test]
    fn capacity_and_fallback_bound_latency_even_when_cap_is_higher() {
        let inventory = json!({"sensors":[sensor(1,1,1,20,100)]});
        let p = buffered_full_trial_plan(&inventory, 7_000_000_000).unwrap();
        assert_eq!(p[0]["latency_ns"], 7_000_000_000u64); // Exceeds reserve window intentionally.
        assert_eq!(p[0]["batching_mode"], "trial_over_reserved_fifo");
        let mut invalid = p.clone();
        invalid[0]["latency_ns"] = json!(17_000_000_000u64);
        assert!(!buffered_full_trial_safe(
            &invalid,
            7_000_000_000,
            17_000_000_000
        ));
        invalid = p.clone();
        invalid[0]["latency_ns"] = json!(7_040_000_000u64);
        assert!(!buffered_full_trial_safe(
            &invalid,
            7_000_000_000,
            17_000_000_000
        ));
        assert!(buffered_full_trial_plan(&inventory, 20_000_000_000).is_ok());
        assert!(buffered_full_trial_plan(&inventory, 40_000_000_000).is_ok());
        assert!(buffered_full_trial_plan(&inventory, 9_000_000_000).is_err());
    }

    #[test]
    fn trial_fails_closed_for_missing_or_inconsistent_fifo_and_nonwakeup_channels() {
        let no_metadata = json!({"sensors":[{"type":1,"handle":1,"flags":1,
            "min_delay_us":0,"max_delay_us":0}]});
        let plan = buffered_full_trial_plan(&no_metadata, 7_000_000_000).unwrap();
        assert_eq!(plan[0]["batching_mode"], "trial_fifo_metadata_unknown");
        let partial = json!({"sensors":[{"type":1,"handle":1,"flags":1,
            "min_delay_us":0,"max_delay_us":0,"fifo_reserved":10}]});
        assert!(buffered_full_trial_plan(&partial, 7_000_000_000).is_err());
        let inconsistent = json!({"sensors":[sensor(1,1,1,101,100)]});
        assert!(buffered_full_trial_plan(&inconsistent, 7_000_000_000).is_err());
        let nonwakeup = json!({"sensors":[sensor(1,1,0,100,200)]});
        assert!(buffered_full_trial_plan(&nonwakeup, 7_000_000_000).is_err());
        let no_continuous = json!({"sensors":[sensor(21,1,3,300,10000)]});
        assert!(buffered_full_trial_plan(&no_continuous, 7_000_000_000).is_err());
    }
}
