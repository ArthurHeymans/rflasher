//! Programmer backends and high-level flash programming abstractions.
//!
//! Backends are selected independently with Cargo features. Native
//! applications normally enable [`portable-programmers`](#features), while
//! the WebAssembly frontend enables `wasm` together with the required backend
//! features. Firmware-oriented internal chipset access remains in the separate
//! `rflasher-internal` crate so it can be used without `std`.
//!
//! All operational APIs are async on every target; native applications block
//! on them at their outermost boundary (e.g. `futures_lite::future::block_on`).

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(async_fn_in_trait)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "ch341a")]
#[path = "backends/ch341a/mod.rs"]
pub mod ch341a;
#[cfg(feature = "ch347")]
#[path = "backends/ch347/mod.rs"]
pub mod ch347;
#[cfg(feature = "dediprog")]
#[path = "backends/dediprog/mod.rs"]
pub mod dediprog;
#[cfg(feature = "dummy")]
#[path = "backends/dummy/mod.rs"]
pub mod dummy;
#[cfg(feature = "ft4222")]
#[path = "backends/ft4222/mod.rs"]
pub mod ft4222;
#[cfg(any(feature = "ftdi", feature = "ftdi-wasm"))]
#[path = "backends/ftdi/mod.rs"]
pub mod ftdi;
#[cfg(all(feature = "linux-gpio", target_os = "linux"))]
#[path = "backends/linux_gpio/mod.rs"]
pub mod linux_gpio;
#[cfg(all(feature = "linux-mtd", target_os = "linux"))]
#[path = "backends/linux_mtd/mod.rs"]
pub mod linux_mtd;
#[cfg(all(feature = "linux-spi", target_os = "linux"))]
#[path = "backends/linux_spi/mod.rs"]
pub mod linux_spi;
#[cfg(feature = "raiden")]
#[path = "backends/raiden/mod.rs"]
pub mod raiden;
#[cfg(feature = "serprog")]
#[path = "backends/serprog/mod.rs"]
pub mod serprog;
#[cfg(feature = "sunxi-fel")]
#[path = "backends/sunxi_fel/mod.rs"]
pub mod sunxi_fel;

// Shared endpoint-completion timeout helper for the nusb-backed programmers.
#[cfg(any(
    feature = "ch341a",
    feature = "ch347",
    feature = "dediprog",
    feature = "ft4222",
    feature = "raiden"
))]
#[path = "backends/usb_ep.rs"]
pub(crate) mod usb_ep;

// The registry and its erasure layer are native-only: WASM frontends select
// and construct their backend explicitly (WebUSB permission flow).
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
mod erased;
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
mod handle;
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
mod registry;

#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub use erased::{ErasedFlashDevice, ErasedSpiMaster};
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub use handle::{ChipInfo, FlashHandle};
#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub use registry::{
    BoxedSpiMaster, ProgrammerInfo, ProgrammerParams, available_programmers, open_flash,
    open_spi_programmer, parse_programmer_params, programmer_names_short,
};

pub use rflasher_core::flash::FlashDevice;
#[cfg(feature = "std")]
pub use rflasher_core::layout::Layout;
