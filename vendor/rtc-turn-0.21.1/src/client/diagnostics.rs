use std::{sync::OnceLock, time::Instant};

pub(crate) fn monotonic_ms() -> u128 {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    STARTED.get_or_init(Instant::now).elapsed().as_millis()
}
