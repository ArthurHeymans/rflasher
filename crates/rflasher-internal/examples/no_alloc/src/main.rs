//! Link the firmware dependency graph without supplying a global allocator.
//! cargo build --manifest-path crates/rflasher-internal/examples/no_alloc/Cargo.toml \
//!   --target x86_64-unknown-none
#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use rflasher_internal as _;

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    _start()
}

#[cfg(not(target_os = "none"))]
fn main() {}
