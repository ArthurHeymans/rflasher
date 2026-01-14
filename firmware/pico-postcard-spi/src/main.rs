//! Postcard-SPI programmer firmware for Raspberry Pi Pico
//!
//! This firmware implements a multi-I/O SPI programmer using postcard-rpc
//! over USB. It supports standard SPI (1-1-1) and multi-I/O modes using
//! the RP2040's PIO peripheral.
//!
//! ## Pin Assignments
//!
//! | Pin   | Function      |
//! |-------|---------------|
//! | GP2   | SCK (clock)   |
//! | GP3   | CS0           |
//! | GP4   | CS1           |
//! | GP5   | IO0 (MOSI)    |
//! | GP6   | IO1 (MISO)    |
//! | GP7   | IO2           |
//! | GP8   | IO3           |

#![no_std]
#![no_main]

mod handlers;
mod pio_qspi;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_usb::UsbDevice;
use postcard_rpc::{
    define_dispatch,
    server::{
        impls::embassy_usb_v0_5::{
            dispatch_impl::{WireRxBuf, WireRxImpl, WireSpawnImpl, WireStorage, WireTxImpl},
            PacketBuffers,
        },
        Dispatch, Server,
    },
};
use postcard_spi_icd::*;
use static_cell::ConstStaticCell;
use {defmt_rtt as _, panic_probe as _};

use crate::handlers::*;
use crate::pio_qspi::PioQspi;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
});

// USB and RPC type definitions
type AppDriver = Driver<'static, USB>;
type AppStorage = WireStorage<CriticalSectionRawMutex, AppDriver, 256, 256, 64, 256>;
type BufStorage = PacketBuffers<2048, 2048>;
type AppTx = WireTxImpl<CriticalSectionRawMutex, AppDriver>;
type AppRx = WireRxImpl<AppDriver>;
type AppServer = Server<AppTx, AppRx, WireRxBuf, MyApp>;

static PBUFS: ConstStaticCell<BufStorage> = ConstStaticCell::new(BufStorage::new());
static STORAGE: AppStorage = AppStorage::new();

/// Maximum transfer size (limited by RAM)
pub const MAX_TRANSFER_SIZE: u32 = 1024;

/// Number of chip select lines
pub const NUM_CS: u8 = 2;

/// Device name
pub const DEVICE_NAME: &[u8; 16] = b"pico-qspi\0\0\0\0\0\0\0";

/// Shared context for RPC handlers
pub struct Context {
    /// PIO-based QSPI driver
    pub qspi: PioQspi,
    /// Chip select pins
    pub cs_pins: [Output<'static>; 2],
    /// Currently selected CS
    pub current_cs: u8,
    /// Current SPI speed in Hz
    pub current_speed_hz: u32,
}

impl Context {
    /// Assert the currently selected chip select (active low)
    pub fn cs_assert(&mut self) {
        let cs = self.current_cs as usize;
        if cs < self.cs_pins.len() {
            self.cs_pins[cs].set_low();
        }
    }

    /// Deassert the currently selected chip select
    pub fn cs_deassert(&mut self) {
        let cs = self.current_cs as usize;
        if cs < self.cs_pins.len() {
            self.cs_pins[cs].set_high();
        }
    }

    /// Deassert all chip selects
    pub fn cs_deassert_all(&mut self) {
        for pin in &mut self.cs_pins {
            pin.set_high();
        }
    }
}

fn usb_config() -> embassy_usb::Config<'static> {
    let mut config = embassy_usb::Config::new(USB_VID, USB_PID);
    config.manufacturer = Some("rflasher");
    config.product = Some("pico-postcard-spi");
    config.serial_number = Some("00000001");

    // Required for composite devices with IADs (Interface Association Descriptors)
    config.device_class = 0xEF; // Miscellaneous
    config.device_sub_class = 0x02; // Common Class
    config.device_protocol = 0x01; // IAD

    config
}

// Define the RPC dispatch table
define_dispatch! {
    app: MyApp;
    spawn_fn: spawn_fn;
    tx_impl: AppTx;
    spawn_impl: WireSpawnImpl;
    context: Context;

    endpoints: {
        list: postcard_spi_icd::ENDPOINT_LIST;

        | EndpointTy                | kind      | handler                       |
        | ----------                | ----      | -------                       |
        | GetInfoEndpoint           | blocking  | get_info_handler              |
        | SetSpeedEndpoint          | blocking  | set_speed_handler             |
        | BatchEndpoint             | blocking  | batch_handler                 |
    };
    topics_in: {
        list: postcard_spi_icd::TOPICS_IN_LIST;

        | TopicTy                   | kind      | handler                       |
        | ----------                | ----      | -------                       |
    };
    topics_out: {
        list: postcard_spi_icd::TOPICS_OUT_LIST;
    };
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("pico-postcard-spi starting...");

    let p = embassy_rp::init(Default::default());

    // Initialize chip select pins (active low, start high/deasserted)
    let cs0 = Output::new(p.PIN_5, Level::High);
    let cs1 = Output::new(p.PIN_9, Level::High);

    // Initialize PIO QSPI driver (bit-banged GPIO for now)
    let qspi = PioQspi::new(
        p.PIN_2, // SCK
        p.PIN_3, // IO0 (MOSI)
        p.PIN_4, // IO1 (MISO)
        p.PIN_7, // IO2
        p.PIN_8, // IO3
    );

    info!("QSPI driver initialized");

    // Create context
    let context = Context {
        qspi,
        cs_pins: [cs0, cs1],
        current_cs: 0,
        current_speed_hz: 1_000_000, // Default 1 MHz
    };
    // Initialize USB
    let driver = Driver::new(p.USB, Irqs);
    let pbufs = PBUFS.take();
    let config = usb_config();

    // USB Full Speed max packet size is 64 bytes
    let (device, tx_impl, rx_impl) = STORAGE.init(driver, config, pbufs.tx_buf.as_mut_slice(), 64);

    let dispatcher = MyApp::new(context, spawner.into());
    let vkk = dispatcher.min_key_len();
    let server: AppServer = Server::new(
        tx_impl,
        rx_impl,
        pbufs.rx_buf.as_mut_slice(),
        dispatcher,
        vkk,
    );

    spawner.must_spawn(usb_task(device));
    spawner.must_spawn(server_task(server));

    info!("pico-postcard-spi ready");
}

/// USB device task
#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, AppDriver>) {
    usb.run().await;
}

/// RPC server task
#[embassy_executor::task]
async fn server_task(mut server: AppServer) {
    loop {
        let _ = server.run().await;
        warn!("Server disconnected, waiting for reconnect...");
    }
}
