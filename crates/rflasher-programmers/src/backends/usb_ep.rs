//! Shared endpoint-completion helpers for nusb-backed programmers.
//!
//! nusb's async `next_complete()` has no deadline of its own, so this helper
//! races it against a platform timer (`futures-timer` natively, `setTimeout`
//! via `gloo-timers` on WASM). On timeout the transfer stays pending in the
//! endpoint queue, matching nusb's blocking `wait_next_complete` semantics;
//! callers treat `None` as a fatal device timeout.

use std::task::Poll;
use std::time::Duration;

use nusb::Endpoint;
use nusb::transfer::{BulkOrInterrupt, Completion, EndpointDirection};

/// Extension trait adding deadline-bounded wait, non-blocking poll, and
/// cancel/drain cleanup to [`Endpoint`].
#[allow(dead_code)]
pub(crate) trait EpWaitExt {
    /// Wait for the next completion, giving up after `timeout`.
    ///
    /// Returns `None` when the deadline expires before a transfer completes.
    async fn next_complete_timeout(&mut self, timeout: Duration) -> Option<Completion>;

    /// Poll for an already-ready completion without waiting.
    async fn try_next_complete(&mut self) -> Option<Completion>;

    /// Cancel pending transfers (where supported) and drain completions,
    /// waiting up to `timeout` per transfer.
    ///
    /// Returns the number of transfers still pending if draining timed out.
    async fn cancel_and_drain(&mut self, timeout: Duration) -> usize;
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

    async fn try_next_complete(&mut self) -> Option<Completion> {
        std::future::poll_fn(|cx| match self.poll_next_complete(cx) {
            Poll::Ready(c) => Poll::Ready(Some(c)),
            Poll::Pending => Poll::Ready(None),
        })
        .await
    }

    async fn cancel_and_drain(&mut self, timeout: Duration) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        self.cancel_all();
        while self.pending() > 0 {
            if self.next_complete_timeout(timeout).await.is_none() {
                break;
            }
        }
        self.pending()
    }
}
