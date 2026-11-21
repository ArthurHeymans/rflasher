//! Shared platform-aware async delay helper for programmer backends.
//!
//! Uses `std::thread::sleep` on native targets (which run inside a single
//! `block_on` boundary) and `gloo_timers::future::sleep` on WASM so the
//! browser event loop can run during polling loops.

use std::time::Duration;

/// Sleep for `duration` on both native and WASM targets.
#[allow(dead_code)]
pub(crate) async fn delay(duration: Duration) {
    if duration.is_zero() {
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::sleep(duration);
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    {
        // Browser setTimeout has 1 ms resolution; round positive sub-ms
        // fractions up so requested SPI delays are never shortened.
        let ms = duration
            .as_nanos()
            .div_ceil(1_000_000)
            .min(u32::MAX as u128) as u32;
        gloo_timers::future::TimeoutFuture::new(ms).await;
    }

    #[cfg(all(not(feature = "wasm"), target_arch = "wasm32"))]
    {
        let _ = duration;
    }
}
