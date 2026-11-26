//! EZP2013 (`ezp_spi`) device implementation
//!
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.

use std::time::Duration;

use nusb::Endpoint;
use nusb::transfer::{Buffer, Bulk, ControlIn, ControlType, In, Out, Recipient, TransferError};
use rflasher_core::chip::{Features, FlashChip, WriteGranularity};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::flash::{AddressMode, FlashContext};
use rflasher_core::programmer::{FlashAccess, SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, IoMode, SpiCommand, opcodes};

use super::error::{EzpSpiError, Result};
use super::protocol::*;
use crate::time::delay;
use crate::usb_ep::EpWaitExt;

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;

/// USB devices this backend drives.
const USB_SELECTORS: &[nusb::DeviceSelector] =
    &[nusb::DeviceSelector::all().with_vid_pid(EZP_VID, EZP_PID)];

async fn with_timeout<T, Fut: core::future::IntoFuture<Output = T>>(
    timeout: Duration,
    fut: Fut,
) -> Option<T> {
    let timer = async {
        #[cfg(not(target_arch = "wasm32"))]
        futures_timer::Delay::new(timeout).await;
        #[cfg(target_arch = "wasm32")]
        gloo_timers::future::sleep(timeout).await;
        None
    };

    futures_lite::future::or(async { Some(fut.await) }, timer).await
}

// ---------------------------------------------------------------------------
// Internal USB transport abstraction (enables hardware-free mock tests)
// ---------------------------------------------------------------------------

pub(crate) trait EzpTransport {
    fn now(&self) -> web_time::Instant {
        web_time::Instant::now()
    }
    async fn control_in(&mut self, request: u8, max_len: u16, timeout: Duration)
    -> Result<Vec<u8>>;
    async fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<usize>;
    async fn bulk_in(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize>;
    async fn delay_us(&mut self, us: u32);
}

struct NusbTransport {
    interface: nusb::Interface,
    out_ep: Endpoint<Bulk, Out>,
    in_ep: Endpoint<Bulk, In>,
    /// Set before any USB transfer is awaited and cleared only on clean
    /// completion. Any timeout, transport error, or dropped future leaves the
    /// session poisoned so a subsequent command cannot submit behind an
    /// unresolved WebUSB transfer or consume stale firmware payload.
    poisoned: bool,
}

impl NusbTransport {
    async fn open(device_info: nusb::DeviceInfo) -> Result<Self> {
        log::info!(
            "Opening EZP2013 device VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        let device = device_info
            .open()
            .await
            .map_err(|e| EzpSpiError::OpenFailed(e.to_string()))?;

        let interface = device
            .claim_interface(EZP_INTERFACE)
            .await
            .map_err(|e| EzpSpiError::ClaimFailed(e.to_string()))?;

        let out_ep = interface
            .endpoint::<Bulk, Out>(EZP_EP_OUT)
            .map_err(|e| EzpSpiError::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(EZP_EP_IN)
            .map_err(|e| EzpSpiError::ClaimFailed(e.to_string()))?;

        Ok(Self {
            interface,
            out_ep,
            in_ep,
            poisoned: false,
        })
    }

    fn ensure_ready(&self) -> Result<()> {
        if self.poisoned || self.out_ep.pending() > 0 || self.in_ep.pending() > 0 {
            return Err(EzpSpiError::SessionPoisoned);
        }
        Ok(())
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    async fn shutdown(&mut self) {
        self.poisoned = true;
        let timeout = Duration::from_millis(200);
        let _ = self.out_ep.cancel_and_drain(timeout).await;
        let _ = self.in_ep.cancel_and_drain(timeout).await;
    }
}

impl EzpTransport for NusbTransport {
    async fn control_in(
        &mut self,
        request: u8,
        max_len: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.ensure_ready()?;
        if timeout.is_zero() {
            return Err(EzpSpiError::Timeout(format!(
                "control IN 0x{request:02x} budget exhausted"
            )));
        }

        // Poison before awaiting: on WebUSB, nusb ignores the control timeout
        // parameter, so a timed-out or dropped future leaves the browser
        // promise unresolved.
        self.poisoned = true;
        let fut = self.interface.control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request,
                value: 0,
                index: 0,
                length: max_len,
            },
            timeout,
        );

        match with_timeout(timeout, fut).await {
            None => Err(EzpSpiError::Timeout(format!("control IN 0x{request:02x}"))),
            Some(Ok(data)) => {
                self.poisoned = false;
                Ok(data)
            }
            Some(Err(TransferError::Stall)) => {
                // Clean protocol stall completed on the wire; no pending I/O remains.
                self.poisoned = false;
                Err(EzpSpiError::Stall(format!("control IN 0x{request:02x}")))
            }
            Some(Err(TransferError::Cancelled)) => Err(EzpSpiError::Timeout(format!(
                "control IN 0x{request:02x} cancelled/timed out"
            ))),
            Some(Err(e)) => Err(EzpSpiError::TransferFailed(format!(
                "control IN 0x{request:02x}: {e}"
            ))),
        }
    }

    async fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<usize> {
        self.ensure_ready()?;
        if timeout.is_zero() {
            return Err(EzpSpiError::Timeout(
                "bulk OUT transfer budget exhausted".into(),
            ));
        }

        self.poisoned = true;
        let buf = Buffer::from(data.to_vec());
        self.out_ep.submit(buf);

        let Some(completion) = self.out_ep.next_complete_timeout(timeout).await else {
            let _ = self
                .out_ep
                .cancel_and_drain(Duration::from_millis(200))
                .await;
            return Err(EzpSpiError::Timeout("bulk OUT transfer".into()));
        };

        completion
            .status
            .map_err(|e| EzpSpiError::TransferFailed(format!("bulk OUT: {e}")))?;
        if self.out_ep.pending() == 0 && self.in_ep.pending() == 0 {
            self.poisoned = false;
        }
        Ok(completion.actual_len)
    }

    async fn bulk_in(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.ensure_ready()?;
        if timeout.is_zero() {
            return Err(EzpSpiError::Timeout(
                "bulk IN transfer budget exhausted".into(),
            ));
        }

        self.poisoned = true;
        let mps = self.in_ep.max_packet_size().max(EZP_PACKET_SIZE);
        let req_len = buf.len().div_ceil(mps) * mps;
        let mut xfer = Buffer::new(req_len);
        xfer.set_requested_len(req_len);
        self.in_ep.submit(xfer);

        let Some(completion) = self.in_ep.next_complete_timeout(timeout).await else {
            let _ = self
                .in_ep
                .cancel_and_drain(Duration::from_millis(200))
                .await;
            return Err(EzpSpiError::Timeout("bulk IN transfer".into()));
        };

        completion
            .status
            .map_err(|e| EzpSpiError::TransferFailed(format!("bulk IN: {e}")))?;
        if self.out_ep.pending() == 0 && self.in_ep.pending() == 0 {
            self.poisoned = false;
        }
        let n = completion.actual_len.min(buf.len());
        buf[..n].copy_from_slice(&completion.buffer[..n]);
        Ok(n)
    }

    async fn delay_us(&mut self, us: u32) {
        delay(Duration::from_micros(u64::from(us))).await;
    }
}

// ---------------------------------------------------------------------------
// Generic protocol implementation over any EzpTransport
// ---------------------------------------------------------------------------

pub(crate) struct EzpDevice<T> {
    transport: T,
    device_type: String,
    firmware_version: String,
    /// `WREN` (0x06) or `EWSR` (0x50), whichever the caller issued last before
    /// a status-register write (`WRDI` resets to `WREN`).
    pending_wren: u8,
    /// Set while a multi-step firmware exchange is active and cleared only when
    /// the entire exchange finishes cleanly. Prevents a subsequent command from
    /// consuming stale payload or ACK packets after a failure or dropped future.
    poisoned: bool,
    /// Set by `chip_identified` when the identified chip's rated voltage range
    /// excludes the programmer's fixed 3.3 V output.
    voltage_incompatible: bool,
}

impl<T: EzpTransport> EzpDevice<T> {
    pub(crate) async fn init(mut transport: T) -> Result<Self> {
        let timeout = Duration::from_millis(EZP_USB_TIMEOUT_MS);
        let type_bytes = transport
            .control_in(EZP_REQ_TYPE, EZP_PACKET_SIZE as u16, timeout)
            .await?;
        let ver_bytes = transport
            .control_in(EZP_REQ_VERSION, EZP_PACKET_SIZE as u16, timeout)
            .await?;

        let device_type = decode_c_string(&type_bytes);
        let firmware_version = decode_c_string(&ver_bytes);

        log::info!(
            "Found EZP2013 \"{device_type}\", firmware version {firmware_version} (fixed 3.3 V supply)"
        );
        validate_identity(&device_type, &firmware_version)?;

        Ok(Self {
            transport,
            device_type,
            firmware_version,
            pending_wren: opcodes::WREN,
            poisoned: false,
            voltage_incompatible: false,
        })
    }

    pub(crate) fn device_type(&self) -> &str {
        &self.device_type
    }

    pub(crate) fn firmware_version(&self) -> &str {
        &self.firmware_version
    }

    fn ensure_healthy(&self) -> Result<()> {
        if self.poisoned {
            return Err(EzpSpiError::SessionPoisoned);
        }
        Ok(())
    }

    /// Send a 64-byte command packet and read its 64-byte acknowledgement.
    ///
    /// Callers must enter an exchange scope (`self.poisoned = true`) before
    /// calling this helper and disarm only once the complete firmware exchange
    /// finishes.
    async fn send_command_with_timeout(
        &mut self,
        pkt: &[u8; EZP_PACKET_SIZE],
        timeout: Duration,
    ) -> Result<[u8; EZP_PACKET_SIZE]> {
        let cmd = pkt[EZP_OFF_CMD];
        if cmd != EZP_CMD_STATUS {
            match self
                .transport
                .control_in(EZP_REQ_READY, EZP_PACKET_SIZE as u16, timeout)
                .await
            {
                Ok(resp) if resp.as_slice() == b"FOK" => {}
                Ok(resp) => {
                    log::debug!(
                        "EZP2013 ready check before cmd 0x{cmd:02x} returned {:02x?}",
                        resp
                    );
                }
                Err(EzpSpiError::Stall(msg)) => {
                    log::debug!("EZP2013 ready check before cmd 0x{cmd:02x} stalled: {msg}");
                }
                Err(e) => return Err(e),
            }
        }

        let out_actual = self.transport.bulk_out(pkt, timeout).await?;
        if out_actual != EZP_PACKET_SIZE {
            return Err(EzpSpiError::TransferFailed(format!(
                "short command OUT for cmd 0x{cmd:02x}: expected {EZP_PACKET_SIZE} bytes, got {out_actual}"
            )));
        }

        let mut ack = [0u8; EZP_PACKET_SIZE];
        let in_actual = self.transport.bulk_in(&mut ack, timeout).await?;
        if in_actual != EZP_PACKET_SIZE {
            return Err(EzpSpiError::InvalidResponse(format!(
                "short ACK for cmd 0x{cmd:02x}: expected {EZP_PACKET_SIZE} bytes, got {in_actual}"
            )));
        }

        Ok(ack)
    }

    /// Poll `EZP_REQ_DONE` until the programmer responds with `"AOK"`, bounded
    /// by a monotonic wall-clock deadline.
    async fn wait_done(&mut self) -> Result<()> {
        let usb_timeout = Duration::from_millis(EZP_USB_TIMEOUT_MS);
        let poll = Duration::from_micros(u64::from(EZP_POLL_US));
        let deadline = self.transport.now() + Duration::from_millis(EZP_DONE_TIMEOUT_MS);

        loop {
            let remaining = deadline.saturating_duration_since(self.transport.now());
            if remaining.is_zero() {
                return Err(EzpSpiError::Timeout(
                    "programmer did not finish writing (no AOK)".into(),
                ));
            }

            match self
                .transport
                .control_in(
                    EZP_REQ_DONE,
                    EZP_PACKET_SIZE as u16,
                    remaining.min(usb_timeout),
                )
                .await
            {
                Ok(resp) if resp.as_slice() == b"AOK" => return Ok(()),
                Ok(resp) => log::trace!("EZP2013 REQ_DONE poll returned {:02x?}", resp),
                Err(EzpSpiError::Stall(msg)) => {
                    log::trace!("EZP2013 REQ_DONE poll stalled: {msg}");
                }
                Err(e) => return Err(e),
            }

            let remaining = deadline.saturating_duration_since(self.transport.now());
            if remaining.is_zero() {
                return Err(EzpSpiError::Timeout(
                    "programmer did not finish writing (no AOK)".into(),
                ));
            }
            let sleep_us = remaining.min(poll).as_micros().max(1) as u32;
            self.transport.delay_us(sleep_us).await;
        }
    }

    /// Poll `EZP_CMD_STATUS` until the programmer reports `EZP_STATUS_IDLE`,
    /// bounded by a monotonic wall-clock deadline.
    async fn wait_idle(&mut self, timeout: Duration) -> Result<()> {
        let pkt = status_packet();
        let usb_timeout = Duration::from_millis(EZP_USB_TIMEOUT_MS);
        let poll = Duration::from_micros(u64::from(EZP_POLL_US));
        let deadline = self.transport.now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(self.transport.now());
            if remaining.is_zero() {
                return Err(EzpSpiError::Timeout(
                    "timed out waiting for EZP2013 idle status".into(),
                ));
            }

            let ack = self
                .send_command_with_timeout(&pkt, remaining.min(usb_timeout))
                .await?;
            match ack[0] {
                EZP_STATUS_IDLE => return Ok(()),
                EZP_STATUS_BUSY => {
                    let remaining = deadline.saturating_duration_since(self.transport.now());
                    if remaining.is_zero() {
                        return Err(EzpSpiError::Timeout(
                            "timed out waiting for EZP2013 idle status".into(),
                        ));
                    }
                    let sleep_us = remaining.min(poll).as_micros().max(1) as u32;
                    self.transport.delay_us(sleep_us).await;
                }
                other => {
                    return Err(EzpSpiError::InvalidResponse(format!(
                        "unexpected status 0x{other:02x}"
                    )));
                }
            }
        }
    }

    /// Send a read command and receive `buf.len()` bytes in whole 64-byte USB packets.
    async fn ezp_read(&mut self, pkt: &[u8; EZP_PACKET_SIZE], buf: &mut [u8]) -> Result<()> {
        self.ensure_healthy()?;
        self.poisoned = true;
        let result = self.ezp_read_inner(pkt, buf).await;
        if result.is_ok() {
            self.poisoned = false;
        }
        result
    }

    async fn ezp_read_inner(&mut self, pkt: &[u8; EZP_PACKET_SIZE], buf: &mut [u8]) -> Result<()> {
        let timeout = Duration::from_millis(EZP_USB_TIMEOUT_MS);
        self.send_command_with_timeout(pkt, timeout).await?;
        let mut done = 0usize;
        while done < buf.len() {
            let left = (buf.len() - done).min(EZP_MAX_CHUNK);
            if left < EZP_PACKET_SIZE {
                let mut tail = [0u8; EZP_PACKET_SIZE];
                let actual = self.transport.bulk_in(&mut tail, timeout).await?;
                if actual == 0 {
                    return Err(EzpSpiError::TransferFailed(
                        "empty bulk IN transfer during read".into(),
                    ));
                }
                let take = actual.min(left);
                buf[done..done + take].copy_from_slice(&tail[..take]);
                done += take;
            } else {
                let req = (left / EZP_PACKET_SIZE) * EZP_PACKET_SIZE;
                let actual = self
                    .transport
                    .bulk_in(&mut buf[done..done + req], timeout)
                    .await?;
                if actual == 0 {
                    return Err(EzpSpiError::TransferFailed(
                        "empty bulk IN transfer during read".into(),
                    ));
                }
                done += actual.min(req);
            }
        }
        Ok(())
    }

    /// Send a write command and stream its payload in `EZP_MAX_CHUNK` blocks.
    async fn ezp_write(&mut self, pkt: &[u8; EZP_PACKET_SIZE], data: &[u8]) -> Result<()> {
        if !is_valid_write_len(data.len()) {
            return Err(EzpSpiError::InvalidLength(data.len()));
        }
        self.ensure_healthy()?;
        self.poisoned = true;
        let result = self.ezp_write_inner(pkt, data).await;
        if result.is_ok() {
            self.poisoned = false;
        }
        result
    }

    async fn ezp_write_inner(&mut self, pkt: &[u8; EZP_PACKET_SIZE], data: &[u8]) -> Result<()> {
        let timeout = Duration::from_millis(EZP_USB_TIMEOUT_MS);
        self.send_command_with_timeout(pkt, timeout).await?;
        for chunk in data.chunks(EZP_MAX_CHUNK) {
            let actual = self.transport.bulk_out(chunk, timeout).await?;
            if actual != chunk.len() {
                return Err(EzpSpiError::TransferFailed(format!(
                    "short bulk OUT during write: expected {} bytes, got {actual}",
                    chunk.len()
                )));
            }
        }
        self.wait_done().await
    }

    /// Clock out `opcode addr[3]` and read `buf.len()` bytes (`<= EZP_MAX_XFER`).
    async fn read_op(&mut self, opcode: u8, addr: u32, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut pkt = init_packet(EZP_CMD_READ, addr, buf.len() as u32);
        pkt[EZP_OFF_OPS + EZP_OP_READ] = opcode;
        self.ezp_read(&pkt, buf).await
    }

    /// Read arbitrarily many bytes in `EZP_MAX_XFER` chunks.
    async fn read_op_chunked(
        &mut self,
        opcode: u8,
        mut addr: u32,
        buf: &mut [u8],
        advance_addr: bool,
    ) -> Result<()> {
        for chunk in buf.chunks_mut(EZP_MAX_XFER) {
            self.read_op(opcode, addr, chunk).await?;
            if advance_addr {
                addr = addr.wrapping_add(chunk.len() as u32);
            }
        }
        Ok(())
    }

    /// Read SFDP data (`0x5A addr[3] dummy[1] data[...]`).
    ///
    /// Because `EZP_CMD_READ` does not emit a dummy byte, request one extra
    /// leading byte and discard it.
    async fn read_sfdp(&mut self, mut addr: u32, buf: &mut [u8]) -> Result<()> {
        let mut tmp = Vec::new();
        for chunk in buf.chunks_mut(EZP_MAX_XFER - 1) {
            tmp.resize(chunk.len() + 1, 0);
            self.read_op(opcodes::RDSFDP, addr, &mut tmp).await?;
            chunk.copy_from_slice(&tmp[1..]);
            addr = addr.wrapping_add(chunk.len() as u32);
        }
        Ok(())
    }

    /// Read up to 3 bytes of JEDEC ID via `EZP_CMD_RDID`.
    async fn read_id(&mut self, out: &mut [u8]) -> Result<()> {
        if out.is_empty() {
            return Ok(());
        }
        self.ensure_healthy()?;
        self.poisoned = true;
        let timeout = Duration::from_millis(EZP_USB_TIMEOUT_MS);
        let result = self
            .send_command_with_timeout(&rdid_packet(), timeout)
            .await;
        match result {
            Ok(ack) => {
                self.poisoned = false;
                out.copy_from_slice(&ack[..out.len()]);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Program flash via `EZP_CMD_WRITE` (page program or SST AAI word mode).
    async fn program(&mut self, opcode: u8, addr: u32, data: &[u8]) -> Result<()> {
        let mut pkt = init_packet(EZP_CMD_WRITE, addr, data.len() as u32);
        if opcode == opcodes::AAI_WP {
            pkt[EZP_OFF_WMODE] = EZP_WMODE_AAI_WORD;
        }
        pkt[EZP_OFF_OPS + EZP_OP_PROGRAM] = opcode;
        self.ezp_write(&pkt, data).await
    }

    /// Erase the entire chip using `opcode` (`0x60` or `0xC7`).
    async fn erase_chip(&mut self, opcode: u8) -> Result<()> {
        self.ensure_healthy()?;
        self.poisoned = true;
        let result = async {
            let mut pkt = init_packet(EZP_CMD_ERASE, 0, 0);
            pkt[EZP_OFF_OPS + EZP_OP_CHIP_ERASE] = opcode;
            self.send_command_with_timeout(&pkt, Duration::from_millis(EZP_USB_TIMEOUT_MS))
                .await?;
            self.wait_idle(Duration::from_millis(EZP_CHIP_ERASE_TIMEOUT_MS))
                .await
        }
        .await;
        if result.is_ok() {
            self.poisoned = false;
        }
        result
    }

    /// Erase one sector/block via the security-area erase command with empty
    /// area enter/exit opcodes so the erase executes on the main array.
    async fn erase_block(&mut self, opcode: u8, addr: u32) -> Result<()> {
        self.ensure_healthy()?;
        self.poisoned = true;
        let result = async {
            let mut pkt = init_packet(EZP_CMD_ERASE, addr, EZP_PAGE_SIZE as u32);
            pkt[EZP_OFF_AREA] = 1;
            pkt[EZP_OFF_OPS + EZP_OP_AREA_ERASE] = opcode;
            self.send_command_with_timeout(&pkt, Duration::from_millis(EZP_USB_TIMEOUT_MS))
                .await?;
            self.wait_idle(Duration::from_millis(EZP_BLOCK_ERASE_TIMEOUT_MS))
                .await
        }
        .await;
        if result.is_ok() {
            self.poisoned = false;
        }
        result
    }

    /// Issue a 1-byte register write (`wren`, then `opcode value`) by placing
    /// `value` in the upper bank byte of a 1-byte read and forcing the
    /// firmware's `RDEAR`/`WREAR` bank-update step to fire.
    async fn write_reg8(&mut self, opcode: u8, value: u8, wren: u8) -> Result<()> {
        let mut pkt = init_packet(EZP_CMD_READ, u32::from(value) << 24, 1);
        pkt[EZP_OFF_OPS + EZP_OP_WREN] = wren;
        pkt[EZP_OFF_OPS + EZP_OP_RDEAR] = if value != 0 { 0x00 } else { opcodes::RDID };
        pkt[EZP_OFF_OPS + EZP_OP_WREAR] = opcode;
        let mut dummy = [0u8; 1];
        self.ezp_read(&pkt, &mut dummy).await
    }

    /// Issue a 2-byte `WRSR` (`wren`, then `opcode value[0] value[1]`) using
    /// a 4-byte AAI word write where the continuation frame carries the exact
    /// 3-byte `WRSR` sequence.
    async fn write_reg16(&mut self, opcode: u8, value: [u8; 2], wren: u8) -> Result<()> {
        let buf = [value[0], value[1], value[0], value[1]];
        let addr = (u32::from(value[0]) << 16) | (u32::from(value[1]) << 8);
        let mut pkt = init_packet(EZP_CMD_WRITE, addr, buf.len() as u32);
        pkt[EZP_OFF_WMODE] = EZP_WMODE_AAI_WORD;
        pkt[EZP_OFF_OPS + EZP_OP_WREN] = wren;
        pkt[EZP_OFF_OPS + EZP_OP_PROGRAM] = opcode;
        self.ezp_write(&pkt, &buf).await
    }

    fn map_io_err(context: &str, err: EzpSpiError) -> CoreError {
        log::error!("EZP2013 {context} failed: {err}");
        match err {
            EzpSpiError::Timeout(_) => CoreError::Timeout,
            _ => CoreError::ProgrammerError,
        }
    }
}

impl<T: EzpTransport> SpiMaster for EzpDevice<T> {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::NO_4BA_MODES
    }

    fn max_read_len(&self) -> usize {
        EZP_MAX_XFER
    }

    fn max_write_len(&self) -> usize {
        EZP_PAGE_SIZE
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        // AAI_WP (0xAD) is intentionally excluded from generic `execute()`
        // because raw addressless continuation frames cannot be sent singly;
        // SST25 chips validate via PP (0x02) and use native AAI in `program_flash()`.
        matches!(
            opcode,
            opcodes::RDID
                | opcodes::READ
                | opcodes::REMS
                | opcodes::RES
                | opcodes::RDSFDP
                | opcodes::RDSR
                | opcodes::RDSR2
                | opcodes::RDSR3
                | opcodes::WREN
                | opcodes::EWSR
                | opcodes::WRDI
                | opcodes::WRSR
                | opcodes::WRSR2
                | opcodes::WRSR3
                | opcodes::PP
                | opcodes::SE_20
                | opcodes::BE_52
                | opcodes::BE_D8
                | opcodes::CE_60
                | opcodes::CE_C7
        )
    }

    fn supports_status_write(&self, enable_opcode: u8, len: usize) -> bool {
        matches!(
            (enable_opcode, len),
            (opcodes::WREN, 1 | 2) | (opcodes::EWSR, 1)
        )
    }

    fn chip_identified(&mut self, chip: &FlashChip) {
        let incompatible = chip.voltage_min_mv > 0
            && chip.voltage_max_mv > 0
            && !(chip.voltage_min_mv..=chip.voltage_max_mv).contains(&EZP_SUPPLY_MV);
        if incompatible {
            log::error!(
                "EZP2013 supplies fixed 3.3 V, which is outside {}'s rated range                  ({:.2}-{:.2} V); refusing further operations (use a 1.8 V level-shifting adapter)",
                chip.name,
                f32::from(chip.voltage_min_mv) / 1000.0,
                f32::from(chip.voltage_max_mv) / 1000.0,
            );
        }
        self.voltage_incompatible = incompatible;
    }

    fn check_access(&self, _access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        if self.voltage_incompatible {
            return Err(CoreError::ChipNotSupported);
        }
        if in_range(addr, len as usize) {
            Ok(())
        } else {
            log::error!(
                "EZP2013 can only access the first 16 MiB (requested {addr:#x}..+{len:#x})"
            );
            Err(CoreError::AddressOutOfBounds)
        }
    }

    async fn delay_us(&mut self, us: u32) {
        self.transport.delay_us(us).await;
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        if self.voltage_incompatible {
            return Err(CoreError::ChipNotSupported);
        }
        if self.poisoned {
            return Err(Self::map_io_err("command", EzpSpiError::SessionPoisoned));
        }
        if cmd.io_mode != IoMode::Single {
            return Err(CoreError::IoModeNotSupported);
        }

        let no_addr = cmd.address.is_none() && cmd.address_width == AddressWidth::None;
        let addr_3b = match (cmd.address_width, cmd.address) {
            (AddressWidth::ThreeByte, Some(a)) => Some(a),
            _ => None,
        };

        match cmd.opcode {
            opcodes::WREN | opcodes::EWSR | opcodes::WRDI => {
                if !no_addr
                    || cmd.dummy_cycles != 0
                    || !cmd.write_data.is_empty()
                    || !cmd.read_buf.is_empty()
                {
                    return Err(CoreError::SpiTransferFailed);
                }
                self.pending_wren = match cmd.opcode {
                    opcodes::WREN | opcodes::EWSR => cmd.opcode,
                    _ => opcodes::WREN,
                };
                Ok(())
            }
            opcodes::WRSR | opcodes::WRSR2 | opcodes::WRSR3 => {
                if !no_addr || cmd.dummy_cycles != 0 || !cmd.read_buf.is_empty() {
                    return Err(CoreError::SpiTransferFailed);
                }
                let wren = core::mem::replace(&mut self.pending_wren, opcodes::WREN);
                match cmd.write_data {
                    [val] => self
                        .write_reg8(cmd.opcode, *val, wren)
                        .await
                        .map_err(|e| Self::map_io_err("8-bit status register write", e)),
                    [v0, v1] if cmd.opcode == opcodes::WRSR => {
                        if !self.supports_status_write(wren, 2) {
                            log::error!(
                                "EZP2013 cannot perform a 2-byte WRSR after EWSR (0x50);                                  use non-volatile WREN (0x06) status writes instead"
                            );
                            return Err(CoreError::OpcodeNotSupported);
                        }
                        self.write_reg16(cmd.opcode, [*v0, *v1], wren)
                            .await
                            .map_err(|e| Self::map_io_err("16-bit status register write", e))
                    }
                    _ => Err(CoreError::SpiTransferFailed),
                }
            }
            opcodes::RDID => {
                if !no_addr
                    || cmd.dummy_cycles != 0
                    || !cmd.write_data.is_empty()
                    || cmd.read_buf.len() > 3
                {
                    return Err(CoreError::SpiTransferFailed);
                }
                self.read_id(cmd.read_buf)
                    .await
                    .map_err(|e| Self::map_io_err("RDID", e))
            }
            opcodes::RDSR | opcodes::RDSR2 | opcodes::RDSR3 => {
                // Status register repeats while the firmware clocks 3 address bytes.
                if !no_addr
                    || cmd.dummy_cycles != 0
                    || !cmd.write_data.is_empty()
                    || cmd.read_buf.is_empty()
                {
                    return Err(CoreError::SpiTransferFailed);
                }
                self.read_op_chunked(cmd.opcode, 0, cmd.read_buf, false)
                    .await
                    .map_err(|e| Self::map_io_err("RDSR", e))
            }
            opcodes::READ | opcodes::REMS | opcodes::RES => {
                let Some(addr) = addr_3b else {
                    return Err(CoreError::SpiTransferFailed);
                };
                if cmd.dummy_cycles != 0 || !cmd.write_data.is_empty() || cmd.read_buf.is_empty() {
                    return Err(CoreError::SpiTransferFailed);
                }
                if !in_range(addr, cmd.read_buf.len()) {
                    return Err(CoreError::AddressOutOfBounds);
                }
                self.read_op_chunked(cmd.opcode, addr, cmd.read_buf, true)
                    .await
                    .map_err(|e| Self::map_io_err("read command", e))
            }
            opcodes::RDSFDP => {
                let Some(addr) = addr_3b else {
                    return Err(CoreError::SpiTransferFailed);
                };
                if cmd.dummy_cycles != 8 || !cmd.write_data.is_empty() || cmd.read_buf.is_empty() {
                    return Err(CoreError::SpiTransferFailed);
                }
                if !in_range(addr, cmd.read_buf.len().saturating_add(1)) {
                    return Err(CoreError::AddressOutOfBounds);
                }
                self.read_sfdp(addr, cmd.read_buf)
                    .await
                    .map_err(|e| Self::map_io_err("SFDP read", e))
            }
            opcodes::PP => {
                let Some(addr) = addr_3b else {
                    return Err(CoreError::SpiTransferFailed);
                };
                if cmd.dummy_cycles != 0
                    || !cmd.read_buf.is_empty()
                    || cmd.write_data.is_empty()
                    || cmd.write_data.len() > EZP_PAGE_SIZE
                {
                    return Err(CoreError::SpiTransferFailed);
                }
                if !in_range(addr, cmd.write_data.len()) {
                    return Err(CoreError::AddressOutOfBounds);
                }
                self.program(opcodes::PP, addr, cmd.write_data)
                    .await
                    .map_err(|e| Self::map_io_err("page program", e))
            }
            opcodes::SE_20 | opcodes::BE_52 | opcodes::BE_D8 => {
                let Some(addr) = addr_3b else {
                    return Err(CoreError::SpiTransferFailed);
                };
                if cmd.dummy_cycles != 0 || !cmd.write_data.is_empty() || !cmd.read_buf.is_empty() {
                    return Err(CoreError::SpiTransferFailed);
                }
                if !in_range(addr, EZP_PAGE_SIZE) {
                    return Err(CoreError::AddressOutOfBounds);
                }
                self.erase_block(cmd.opcode, addr)
                    .await
                    .map_err(|e| Self::map_io_err("block erase", e))
            }
            opcodes::CE_60 | opcodes::CE_C7 => {
                if !no_addr
                    || cmd.dummy_cycles != 0
                    || !cmd.write_data.is_empty()
                    || !cmd.read_buf.is_empty()
                {
                    return Err(CoreError::SpiTransferFailed);
                }
                self.erase_chip(cmd.opcode)
                    .await
                    .map_err(|e| Self::map_io_err("chip erase", e))
            }
            _ => Err(CoreError::OpcodeNotSupported),
        }
    }

    async fn read_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        buf: &mut [u8],
    ) -> CoreResult<()> {
        if self.voltage_incompatible || ctx.address_mode == AddressMode::FourByte {
            return Err(CoreError::ChipNotSupported);
        }
        if !in_range(addr, buf.len()) {
            return Err(CoreError::AddressOutOfBounds);
        }
        let mut cur_addr = addr;
        for chunk in buf.chunks_mut(EZP_MAX_XFER) {
            self.read_op(opcodes::READ, cur_addr, chunk)
                .await
                .map_err(|e| {
                    log::error!(
                        "EZP2013 bulk read at {cur_addr:#08x} ({} bytes) failed: {e}",
                        chunk.len()
                    );
                    CoreError::ReadError { addr: cur_addr }
                })?;
            cur_addr += chunk.len() as u32;
        }
        Ok(())
    }

    async fn program_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        data: &[u8],
    ) -> CoreResult<()> {
        if self.voltage_incompatible || ctx.address_mode == AddressMode::FourByte {
            return Err(CoreError::ChipNotSupported);
        }
        if !in_range(addr, data.len()) {
            return Err(CoreError::AddressOutOfBounds);
        }
        if data.is_empty() {
            return Ok(());
        }

        let mut offset = 0usize;
        let mut cur_addr = addr;

        if ctx.chip.features.contains(Features::AAI_WORD) {
            while offset < data.len() {
                let remaining = data.len() - offset;
                let (opcode, chunk) = if !cur_addr.is_multiple_of(2) || remaining == 1 {
                    (opcodes::PP, 1)
                } else {
                    (opcodes::AAI_WP, ezp_write_len(remaining, 2))
                };
                self.program(opcode, cur_addr, &data[offset..offset + chunk])
                    .await
                    .map_err(|e| {
                        log::error!(
                            "EZP2013 AAI write at {cur_addr:#08x} ({chunk} bytes) failed: {e}"
                        );
                        CoreError::WriteError { addr: cur_addr }
                    })?;
                offset += chunk;
                cur_addr += chunk as u32;
            }
            return Ok(());
        }

        if ctx.chip.write_granularity == WriteGranularity::Byte {
            while offset < data.len() {
                self.program(opcodes::PP, cur_addr, &data[offset..offset + 1])
                    .await
                    .map_err(|e| {
                        log::error!("EZP2013 byte write at {cur_addr:#08x} failed: {e}");
                        CoreError::WriteError { addr: cur_addr }
                    })?;
                offset += 1;
                cur_addr += 1;
            }
            return Ok(());
        }

        let page_size = ctx.page_size();
        if page_size == 0 {
            return Err(CoreError::ChipNotSupported);
        }
        while offset < data.len() {
            let remaining = data.len() - offset;
            let chunk = page_program_chunk_len(cur_addr, remaining, page_size);
            self.program(opcodes::PP, cur_addr, &data[offset..offset + chunk])
                .await
                .map_err(|e| {
                    log::error!(
                        "EZP2013 page write at {cur_addr:#08x} ({chunk} bytes) failed: {e}"
                    );
                    CoreError::WriteError { addr: cur_addr }
                })?;
            offset += chunk;
            cur_addr += chunk as u32;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public EZP2013 programmer struct
// ---------------------------------------------------------------------------

/// EZP2013 / CORIGHT EP01 USB SPI flash programmer
pub struct EzpSpi {
    inner: EzpDevice<NusbTransport>,
}

impl EzpSpi {
    /// Open an EZP2013 device
    ///
    /// Natively, find devices with [`crate::UsbProgrammer::candidates`]; in the
    /// browser, use `request_device`.
    pub async fn open(device_info: nusb::DeviceInfo) -> Result<Self> {
        let transport = NusbTransport::open(device_info).await?;
        let inner = EzpDevice::init(transport).await?;
        Ok(Self { inner })
    }

    /// Device type string reported by vendor request `0x54` (e.g. `"EP01 USB Programmer."`)
    pub fn device_type(&self) -> &str {
        self.inner.device_type()
    }

    /// Firmware version string reported by vendor request `0x56` (e.g. `"1.2.0.3"`)
    pub fn firmware_version(&self) -> &str {
        self.inner.firmware_version()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::UsbProgrammer for EzpSpi {
    const NAME: &'static str = "EZP2013";
    type Config = ();
    type Error = EzpSpiError;

    async fn candidates(_config: &()) -> Result<Vec<nusb::DeviceInfo>> {
        crate::usb::list_devices(USB_SELECTORS)
            .await
            .map_err(|e| EzpSpiError::OpenFailed(e.to_string()))
    }

    async fn open_device(device: nusb::DeviceInfo, _config: ()) -> Result<Self> {
        Self::open(device).await
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl EzpSpi {
    /// Request an EZP2013 device via the WebUSB permission prompt
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        log::info!("Requesting EZP2013 device via WebUSB picker...");

        let device_info = nusb::request_device(USB_SELECTORS)
            .await
            .map_err(|e| EzpSpiError::OpenFailed(format!("WebUSB request failed: {e}")))?
            .ok_or(EzpSpiError::DeviceNotFound)?;

        log::info!(
            "EZP2013 device selected: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        Ok(device_info)
    }

    /// Drain any pending USB transfers before closing (WASM disconnect hook)
    pub async fn shutdown(&mut self) {
        self.inner.transport.shutdown().await;
    }
}

impl SpiMaster for EzpSpi {
    fn features(&self) -> SpiFeatures {
        self.inner.features()
    }

    fn max_read_len(&self) -> usize {
        self.inner.max_read_len()
    }

    fn max_write_len(&self) -> usize {
        self.inner.max_write_len()
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        self.inner.probe_opcode(opcode)
    }

    fn supports_status_write(&self, enable_opcode: u8, len: usize) -> bool {
        self.inner.supports_status_write(enable_opcode, len)
    }

    fn chip_identified(&mut self, chip: &FlashChip) {
        self.inner.chip_identified(chip);
    }

    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        self.inner.check_access(access, addr, len)
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        self.inner.execute(cmd).await
    }

    async fn delay_us(&mut self, us: u32) {
        self.inner.delay_us(us).await;
    }

    async fn read_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        buf: &mut [u8],
    ) -> CoreResult<()> {
        self.inner.read_flash(ctx, addr, buf).await
    }

    async fn program_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        data: &[u8],
    ) -> CoreResult<()> {
        self.inner.program_flash(ctx, addr, data).await
    }
}
