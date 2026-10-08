//! BG brightness configuration, ordered as stock SidekickService.
//! Alpha80 is from the original wear-resources APK. Levels127/64 are bounded
//! diagnostic choices, not inferred from getLastBrightness or stock defaults.
//! Automatic thresholds are our initial curve, pending physical validation.
pub const BRIGHT: u16 = 127;
pub const DIM: u16 = 64;
pub const ALPHA: f32 = 80.0;
pub trait Transport {
    fn levels(&self, bright: u16, dim: u16) -> Result<(), String>;
    fn als_off(&self, alpha: f32) -> Result<(), String>;
    fn automatic_levels(&self, table: &AutoTable) -> Result<(), String>;
    fn als_on(&self, alpha: f32) -> Result<(), String>;
}
/// Four hysteretic transitions between five brightness bands. The face's
/// existing levels remain the ceiling; no band exceeds its manual brightness.
pub struct AutoTable {
    pub down: Vec<u16>,
    pub up: Vec<u16>,
    pub bright: Vec<u16>,
    pub dim: Vec<u16>,
}
impl AutoTable {
    pub fn new(bright: u16, dim: u16) -> Result<Self, String> {
        if bright > 255 || dim > bright { return Err("invalid ambient brightness levels".into()); }
        let scale = |maximum: u16| [20u32, 35, 55, 80, 100].map(|p| {
            if maximum == 0 { 0 } else { ((u32::from(maximum) * p / 100).max(1)) as u16 }
        }).to_vec();
        Ok(Self {
            down: vec![3, 30, 150, 750],
            up: vec![5, 50, 250, 1250],
            bright: scale(bright), dim: scale(dim),
        })
    }
}
pub fn configure_face(t: &impl Transport, bright: u16, dim: u16, automatic: bool) -> Result<(), String> {
    if automatic {
        t.automatic_levels(&AutoTable::new(bright, dim)?)?;
        t.als_on(ALPHA)
    } else {
        t.levels(bright, dim)?;
        t.als_off(ALPHA)
    }
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
        fn automatic_levels(&self, table: &AutoTable)->Result<(),String> {
            assert_eq!(table.bright.len(), table.up.len()+1);
            self.events.borrow_mut().push("automatic levels".into());
            if self.fail_levels { Err("levels".into()) } else { Ok(()) }
        }
        fn als_on(&self, alpha:f32)->Result<(),String> {
            self.events.borrow_mut().push(format!("als_on {alpha}"));
            if self.fail_als { Err("als".into()) } else { Ok(()) }
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
    #[test] fn automatic_mode_orders_configuration_and_bounds_levels() {
        for (bright, dim) in [(0,0), (1,1), (127,64), (255,255)] {
            let table=AutoTable::new(bright,dim).unwrap();
            assert_eq!(table.down.len(),table.up.len());
            assert_eq!(table.bright.len(),table.dim.len());
            assert_eq!(table.bright.len(),table.up.len()+1);
            assert!(table.down.iter().zip(&table.up).all(|(d,u)|d<u));
            assert!(table.bright.windows(2).all(|w| w[0]<=w[1]));
            assert!(table.dim.iter().zip(&table.bright).all(|(d,b)|d<=b));
            assert_eq!(table.bright.last(),Some(&bright));
            assert_eq!(table.dim.last(),Some(&dim));
        }
        assert!(AutoTable::new(256,0).is_err());
        assert!(AutoTable::new(20,21).is_err());
        for (fail_levels,fail_als) in [(false,false),(true,false),(false,true)] {
            let t=Mock{events:RefCell::new(vec![]),fail_levels,fail_als};
            assert_eq!(configure_face(&t,127,64,true).is_ok(),!fail_levels && !fail_als);
            let expected=if fail_levels {vec!["automatic levels"]} else {vec!["automatic levels","als_on 80"]};
            assert_eq!(*t.events.borrow(),expected);
        }
    }
}
