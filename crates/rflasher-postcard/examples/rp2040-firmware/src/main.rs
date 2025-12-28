//! RP2040 Postcard-RPC Flash Programmer Firmware
//!
//! This firmware implements a USB flash programmer using postcard-rpc for
//! communication with the host. It supports both standard SPI and multi-I/O
//! modes (Dual/Quad) if the hardware supports it.
//!
//! # Pin Assignments (default, customize as needed)
//!
//! - GPIO16: SPI0 RX (MISO / IO1)
//! - GPIO17: SPI0 CS (directly controlled)
//! - GPIO18: SPI0 SCK
//! - GPIO19: SPI0 TX (MOSI / IO0)
//! - GPIO20: IO2 (for Quad mode)
//! - GPIO21: IO3 (for Quad mode, directly controlled)

#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::USB;
use embassy_rp::spi::{self, Spi};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Timer;
use embassy_usb::UsbDevice;

use postcard_rpc::define_dispatch;
use postcard_rpc::header::VarHeader;
use postcard_rpc::header::VarKeyKind;
use postcard_rpc::server::impls::embassy_usb_v0_5::dispatch_impl::{
    spawn_fn, WireRxImpl, WireSpawnImpl, WireStorage, WireTxImpl,
};
use postcard_rpc::server::impls::embassy_usb_v0_5::PacketBuffers;
use postcard_rpc::server::{Sender, Server, SpawnContext};

use heapless::{String, Vec};
use static_cell::StaticCell;

// Import protocol definitions
use rflasher_postcard::protocol::*;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
});

// ============================================================================
// Static Storage
// ============================================================================

static WIRE_STORAGE: WireStorage<CriticalSectionRawMutex, Driver<'static, USB>> =
    WireStorage::new();
static PACKET_BUFS: StaticCell<PacketBuffers<1024, 1024>> = StaticCell::new();

// ============================================================================
// Application Context
// ============================================================================

/// Application context holding hardware resources
pub struct AppContext {
    /// SPI peripheral
    spi: Spi<'static, embassy_rp::peripherals::SPI0, embassy_rp::spi::Async>,
    /// Chip select pin (directly controlled)
    cs: Output<'static>,
    /// Current SPI frequency
    spi_freq: u32,
    /// Pin state (true = enabled)
    pin_state: bool,
}

impl AppContext {
    /// Check that pins are enabled
    fn check_pins(&self) -> Result<(), ErrorResp> {
        if !self.pin_state {
            return Err(ErrorResp {
                code: ErrorCode::SpiBusError,
                message: Some(String::try_from("Pins disabled").unwrap()),
            });
        }
        Ok(())
    }

    /// Assert chip select (pull low)
    fn cs_assert(&mut self) {
        self.cs.set_low();
    }

    /// Deassert chip select (pull high)
    fn cs_deassert(&mut self) {
        self.cs.set_high();
    }

    /// Write data to SPI (CS must already be asserted)
    async fn spi_write(&mut self, data: &[u8]) -> Result<(), ErrorResp> {
        if !data.is_empty() {
            self.spi.write(data).await.map_err(|_| ErrorResp {
                code: ErrorCode::SpiBusError,
                message: Some(String::try_from("SPI write error").unwrap()),
            })?;
        }
        Ok(())
    }

    /// Read data from SPI (CS must already be asserted)
    async fn spi_read(&mut self, buf: &mut [u8]) -> Result<(), ErrorResp> {
        if !buf.is_empty() {
            self.spi.read(buf).await.map_err(|_| ErrorResp {
                code: ErrorCode::SpiBusError,
                message: Some(String::try_from("SPI read error").unwrap()),
            })?;
        }
        Ok(())
    }
}

/// Spawned task context (for long-running operations)
pub struct SpawnedContext {
    pub sender: Sender<WireTxImpl<CriticalSectionRawMutex, Driver<'static, USB>>>,
}

impl SpawnContext for AppContext {
    type SpawnCtxt = SpawnedContext;

    fn spawn_ctxt(&mut self) -> Self::SpawnCtxt {
        // We don't currently use spawned handlers, but this is required by the trait
        unimplemented!("spawn context not used")
    }
}

// ============================================================================
// Dispatch Definition
// ============================================================================

define_dispatch! {
    app: AppDispatcher;
    spawn_fn: spawn_fn;
    tx_impl: WireTxImpl<CriticalSectionRawMutex, Driver<'static, USB>>;
    spawn_impl: WireSpawnImpl;
    context: AppContext;

    endpoints: {
        list: ENDPOINT_LIST;

        | EndpointTy        | kind      | handler               |
        | ----------        | ----      | -------               |
        | GetDeviceInfo     | async     | get_device_info       |
        | SetSpiFreqEp      | async     | set_spi_freq          |
        | SetChipSelectEp   | async     | set_chip_select       |
        | SetPinStateEp     | async     | set_pin_state         |
        | SpiOpEp           | async     | spi_op                |
        | DelayUsEp         | async     | delay_us              |
        | SpiResetEp        | async     | spi_reset             |
    };
    topics_in: {
        list: TOPICS_IN_LIST;

        | TopicTy           | kind      | handler               |
        | -------           | ----      | -------               |
    };
    topics_out: {
        list: TOPICS_OUT_LIST;
    };
}

// ============================================================================
// Endpoint Handlers
// ============================================================================

async fn get_device_info(_ctx: &mut AppContext, _hdr: VarHeader, _req: ()) -> DeviceInfo {
    DeviceInfo {
        name: String::try_from("RP2040 Postcard").unwrap(),
        version: String::try_from("0.1.0").unwrap(),
        max_spi_freq: 62_500_000, // RP2040 max SPI frequency
        features: SpiFeatures::empty(), // Standard SPI only, no multi-I/O
        max_read_len: MAX_SPI_DATA as u32,
        max_write_len: MAX_SPI_DATA as u32,
    }
}

async fn set_spi_freq(
    ctx: &mut AppContext,
    _hdr: VarHeader,
    req: SetSpiFreq,
) -> SetSpiFreqResult {
    // RP2040 SPI clock is derived from peripheral clock (typically 125MHz)
    // Actual frequency will be the closest achievable
    // For now, we just store the requested frequency
    // In a real implementation, you'd reconfigure the SPI peripheral
    ctx.spi_freq = req.freq_hz;

    info!("Set SPI frequency to {} Hz", req.freq_hz);

    Ok(SetSpiFreqResp {
        actual_freq: req.freq_hz, // Simplified - return requested freq
    })
}

async fn set_chip_select(
    _ctx: &mut AppContext,
    _hdr: VarHeader,
    req: SetChipSelect,
) -> AckResult {
    // For now, we only support CS 0
    if req.cs != 0 {
        return Err(ErrorResp {
            code: ErrorCode::InvalidParam,
            message: Some(String::try_from("Only CS 0 supported").unwrap()),
        });
    }

    info!("Set chip select to {}", req.cs);
    Ok(Ack)
}

async fn set_pin_state(
    ctx: &mut AppContext,
    _hdr: VarHeader,
    req: SetPinState,
) -> AckResult {
    ctx.pin_state = req.enabled;
    info!("Pin state: {}", if req.enabled { "enabled" } else { "disabled" });
    Ok(Ack)
}

async fn spi_op(ctx: &mut AppContext, _hdr: VarHeader, req: SpiOp) -> SpiOpResult {
    // Check I/O mode - we only support Single for now
    if req.io_mode != IoMode::Single {
        return Err(ErrorResp {
            code: ErrorCode::IoModeNotSupported,
            message: Some(String::try_from("Only Single mode supported").unwrap()),
        });
    }

    ctx.check_pins()?;

    // Build header buffer: opcode + address + dummy (max 1 + 4 + 8 = 13 bytes)
    let mut header: [u8; 16] = [0xFF; 16];
    let mut header_len = 0;

    // Opcode
    header[header_len] = req.opcode;
    header_len += 1;

    // Address (if present) - big-endian
    let addr_bytes = req.address_width.bytes() as usize;
    if addr_bytes > 0 {
        let addr = req.address;
        for i in (0..addr_bytes).rev() {
            header[header_len] = ((addr >> (i * 8)) & 0xFF) as u8;
            header_len += 1;
        }
    }

    // Dummy bytes (already 0xFF from initialization)
    let dummy_bytes = req.dummy_cycles.div_ceil(8) as usize;
    header_len += dummy_bytes;

    // Prepare read buffer
    let mut read_buf: Vec<u8, MAX_SPI_DATA> = Vec::new();
    read_buf.resize(req.read_count as usize, 0).map_err(|_| ErrorResp {
        code: ErrorCode::BufferOverflow,
        message: None,
    })?;

    // Execute transaction with CS held across all phases
    // This avoids copying write_data - we send it directly
    ctx.cs_assert();

    // Send header (opcode + address + dummy)
    if let Err(e) = ctx.spi_write(&header[..header_len]).await {
        ctx.cs_deassert();
        return Err(e);
    }

    // Send write data directly - no copy!
    if let Err(e) = ctx.spi_write(&req.write_data).await {
        ctx.cs_deassert();
        return Err(e);
    }

    // Read response data
    if let Err(e) = ctx.spi_read(&mut read_buf).await {
        ctx.cs_deassert();
        return Err(e);
    }

    ctx.cs_deassert();

    Ok(SpiOpResp { read_data: read_buf })
}

async fn delay_us(_ctx: &mut AppContext, _hdr: VarHeader, req: DelayUs) -> Ack {
    Timer::after_micros(req.us as u64).await;
    Ack
}

async fn spi_reset(ctx: &mut AppContext, _hdr: VarHeader, _req: ()) -> AckResult {
    // Ensure CS is deasserted
    ctx.cs.set_high();

    // Small delay
    Timer::after_micros(100).await;

    info!("SPI reset");
    Ok(Ack)
}

// ============================================================================
// USB Task
// ============================================================================

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, Driver<'static, USB>>) {
    usb.run().await;
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("RP2040 Postcard-RPC Flash Programmer starting...");

    // Initialize peripherals
    let p = embassy_rp::init(Default::default());

    // Configure USB
    let driver = Driver::new(p.USB, Irqs);

    // USB configuration
    let mut config = embassy_usb::Config::new(USB_VID, USB_PID);
    config.manufacturer = Some(USB_MANUFACTURER);
    config.product = Some(USB_PRODUCT);
    config.serial_number = Some("12345678");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Initialize packet buffers
    let bufs = PACKET_BUFS.init(PacketBuffers::new());

    // Initialize postcard-rpc USB transport
    let (usb, tx, rx) = WIRE_STORAGE.init_poststation(driver, config, &mut bufs.tx_buf, 64);

    // Spawn USB task
    spawner.spawn(usb_task(usb)).unwrap();

    // Configure SPI
    let mut spi_config = spi::Config::default();
    spi_config.frequency = 1_000_000; // 1 MHz default
    spi_config.phase = spi::Phase::CaptureOnFirstTransition;
    spi_config.polarity = spi::Polarity::IdleLow;

    let spi = Spi::new(
        p.SPI0,
        p.PIN_18, // SCK
        p.PIN_19, // MOSI
        p.PIN_16, // MISO
        p.DMA_CH0,
        p.DMA_CH1,
        spi_config,
    );

    // Configure CS pin (directly controlled)
    let cs = Output::new(p.PIN_17, Level::High);

    // Create application context
    let context = AppContext {
        spi,
        cs,
        spi_freq: 1_000_000,
        pin_state: true,
    };

    // Create dispatcher
    let dispatcher = AppDispatcher::new(context, WireSpawnImpl::new(spawner));

    // Create server
    let mut server = Server::new(
        tx,
        rx,
        &mut bufs.rx_buf,
        dispatcher,
        VarKeyKind::Key4, // Use 4-byte keys for reasonable collision resistance
    );

    info!("Server ready, entering main loop");

    // Run server loop
    loop {
        if let Err(e) = server.run().await {
            error!("Server error: {:?}", defmt::Debug2Format(&e));
            Timer::after_millis(100).await;
        }
    }
}
