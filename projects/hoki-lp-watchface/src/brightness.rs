//! Manual BG brightness configuration, ordered as stock SidekickService.
//! Alpha80 is from the original wear-resources APK. Levels127/64 are bounded
//! diagnostic choices, not inferred from getLastBrightness or stock defaults.
pub const BRIGHT: u16 = 127;
pub const DIM: u16 = 64;
pub const ALPHA: f32 = 80.0;
pub trait Transport {
    fn levels(&self, bright: u16, dim: u16) -> Result<(), String>;
    fn als_off(&self, alpha: f32) -> Result<(), String>;
}
pub fn configure(t: &impl Transport) -> Result<(), String> {
    t.levels(BRIGHT, DIM)?;
    // OFF disables automatic sensing; this call still transmits cached levels.
    t.als_off(ALPHA)
}
#[cfg(test)] mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Mock { events: RefCell<Vec<String>>, fail_levels: bool, fail_als: bool }
    impl Transport for Mock {
        fn levels(&self, bright:u16, dim:u16)->Result<(),String> {
            self.events.borrow_mut().push(format!("levels {bright} {dim}"));
            if self.fail_levels {Err("levels".into())} else {Ok(())}
        }
        fn als_off(&self, alpha:f32)->Result<(),String> {
            self.events.borrow_mut().push(format!("als_off {alpha}"));
            if self.fail_als {Err("als".into())} else {Ok(())}
        }
    }
    #[test] fn sends_levels_before_off_mode_and_propagates_errors() {
        for (fail_levels,fail_als) in [(false,false),(true,false),(false,true)] {
            let t=Mock{events:RefCell::new(vec![]),fail_levels,fail_als};
            assert_eq!(configure(&t).is_ok(),!fail_levels && !fail_als);
            let expected=if fail_levels {vec!["levels 127 64"]} else {vec!["levels 127 64","als_off 80"]};
            assert_eq!(*t.events.borrow(),expected);
        }
    }
}
