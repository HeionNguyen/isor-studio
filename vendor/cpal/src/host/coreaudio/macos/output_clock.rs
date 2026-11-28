// isor-studio modification to CPAL 0.15.3, licensed under Apache-2.0.
use crate::{BackendSpecificError, StreamInstant};

/// Only these two integers cross into the CoreAudio output callback.
pub(super) struct OutputClock {
    numer: u32,
    denom: u32,
}

impl OutputClock {
    pub(super) fn query() -> Result<Self, BackendSpecificError> {
        let mut info = mach2::mach_time::mach_timebase_info::default();
        let status = unsafe { mach2::mach_time::mach_timebase_info(&mut info) };
        Self::from_query(status, info.numer, info.denom)
    }

    fn from_query(status: i32, numer: u32, denom: u32) -> Result<Self, BackendSpecificError> {
        super::check_os_status(status)?;
        // CoreAudio supplies at most u32::MAX bytes per buffer. Even at one
        // frame per second, adding its latency must fit in StreamInstant.
        let max_seconds = i64::MAX as u128 - u128::from(u32::MAX) - 1;
        if numer == 0
            || denom == 0
            || u128::from(u64::MAX) * u128::from(numer) / u128::from(denom) / 1_000_000_000
                > max_seconds
        {
            return Err(BackendSpecificError {
                description: "invalid Mach output timebase".into(),
            });
        }
        Ok(Self { numer, denom })
    }

    pub(super) fn instant(&self, ticks: u64) -> StreamInstant {
        let nanos = u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom);
        StreamInstant::new(
            (nanos / 1_000_000_000) as i64,
            (nanos % 1_000_000_000) as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_or_unrepresentable_timebases_never_install_a_callback() {
        for _ in 0..4096 {
            assert!(OutputClock::from_query(1, 1, 1).is_err());
            assert!(OutputClock::from_query(0, 0, 1).is_err());
            assert!(OutputClock::from_query(0, 1, 0).is_err());
            assert!(OutputClock::from_query(0, u32::MAX, 1).is_err());
        }
        assert!(!std::mem::needs_drop::<OutputClock>());
        assert_eq!(std::mem::size_of::<OutputClock>(), 8);
    }

    #[test]
    fn tick_conversion_covers_the_entire_counter_range() {
        let clock = OutputClock::from_query(0, 125, 3).expect("valid 24 MHz timebase");
        for ticks in [0, 1, 24_000_000, u64::MAX - 1, u64::MAX] {
            assert_eq!(clock.instant(ticks).as_nanos(), i128::from(ticks) * 125 / 3);
        }
    }
}
