//! The PC's time zone, one copy shared by the providers that show local
//! times (answers and timers). Read at start and again each time the bar is
//! shown, as it may change while Flick runs (travel, a manual change).

use std::sync::{Arc, RwLock};

use jiff::tz::TimeZone;

pub struct LocalZone(RwLock<TimeZone>);

impl LocalZone {
    pub fn new(tz: TimeZone) -> Arc<Self> {
        Arc::new(Self(RwLock::new(tz)))
    }

    /// The zone as last read.
    pub fn get(&self) -> TimeZone {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The zone changed (or may have): memory only. Reminders already set keep
    /// their moment; new ones and the shown times use the new zone.
    pub fn set(&self, tz: TimeZone) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = tz;
    }
}
