//! Shared endpoint-completion wait with timeout for nusb-backed programmers.
//!
//! nusb's async `next_complete()` has no deadline of its own, so this helper
//! races it against a platform timer (`futures-timer` natively, `setTimeout`
//! via `gloo-timers` on WASM). On timeout the transfer stays pending in the
//! endpoint queue, matching nusb's blocking `wait_next_complete` semantics;
//! callers treat `None` as a fatal device timeout.

use std::time::Duration;

use nusb::Endpoint;
use nusb::transfer::{BulkOrInterrupt, Completion, EndpointDirection};

/// Extension trait adding a deadline-bounded completion wait to [`Endpoint`].
pub(crate) trait EpWaitExt {
    /// Wait for the next completion, giving up after `timeout`.
    ///
    /// Returns `None` when the deadline expires before a transfer completes.
    async fn next_complete_timeout(&mut self, timeout: Duration) -> Option<Completion>;
}

impl<EpType, Dir> EpWaitExt for Endpoint<EpType, Dir>
where
    EpType: BulkOrInterrupt,
    Dir: EndpointDirection,
{
    async fn next_complete_timeout(&mut self, timeout: Duration) -> Option<Completion> {
        let deadline = async {
            #[cfg(not(target_arch = "wasm32"))]
            futures_timer::Delay::new(timeout).await;
            #[cfg(target_arch = "wasm32")]
            gloo_timers::future::sleep(timeout).await;
            None
        };

        futures_lite::future::or(async { Some(self.next_complete().await) }, deadline).await
    }
}
