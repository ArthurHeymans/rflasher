use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures_lite::future::block_on;
use rflasher_chips::ChipDatabase;
use rflasher_core::chip::{Features, Unlock};
use rflasher_core::flash::{self, FlashContext, FlashDevice, ProbeOptions, SpiFlashDevice};
use rflasher_core::wp::{self, WpBits, WpError, WpRegBitMap, WriteOptions};

use super::*;

fn chip_by_name(db: &ChipDatabase, name: &str) -> FlashChip {
    (*db.find_by_name(name)
        .first()
        .unwrap_or_else(|| panic!("chip {name} not found in database")))
    .clone()
}

/// Deterministic non-repeating byte sequence so swapped pages or chunks fail readback.
fn non_repeating_pattern(len: usize, mut state: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.push((state & 0xFF) as u8);
    }
    out
}

// ---------------------------------------------------------------------------
// Independent packet fixtures and pure protocol tests
// ---------------------------------------------------------------------------

#[test]
fn packet_builders_match_independent_raw_64_byte_fixtures() {
    let mut expected_read = [0u8; 64];
    expected_read[0] = 0x03; // EZP_CMD_READ
    expected_read[1] = 0xF0; // EZP_TYPE_SPI_FLASH
    expected_read[2] = 0x00; // chunk = 0x0800 LE
    expected_read[3] = 0x08;
    expected_read[4..8].copy_from_slice(&[0x56, 0x44, 0x12, 0x00]); // end = 0x0012_4456 LE
    expected_read[8..12].copy_from_slice(&[0x56, 0x34, 0x12, 0x00]); // start = 0x0012_3456 LE
    expected_read[12] = 33; // 3.3 V
    expected_read[13] = 8; // 8 MHz
    expected_read[14] = 0; // page mode
    expected_read[20] = 0; // main array
    expected_read[21] = 0x06; // WREN
    expected_read[22] = 0x04; // WRDI
    expected_read[23] = 0x03; // READ
    expected_read[24] = 0x02; // PP
    expected_read[25] = 0xC7; // CHIP_ERASE
    expected_read[26] = 0x05; // RDSR
    expected_read[27] = 0x00; // WRSR intentionally 0

    assert_eq!(
        init_packet(EZP_CMD_READ, 0x0012_3456, 0x1000),
        expected_read
    );

    // Sub-2KiB length uses exact length as chunk size.
    let mut expected_small = expected_read;
    expected_small[2] = 0x80;
    expected_small[3] = 0x00;
    expected_small[4..8].copy_from_slice(&[0x80, 0x00, 0x00, 0x00]);
    expected_small[8..12].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    assert_eq!(init_packet(EZP_CMD_READ, 0, 128), expected_small);

    let mut expected_rdid = [0u8; 64];
    expected_rdid[0] = 0x41;
    expected_rdid[1] = 0xF0;
    expected_rdid[12] = 33;
    expected_rdid[13] = 8;
    assert_eq!(rdid_packet(), expected_rdid);

    let mut expected_status = [0u8; 64];
    expected_status[0] = 0x20;
    assert_eq!(status_packet(), expected_status);
}

#[test]
fn in_range_is_overflow_safe() {
    assert!(in_range(0, 0));
    assert!(in_range(0, EZP_MAX_ADDR_SPACE));
    assert!(in_range(EZP_MAX_ADDR_SPACE as u32 - 1, 1));
    assert!(in_range(EZP_MAX_ADDR_SPACE as u32, 0));
    assert!(!in_range(EZP_MAX_ADDR_SPACE as u32, 1));
    assert!(!in_range(0, EZP_MAX_ADDR_SPACE + 1));
    assert!(!in_range(u32::MAX, 1));
    assert!(!in_range(u32::MAX, 16));
    assert!(!in_range(0, usize::MAX));
    assert!(!in_range(1, usize::MAX));
}

#[test]
fn valid_write_lengths_enforce_2k_chunk_invariant() {
    assert!(!is_valid_write_len(0));
    assert!(is_valid_write_len(1));
    assert!(is_valid_write_len(256));
    assert!(is_valid_write_len(EZP_MAX_CHUNK));
    assert!(!is_valid_write_len(EZP_MAX_CHUNK + 1));
    assert!(!is_valid_write_len(EZP_MAX_CHUNK + 256));
    assert!(is_valid_write_len(EZP_MAX_CHUNK * 2));
    assert!(is_valid_write_len(EZP_MAX_XFER));
    assert!(!is_valid_write_len(EZP_MAX_XFER + EZP_MAX_CHUNK));
}

#[test]
fn page_program_chunk_len_preserves_invariants_for_all_page_sizes() {
    for page_size in [1usize, 16, 64, 128, 256, 264, 512, 4096] {
        for start in [0u32, 1, 13, 127, 128, 255, 256, 500, 4095, 4096] {
            for total_len in [1usize, 7, 128, 255, 256, 500, 2048, 3000, 5000] {
                let mut cur = start;
                let mut left = total_len;
                while left > 0 {
                    let chunk = page_program_chunk_len(cur, left, page_size);
                    assert!(
                        chunk > 0 && chunk <= left,
                        "page={page_size} start={cur} left={left} -> chunk={chunk}"
                    );
                    assert!(
                        is_valid_write_len(chunk),
                        "invalid ezp_write len {chunk} for page={page_size} start={cur} left={left}"
                    );
                    if chunk > EZP_PAGE_SIZE {
                        // Firmware steps in 256-byte pages from start.
                        assert_eq!(cur as usize % EZP_PAGE_SIZE, 0);
                        assert_eq!(chunk % EZP_PAGE_SIZE, 0);
                        assert!(page_size >= EZP_PAGE_SIZE && page_size % EZP_PAGE_SIZE == 0);
                    } else {
                        // Must stay within a single chip page so firmware cannot wrap.
                        let page_off = cur as usize % page_size;
                        assert!(
                            page_off + chunk <= page_size,
                            "crossed chip page: page={page_size} cur={cur} chunk={chunk}"
                        );
                    }
                    cur += chunk as u32;
                    left -= chunk;
                }
            }
        }
    }
}

#[test]
fn validate_identity_accepts_1_2_0_x_and_rejects_bootloader_or_other_versions() {
    assert!(validate_identity("EP01 USB Programmer.", "1.2.0.3").is_ok());
    assert!(validate_identity("EZP2013", "1.2.0.0").is_ok());
    assert!(matches!(
        validate_identity("CORIGHT BOOTLOADER", "1.2.0.3"),
        Err(EzpSpiError::BootloaderMode(_))
    ));
    assert!(matches!(
        validate_identity("EP01 USB Programmer.", "1.1.0.9"),
        Err(EzpSpiError::UnsupportedFirmware { .. })
    ));
    assert!(matches!(
        validate_identity("EP01 USB Programmer.", "2.0.0.0"),
        Err(EzpSpiError::UnsupportedFirmware { .. })
    ));
}

// ---------------------------------------------------------------------------
// Stateful mock EZP2013 firmware transport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RecordedOp {
    cmd: u8,
    start: u32,
    len: u32,
    chunk_size: u16,
    wmode: u8,
    area: u8,
    ops: [u8; EZP_NUM_OPS],
    payload: Vec<u8>,
    chunk_lengths: Vec<usize>,
    aai_restarts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    ShortOut,
    EmptyBulkInAck,
    EmptyBulkInPayload,
    TimeoutBulkInPayloadLeavingStale,
    NeverDone,
    BadStatus,
    SuspendOnceOnBulkIn,
}

struct MockEzpTransport {
    device_type: String,
    firmware_version: String,
    jedec_id: [u8; 3],
    flash: Vec<u8>,
    sfdp: Vec<u8>,
    sr: [u8; 3],
    chip_page_size: usize,
    /// Block writes/erases when SR1 BP bits (0x1c) are set.
    enforce_bp: bool,
    /// Simulated USB transfer latencies for wall-clock deadline tests.
    control_latency: Duration,
    bulk_latency: Duration,
    base_now: web_time::Instant,
    elapsed: Duration,
    /// Count of busy status responses before returning IDLE.
    busy_polls_left: usize,
    /// Count of non-AOK DONE polls before returning AOK.
    done_polls_left: usize,
    done_poll_count: usize,
    status_poll_count: usize,
    fault: Fault,
    ready_seen_for_last_cmd: bool,
    /// Queued bulk-IN packets: `(is_ack, bytes)`.
    in_queue: VecDeque<(bool, Vec<u8>)>,
    /// Active write awaiting payload: `(op, remaining_bytes, expected_chunk_size)`.
    pending_write: Option<(RecordedOp, usize, usize)>,
    recorded: Vec<RecordedOp>,
    /// Total transport calls observed (used to prove poisoned sessions make zero calls).
    transport_calls: usize,
}

impl MockEzpTransport {
    fn new(jedec_id: [u8; 3], flash_size: usize, chip_page_size: usize) -> Self {
        Self {
            device_type: "EP01 USB Programmer.".into(),
            firmware_version: "1.2.0.3".into(),
            jedec_id,
            flash: vec![0xFF; flash_size],
            sfdp: Vec::new(),
            sr: [0x00, 0x00, 0x00],
            chip_page_size,
            enforce_bp: false,
            control_latency: Duration::ZERO,
            bulk_latency: Duration::ZERO,
            base_now: web_time::Instant::now(),
            elapsed: Duration::ZERO,
            busy_polls_left: 0,
            done_polls_left: 0,
            done_poll_count: 0,
            status_poll_count: 0,
            fault: Fault::None,
            ready_seen_for_last_cmd: false,
            in_queue: VecDeque::new(),
            pending_write: None,
            recorded: Vec::new(),
            transport_calls: 0,
        }
    }

    fn is_bp_protected(&self) -> bool {
        self.enforce_bp && (self.sr[0] & 0x1c) != 0
    }

    /// Execute the firmware's `RDEAR`/`WREAR` bank-select pre-read hook faithfully.
    fn run_bank_select_hook(&mut self, start: u32, ops: &[u8; EZP_NUM_OPS]) {
        let rdear = ops[EZP_OP_RDEAR];
        let wrear = ops[EZP_OP_WREAR];
        if wrear == 0 {
            return;
        }
        let target_bank = (start >> 24) as u8;
        let current_bank = match rdear {
            0x00 => 0x00,
            opcodes::RDID => self.jedec_id[0],
            _ => panic!("unexpected RDEAR opcode 0x{rdear:02x}"),
        };
        if current_bank != target_bank {
            let wren = ops[EZP_OP_WREN];
            assert!(
                matches!(wren, opcodes::WREN | opcodes::EWSR),
                "bank-select hook requires WREN or EWSR, got 0x{wren:02x}"
            );
            match wrear {
                opcodes::WRSR => self.sr[0] = target_bank,
                opcodes::WRSR2 => self.sr[1] = target_bank,
                opcodes::WRSR3 => self.sr[2] = target_bank,
                other => panic!("unexpected WREAR opcode 0x{other:02x}"),
            }
        }
    }

    fn apply_write(&mut self, op: &mut RecordedOp) {
        let prog_op = op.ops[EZP_OP_PROGRAM];
        if op.wmode == EZP_WMODE_AAI_WORD && prog_op == opcodes::WRSR {
            // Model the exact 2-word AAI wire sequence on a strict 16-bit WRSR chip:
            // 1) wren (0x06 persistent WEL, or 0x50 volatile EWSR)
            // 2) 6-byte frame: [0x01, addr[2], addr[1], addr[0], data[0], data[1]]
            //    rejected by chips enforcing 16-bit WRSR length (WEL stays set)
            // 3) RDSR poll: clears volatile EWSR (0x50), preserves persistent WEL (0x06)
            // 4) 3-byte continuation frame: [0x01, data[2], data[3]]
            // 5) WRDI
            assert_eq!(op.payload.len(), 4);
            let expected_addr = (u32::from(op.payload[0]) << 16) | (u32::from(op.payload[1]) << 8);
            assert_eq!(op.start, expected_addr);
            let wren = op.ops[EZP_OP_WREN];
            let persistent_wel = wren == opcodes::WREN;
            // Frame 2: 6-byte frame rejected (no SR change); then RDSR clears volatile EWSR (0x50),
            // leaving only persistent WEL (0x06) active for frame 4's 3-byte continuation.
            if persistent_wel {
                self.sr[0] = op.payload[2];
                self.sr[1] = op.payload[3];
            }
            return;
        }

        if self.is_bp_protected() {
            // Protected flash silently ignores program commands.
            return;
        }

        let start = op.start as usize;
        if op.wmode == EZP_WMODE_AAI_WORD {
            assert_eq!(prog_op, opcodes::AAI_WP);
            assert_eq!(op.payload.len() % 2, 0);
            // Firmware runs AAI in chunks of `chunk_size` (2 KiB), issuing WRDI
            // and restarting WREN + addressed 0xAD at each chunk boundary.
            let step = usize::from(op.chunk_size);
            assert!(step > 0 && step % 2 == 0);
            let mut off = 0usize;
            let mut restarts = 0usize;
            while off < op.payload.len() {
                if off > 0 {
                    restarts += 1;
                }
                let end = (off + step).min(op.payload.len());
                for i in (off..end).step_by(2) {
                    self.flash[start + i] &= op.payload[i];
                    self.flash[start + i + 1] &= op.payload[i + 1];
                }
                off = end;
            }
            op.aai_restarts = restarts;
            return;
        }

        // Standard page program: firmware issues 0x02 frames in 256-byte steps.
        // Model chip-side page wrapping so a page_size < 256 bug corrupts data.
        let mut frame_off = 0usize;
        while frame_off < op.payload.len() {
            let frame_len = (op.payload.len() - frame_off).min(EZP_PAGE_SIZE);
            let frame_start = start + frame_off;
            let page_base = (frame_start / self.chip_page_size) * self.chip_page_size;
            let mut in_page = frame_start % self.chip_page_size;
            for &b in &op.payload[frame_off..frame_off + frame_len] {
                self.flash[page_base + in_page] &= b;
                in_page = (in_page + 1) % self.chip_page_size;
            }
            frame_off += frame_len;
        }
    }
}

struct YieldOnce(bool);
impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

impl EzpTransport for MockEzpTransport {
    fn now(&self) -> web_time::Instant {
        self.base_now + self.elapsed
    }

    async fn control_in(
        &mut self,
        request: u8,
        _max_len: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.transport_calls += 1;
        if timeout.is_zero() {
            return Err(EzpSpiError::Timeout(format!(
                "control IN 0x{request:02x} zero budget"
            )));
        }
        self.elapsed += self.control_latency;
        if self.control_latency > timeout {
            return Err(EzpSpiError::Timeout(format!(
                "control IN 0x{request:02x} exceeded budget"
            )));
        }
        match request {
            EZP_REQ_TYPE => Ok(self.device_type.as_bytes().to_vec()),
            EZP_REQ_VERSION => Ok(self.firmware_version.as_bytes().to_vec()),
            EZP_REQ_READY => {
                self.ready_seen_for_last_cmd = true;
                Ok(b"FOK".to_vec())
            }
            EZP_REQ_DONE => {
                self.done_poll_count += 1;
                if self.fault == Fault::NeverDone {
                    return Ok(b"BSY".to_vec());
                }
                if self.done_polls_left > 0 {
                    self.done_polls_left -= 1;
                    return Ok(b"BSY".to_vec());
                }
                Ok(b"AOK".to_vec())
            }
            other => Err(EzpSpiError::InvalidResponse(format!(
                "unexpected control IN 0x{other:02x}"
            ))),
        }
    }

    async fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<usize> {
        self.transport_calls += 1;
        if timeout.is_zero() {
            return Err(EzpSpiError::Timeout("bulk OUT zero budget".into()));
        }
        self.elapsed += self.bulk_latency;
        if self.bulk_latency > timeout {
            return Err(EzpSpiError::Timeout("bulk OUT exceeded budget".into()));
        }
        if self.fault == Fault::ShortOut {
            return Ok(data.len().saturating_sub(1));
        }

        if let Some((mut op, remaining, expected_chunk)) = self.pending_write.take() {
            assert_eq!(
                data.len(),
                expected_chunk,
                "bulk OUT chunk size {} must match packet advertised chunk size {}",
                data.len(),
                expected_chunk
            );
            assert!(
                data.len() <= remaining,
                "host wrote {} bytes with only {} remaining",
                data.len(),
                remaining
            );
            op.chunk_lengths.push(data.len());
            op.payload.extend_from_slice(data);
            let new_remaining = remaining - data.len();
            if new_remaining == 0 {
                self.apply_write(&mut op);
                self.recorded.push(op);
            } else {
                self.pending_write = Some((op, new_remaining, expected_chunk));
            }
            return Ok(data.len());
        }

        assert_eq!(data.len(), EZP_PACKET_SIZE);
        let cmd = data[EZP_OFF_CMD];
        if cmd == EZP_CMD_STATUS {
            self.status_poll_count += 1;
            assert!(
                !self.ready_seen_for_last_cmd,
                "REQ_READY must not precede CMD_STATUS"
            );
            let mut ack = vec![0u8; EZP_PACKET_SIZE];
            ack[0] = if self.fault == Fault::BadStatus {
                0xFF
            } else if self.busy_polls_left > 0 {
                self.busy_polls_left -= 1;
                EZP_STATUS_BUSY
            } else {
                EZP_STATUS_IDLE
            };
            self.in_queue.push_back((true, ack));
            return Ok(data.len());
        }

        assert!(
            self.ready_seen_for_last_cmd,
            "REQ_READY must precede command 0x{cmd:02x}"
        );
        self.ready_seen_for_last_cmd = false;

        assert_eq!(data[EZP_OFF_TYPE], EZP_TYPE_SPI_FLASH);
        assert_eq!(data[EZP_OFF_VCC], EZP_VCC_3V3);
        assert_eq!(data[EZP_OFF_SPI_MHZ], EZP_SPI_MHZ);

        let chunk_size =
            u16::from_le_bytes(data[EZP_OFF_CHUNK..EZP_OFF_CHUNK + 2].try_into().unwrap());
        let start = u32::from_le_bytes(data[EZP_OFF_START..EZP_OFF_START + 4].try_into().unwrap());
        let end = u32::from_le_bytes(data[EZP_OFF_END..EZP_OFF_END + 4].try_into().unwrap());
        let len = end.wrapping_sub(start);
        let wmode = data[EZP_OFF_WMODE];
        let area = data[EZP_OFF_AREA];
        let mut ops = [0u8; EZP_NUM_OPS];
        ops.copy_from_slice(&data[EZP_OFF_OPS..EZP_OFF_OPS + EZP_NUM_OPS]);
        assert_eq!(ops[EZP_OP_WRSR], 0, "EZP_OP_WRSR must always remain 0");

        let mut ack = vec![0u8; EZP_PACKET_SIZE];
        match cmd {
            EZP_CMD_RDID => {
                ack[0..3].copy_from_slice(&self.jedec_id);
                self.in_queue.push_back((true, ack));
                self.recorded.push(RecordedOp {
                    cmd,
                    start,
                    len,
                    chunk_size,
                    wmode,
                    area,
                    ops,
                    payload: Vec::new(),
                    chunk_lengths: Vec::new(),
                    aai_restarts: 0,
                });
            }
            EZP_CMD_READ => {
                assert_eq!(
                    usize::from(chunk_size),
                    (len as usize).min(EZP_MAX_CHUNK),
                    "CMD_READ advertised chunk size mismatch"
                );
                self.in_queue.push_back((true, ack));
                self.run_bank_select_hook(start, &ops);

                let read_op = ops[EZP_OP_READ];
                let mut payload = vec![0u8; len as usize];
                match read_op {
                    opcodes::RDSR => payload.fill(self.sr[0]),
                    opcodes::RDSR2 => payload.fill(self.sr[1]),
                    opcodes::RDSR3 => payload.fill(self.sr[2]),
                    opcodes::RDSFDP => {
                        payload[0] = 0xAA;
                        for (i, dst) in payload[1..].iter_mut().enumerate() {
                            *dst = self.sfdp.get(start as usize + i).copied().unwrap_or(0xFF);
                        }
                    }
                    opcodes::READ => {
                        for (i, dst) in payload.iter_mut().enumerate() {
                            *dst = self.flash[(start as usize + i) % self.flash.len()];
                        }
                    }
                    _ => payload.fill(0xFF),
                }
                for chunk in payload.chunks(EZP_MAX_CHUNK) {
                    let padded_len = chunk.len().div_ceil(EZP_PACKET_SIZE) * EZP_PACKET_SIZE;
                    let mut pkt = vec![0u8; padded_len];
                    pkt[..chunk.len()].copy_from_slice(chunk);
                    self.in_queue.push_back((false, pkt));
                }
                self.recorded.push(RecordedOp {
                    cmd,
                    start,
                    len,
                    chunk_size,
                    wmode,
                    area,
                    ops,
                    payload: Vec::new(),
                    chunk_lengths: Vec::new(),
                    aai_restarts: 0,
                });
            }
            EZP_CMD_WRITE => {
                assert!(
                    is_valid_write_len(len as usize),
                    "mock received invalid write length {len}"
                );
                assert_eq!(
                    usize::from(chunk_size),
                    (len as usize).min(EZP_MAX_CHUNK),
                    "CMD_WRITE advertised chunk size mismatch"
                );
                self.in_queue.push_back((true, ack));
                self.pending_write = Some((
                    RecordedOp {
                        cmd,
                        start,
                        len,
                        chunk_size,
                        wmode,
                        area,
                        ops,
                        payload: Vec::with_capacity(len as usize),
                        chunk_lengths: Vec::new(),
                        aai_restarts: 0,
                    },
                    len as usize,
                    usize::from(chunk_size),
                ));
            }
            EZP_CMD_ERASE => {
                self.in_queue.push_back((true, ack));
                if !self.is_bp_protected() {
                    if area == 0 {
                        self.flash.fill(0xFF);
                    } else {
                        let erase_size = match ops[EZP_OP_AREA_ERASE] {
                            opcodes::SE_20 => 4096,
                            opcodes::BE_52 => 32768,
                            opcodes::BE_D8 => 65536,
                            other => panic!("unexpected erase opcode 0x{other:02x}"),
                        };
                        let base = (start as usize / erase_size) * erase_size;
                        let end_idx = (base + erase_size).min(self.flash.len());
                        self.flash[base..end_idx].fill(0xFF);
                    }
                }
                self.recorded.push(RecordedOp {
                    cmd,
                    start,
                    len,
                    chunk_size,
                    wmode,
                    area,
                    ops,
                    payload: Vec::new(),
                    chunk_lengths: Vec::new(),
                    aai_restarts: 0,
                });
            }
            other => panic!("unexpected bulk command 0x{other:02x}"),
        }

        Ok(data.len())
    }

    async fn bulk_in(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.transport_calls += 1;
        if timeout.is_zero() {
            return Err(EzpSpiError::Timeout("bulk IN zero budget".into()));
        }
        self.elapsed += self.bulk_latency;
        if self.bulk_latency > timeout {
            return Err(EzpSpiError::Timeout("bulk IN exceeded budget".into()));
        }
        if self.fault == Fault::SuspendOnceOnBulkIn {
            YieldOnce(false).await;
        }
        let (is_ack, pkt) = self
            .in_queue
            .pop_front()
            .expect("unexpected bulk IN with empty queue");
        if is_ack && self.fault == Fault::EmptyBulkInAck {
            return Ok(0);
        }
        if !is_ack && self.fault == Fault::EmptyBulkInPayload {
            return Ok(0);
        }
        if !is_ack && self.fault == Fault::TimeoutBulkInPayloadLeavingStale {
            // Re-queue a stale 64-byte payload packet that would look like a
            // valid ACK if the driver did not poison the session.
            self.in_queue
                .push_front((false, vec![0xDE; EZP_PACKET_SIZE]));
            return Err(EzpSpiError::Timeout("delayed bulk IN payload".into()));
        }
        let n = pkt.len().min(buf.len());
        buf[..n].copy_from_slice(&pkt[..n]);
        if pkt.len() > n {
            self.in_queue.push_front((is_ack, pkt[n..].to_vec()));
        }
        Ok(n)
    }

    async fn delay_us(&mut self, us: u32) {
        self.elapsed += Duration::from_micros(u64::from(us));
    }
}

// ---------------------------------------------------------------------------
// End-to-end tests with rflasher-core and mock firmware
// ---------------------------------------------------------------------------

#[test]
fn init_rejects_bootloader_and_unknown_firmware() {
    block_on(async {
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x18], 4096, 256);
        mock.device_type = "CORIGHT BOOTLOADER".into();
        assert!(matches!(
            EzpDevice::init(mock).await,
            Err(EzpSpiError::BootloaderMode(_))
        ));

        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x18], 4096, 256);
        mock.firmware_version = "2.1.0.0".into();
        assert!(matches!(
            EzpDevice::init(mock).await,
            Err(EzpSpiError::UnsupportedFirmware { .. })
        ));
    });
}

#[test]
fn probe_detects_winbond_chip_and_reads_sfdp_skipping_dummy_byte() {
    block_on(async {
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 2 * 1024 * 1024, 256);
        mock.sfdp = vec![
            b'S', b'F', b'D', b'P', 0x00, 0x01, 0x00, 0xFF, 0x00, 0x00, 0x01, 0x09, 0x30, 0x00,
            0x00, 0xFF,
        ];

        let mut dev = EzpDevice::init(mock).await.unwrap();
        assert_eq!(dev.device_type(), "EP01 USB Programmer.");
        assert_eq!(dev.firmware_version(), "1.2.0.3");

        let db = ChipDatabase::new();
        let probe = flash::probe_with_options(&mut dev, &db, &ProbeOptions::default())
            .await
            .unwrap();
        assert_eq!(probe.chip.jedec_manufacturer, 0xEF);
        assert_eq!(probe.chip.jedec_device, 0x4015);

        let mut sfdp_hdr = [0u8; 8];
        dev.execute(&mut SpiCommand {
            opcode: opcodes::RDSFDP,
            address: Some(0),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::Single,
            dummy_cycles: 8,
            write_data: &[],
            read_buf: &mut sfdp_hdr,
        })
        .await
        .unwrap();
        assert_eq!(&sfdp_hdr[..4], b"SFDP");
    });
}

#[test]
fn bulk_read_erase_and_page_program_roundtrip_with_non_repeating_data() {
    block_on(async {
        let db = ChipDatabase::new();
        let chip = chip_by_name(&db, "W25Q16.V");
        let ctx = FlashContext::new(chip);

        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 128 * 1024, 256);
        mock.busy_polls_left = 2;
        mock.done_polls_left = 2;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&ctx.chip);

        let mut data = non_repeating_pattern(8192, 0x1234_5678);
        // Test unaligned start + multi-chunk run + unaligned tail.
        let addr = 0x1005u32;
        data.truncate(5003);

        dev.program_flash(&ctx, addr, &data).await.unwrap();

        let mut readback = vec![0u8; data.len()];
        dev.read_flash(&ctx, addr, &mut readback).await.unwrap();
        assert_eq!(readback, data);

        let mut device = SpiFlashDevice::new(dev, ctx);
        device.erase(0x1000, 4096).await.unwrap();
        let mut after_erase = vec![0u8; 4096];
        device.read(0x1000, &mut after_erase).await.unwrap();
        assert!(after_erase.iter().all(|&b| b == 0xFF));

        let mut surviving = vec![0u8; 512];
        device.read(0x2000, &mut surviving).await.unwrap();
        assert_eq!(&surviving[..], &data[0x0FFB..0x0FFB + 512]);
        let (dev, _) = device.into_parts();
        let erase_op = dev
            .transport
            .recorded
            .iter()
            .find(|o| o.cmd == EZP_CMD_ERASE)
            .unwrap();
        assert_eq!(erase_op.area, 1);
    });
}

#[test]
fn small_page_size_chip_does_not_wrap_on_256_byte_firmware_frames() {
    block_on(async {
        let db = ChipDatabase::new();
        let mut chip = chip_by_name(&db, "W25Q16.V");
        chip.page_size = 128;
        let ctx = FlashContext::new(chip);

        let mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 16 * 1024, 128);
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&ctx.chip);

        let payload = non_repeating_pattern(512, 0xDEAD_BEEF);
        dev.program_flash(&ctx, 0x0040, &payload).await.unwrap();

        let mut readback = vec![0u8; payload.len()];
        dev.read_flash(&ctx, 0x0040, &mut readback).await.unwrap();
        assert_eq!(readback, payload);

        for op in dev
            .transport
            .recorded
            .iter()
            .filter(|o| o.cmd == EZP_CMD_WRITE)
        {
            assert!(op.len <= 128, "expected <= 128 B write, got {}", op.len);
        }
    });
}

#[test]
fn large_page_size_chip_stays_2k_chunk_aligned() {
    block_on(async {
        let db = ChipDatabase::new();
        let mut chip = chip_by_name(&db, "W25Q16.V");
        chip.page_size = 4096;
        let ctx = FlashContext::new(chip);

        let mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 16 * 1024, 4096);
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&ctx.chip);

        let payload = non_repeating_pattern(3000, 0xCAFE_BABE);
        dev.program_flash(&ctx, 0, &payload).await.unwrap();

        let mut readback = vec![0u8; payload.len()];
        dev.read_flash(&ctx, 0, &mut readback).await.unwrap();
        assert_eq!(readback, payload);
    });
}

#[test]
fn aai_word_program_handles_odd_edges_and_multi_chunk_2k_restarts() {
    block_on(async {
        let db = ChipDatabase::new();
        let chip = chip_by_name(&db, "SST25VF040B");
        assert!(chip.features.contains(Features::AAI_WORD));
        let ctx = FlashContext::new(chip);

        let mock = MockEzpTransport::new([0xBF, 0x25, 0x8D], 32 * 1024, 256);
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&ctx.chip);

        // Odd start + 4096-byte aligned middle (two 2 KiB chunks in one AAI command) + trailing byte.
        let payload = non_repeating_pattern(1 + 4096 + 1, 0xA11A_0001);
        dev.program_flash(&ctx, 0x0001, &payload).await.unwrap();

        let mut readback = vec![0u8; payload.len()];
        dev.read_flash(&ctx, 0x0001, &mut readback).await.unwrap();
        assert_eq!(readback, payload);

        let writes: Vec<&RecordedOp> = dev
            .transport
            .recorded
            .iter()
            .filter(|o| o.cmd == EZP_CMD_WRITE)
            .collect();
        assert_eq!(writes.len(), 3);
        assert_eq!(
            (writes[0].ops[EZP_OP_PROGRAM], writes[0].len),
            (opcodes::PP, 1)
        );
        assert_eq!(
            (
                writes[1].ops[EZP_OP_PROGRAM],
                writes[1].wmode,
                writes[1].len,
                writes[1].chunk_lengths.as_slice(),
                writes[1].aai_restarts,
            ),
            (
                opcodes::AAI_WP,
                EZP_WMODE_AAI_WORD,
                4096,
                &[2048, 2048][..],
                1
            )
        );
        assert_eq!(
            (writes[2].ops[EZP_OP_PROGRAM], writes[2].len),
            (opcodes::PP, 1)
        );
    });
}

#[test]
fn status_register_writes_ewsr_wrdi_and_wrsr2_width_validation() {
    block_on(async {
        let mock = MockEzpTransport::new([0xEF, 0x40, 0x18], 4096, 256);
        let mut dev = EzpDevice::init(mock).await.unwrap();

        // 8-bit WRSR with EWSR (0x50) — non-zero and zero (exercises both RDEAR opcodes).
        for val in [0x1Cu8, 0x00u8] {
            dev.execute(&mut SpiCommand::simple(opcodes::EWSR))
                .await
                .unwrap();
            dev.execute(&mut SpiCommand::write_reg(opcodes::WRSR, &[val]))
                .await
                .unwrap();
            let mut sr = [0u8; 1];
            dev.execute(&mut SpiCommand::read_reg(opcodes::RDSR, &mut sr))
                .await
                .unwrap();
            assert_eq!(sr[0], val);
        }

        // EWSR followed by WRDI clears deferred EWSR so subsequent WRSR uses WREN (0x06).
        dev.execute(&mut SpiCommand::simple(opcodes::EWSR))
            .await
            .unwrap();
        dev.execute(&mut SpiCommand::simple(opcodes::WRDI))
            .await
            .unwrap();
        dev.execute(&mut SpiCommand::write_reg(opcodes::WRSR, &[0x28, 0x02]))
            .await
            .unwrap();
        let last_write = dev
            .transport
            .recorded
            .iter()
            .rev()
            .find(|o| o.cmd == EZP_CMD_WRITE)
            .unwrap();
        assert_eq!(last_write.ops[EZP_OP_WREN], opcodes::WREN);
        let (mut sr1, mut sr2) = ([0u8; 1], [0u8; 1]);
        dev.execute(&mut SpiCommand::read_reg(opcodes::RDSR, &mut sr1))
            .await
            .unwrap();
        dev.execute(&mut SpiCommand::read_reg(opcodes::RDSR2, &mut sr2))
            .await
            .unwrap();
        assert_eq!((sr1[0], sr2[0]), (0x28, 0x02));

        // 16-bit WRSR after EWSR (0x50) is rejected and pending_wren resets to WREN.
        dev.execute(&mut SpiCommand::simple(opcodes::EWSR))
            .await
            .unwrap();
        assert_eq!(
            dev.execute(&mut SpiCommand::write_reg(opcodes::WRSR, &[0x00, 0x02]))
                .await,
            Err(CoreError::OpcodeNotSupported)
        );
        // Next 16-bit WRSR without EWSR uses WREN (0x06) cleanly.
        dev.execute(&mut SpiCommand::write_reg(opcodes::WRSR, &[0x04, 0x02]))
            .await
            .unwrap();
        dev.execute(&mut SpiCommand::read_reg(opcodes::RDSR, &mut sr1))
            .await
            .unwrap();
        assert_eq!(sr1[0], 0x04);

        // 2-byte WRSR2 and WRSR3 are rejected (only opcode 0x01 supports 16-bit form).
        assert_eq!(
            dev.execute(&mut SpiCommand::write_reg(opcodes::WRSR2, &[0x02, 0x00]))
                .await,
            Err(CoreError::SpiTransferFailed)
        );
        assert_eq!(
            dev.execute(&mut SpiCommand::write_reg(opcodes::WRSR3, &[0x00, 0x00]))
                .await,
            Err(CoreError::SpiTransferFailed)
        );
    });
}

#[test]
fn protected_w25q128v_auto_unlocks_via_wren_and_restores_sr1_sr2() {
    block_on(async {
        let db = ChipDatabase::new();
        let chip = chip_by_name(&db, "W25Q128FV");
        assert!(chip.features.contains(Features::WRSR_EWSR));
        assert!(chip.features.contains(Features::WRSR_WREN));
        assert!(chip.features.contains(Features::WRSR_EXT));
        let ctx = FlashContext::new(chip);

        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x18], 64 * 1024, 256);
        mock.enforce_bp = true;
        mock.sr[0] = 0x1C; // BP0..BP2 protected
        mock.sr[1] = 0x02; // QE=1 must be preserved
        mock.sr[2] = 0x00; // WPS=0

        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&ctx.chip);
        assert!(!dev.supports_status_write(opcodes::EWSR, 2));
        assert!(dev.supports_status_write(opcodes::WREN, 2));

        let payload = non_repeating_pattern(512, 0x55AA_1234);
        flash::write(&mut dev, &ctx, 0x1000, &payload)
            .await
            .unwrap();

        // Both data written AND (SR1, SR2) restored after mutation.
        let mut readback = vec![0u8; payload.len()];
        dev.read_flash(&ctx, 0x1000, &mut readback).await.unwrap();
        assert_eq!(readback, payload);
        assert_eq!((dev.transport.sr[0], dev.transport.sr[1]), (0x1C, 0x02));

        // Verify no 2-byte WRSR ever used EWSR.
        let wrsr_writes: Vec<&RecordedOp> = dev
            .transport
            .recorded
            .iter()
            .filter(|o| o.cmd == EZP_CMD_WRITE && o.ops[EZP_OP_PROGRAM] == opcodes::WRSR)
            .collect();
        assert_eq!(wrsr_writes.len(), 2, "expected unlock + restore WRSR");
        assert!(
            wrsr_writes
                .iter()
                .all(|o| o.ops[EZP_OP_WREN] == opcodes::WREN)
        );

        // Also verify erase on protected chip unlocks and restores.
        let mut device = SpiFlashDevice::new(dev, ctx);
        device.erase(0x1000, 4096).await.unwrap();
        let mut erased = vec![0u8; 4096];
        device.read(0x1000, &mut erased).await.unwrap();
        assert!(erased.iter().all(|&b| b == 0xFF));
        let (dev, _) = device.into_parts();
        assert_eq!((dev.transport.sr[0], dev.transport.sr[1]), (0x1C, 0x02));
    });
}

#[test]
fn ewsr_only_extended_wrsr_chip_and_explicit_volatile_wp_are_refused() {
    block_on(async {
        let db = ChipDatabase::new();
        let w25q = chip_by_name(&db, "W25Q128FV");

        // 1. Chip whose ONLY status write is EWSR + WRSR_EXT: automatic unlock fails preflight.
        let mut ewsr_only_chip = w25q.clone();
        ewsr_only_chip.features.remove(Features::WRSR_WREN);
        ewsr_only_chip.unlock = Unlock::Status;
        let ctx = FlashContext::new(ewsr_only_chip);

        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x18], 64 * 1024, 256);
        mock.enforce_bp = true;
        mock.sr[0] = 0x1C;
        mock.sr[1] = 0x02;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&ctx.chip);
        assert_eq!(
            flash::write(&mut dev, &ctx, 0, &[0x5A]).await,
            Err(CoreError::OpcodeNotSupported)
        );
        assert!(
            !dev.transport
                .recorded
                .iter()
                .any(|o| o.cmd == EZP_CMD_WRITE),
            "preflight must refuse before sending any write"
        );

        // 2. Explicit `--temporary` (volatile) WP write on W25Q128FV (2-byte WRSR)
        // must fail with OpcodeNotSupported and never substitute persistent WREN.
        let mock = MockEzpTransport::new([0xEF, 0x40, 0x18], 64 * 1024, 256);
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&w25q);
        let bit_map = WpRegBitMap::winbond_standard();
        let resolved = WriteOptions { volatile: true }
            .for_features(w25q.features)
            .unwrap();
        let mut bits = WpBits::empty();
        bits.bp_count = 3;
        bits.bp[0] = 1;
        assert_eq!(
            wp::write_wp_bits(&mut dev, &bits, &bit_map, resolved).await,
            Err(WpError::SpiError(CoreError::OpcodeNotSupported))
        );
        assert!(
            !dev.transport
                .recorded
                .iter()
                .any(|o| o.cmd == EZP_CMD_WRITE),
            "volatile WP request must not substitute persistent WREN"
        );
    });
}

#[test]
fn incompatible_1v8_chip_profile_is_rejected_after_identification() {
    block_on(async {
        let db = ChipDatabase::new();
        let chip_1v8 = chip_by_name(&db, "W25Q64FW");
        assert!(chip_1v8.voltage_max_mv < EZP_SUPPLY_MV);
        let ctx = FlashContext::new(chip_1v8.clone());

        let mock = MockEzpTransport::new([0xEF, 0x60, 0x17], 64 * 1024, 256);
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.chip_identified(&chip_1v8);

        assert_eq!(
            dev.check_access(FlashAccess::Read, 0, 256),
            Err(CoreError::ChipNotSupported)
        );
        let mut buf = [0u8; 16];
        assert_eq!(
            dev.read_flash(&ctx, 0, &mut buf).await,
            Err(CoreError::ChipNotSupported)
        );
        assert_eq!(
            dev.program_flash(&ctx, 0, &[0xAA; 16]).await,
            Err(CoreError::ChipNotSupported)
        );
        assert_eq!(
            dev.execute(&mut SpiCommand::read_reg(opcodes::RDSR, &mut buf[..1]))
                .await,
            Err(CoreError::ChipNotSupported)
        );
        // Zero commands sent after rejection.
        assert!(dev.transport.recorded.is_empty());
    });
}

#[test]
fn refuses_access_beyond_16_mib_and_unsupported_opcodes() {
    block_on(async {
        let db = ChipDatabase::new();
        let chip_32m = chip_by_name(&db, "W25Q256FV");
        let ctx = FlashContext::new(chip_32m);

        let mock = MockEzpTransport::new([0xEF, 0x40, 0x19], 4096, 256);
        let mut dev = EzpDevice::init(mock).await.unwrap();

        assert!(
            dev.check_access(FlashAccess::Read, 0, EZP_MAX_ADDR_SPACE as u32)
                .is_ok()
        );
        assert_eq!(
            dev.check_access(FlashAccess::Read, EZP_MAX_ADDR_SPACE as u32 - 1, 2),
            Err(CoreError::AddressOutOfBounds)
        );

        let mut buf = [0u8; 4];
        assert_eq!(
            dev.read_flash(&ctx, 0, &mut buf).await,
            Err(CoreError::ChipNotSupported)
        );
        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::READ,
                address: Some(EZP_MAX_ADDR_SPACE as u32 - 2),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::Single,
                dummy_cycles: 0,
                write_data: &[],
                read_buf: &mut buf,
            })
            .await,
            Err(CoreError::AddressOutOfBounds)
        );
        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::READ,
                address: Some(0),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::QuadOut,
                dummy_cycles: 0,
                write_data: &[],
                read_buf: &mut buf,
            })
            .await,
            Err(CoreError::IoModeNotSupported)
        );
        assert!(!dev.probe_opcode(opcodes::EN4B));
        assert_eq!(
            dev.execute(&mut SpiCommand::simple(opcodes::EN4B)).await,
            Err(CoreError::OpcodeNotSupported)
        );
    });
}

#[test]
fn timeout_and_dropped_future_poison_session_preventing_stale_reads() {
    block_on(async {
        // 1. Timed-out bulk IN payload leaving a stale 64-byte packet queued.
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::TimeoutBulkInPayloadLeavingStale;
        let mut dev = EzpDevice::init(mock).await.unwrap();

        let mut buf = [0u8; 16];
        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::READ,
                address: Some(0),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::Single,
                dummy_cycles: 0,
                write_data: &[],
                read_buf: &mut buf,
            })
            .await,
            Err(CoreError::Timeout)
        );
        // Clear fault: stale 64-byte packet is still sitting in `in_queue`.
        dev.transport.fault = Fault::None;
        let calls_after_failure = dev.transport.transport_calls;

        // A second command MUST fail immediately without touching the transport
        // or consuming the stale 64-byte packet as an ACK.
        let mut id = [0u8; 3];
        assert_eq!(
            dev.execute(&mut SpiCommand::read_reg(opcodes::RDID, &mut id))
                .await,
            Err(CoreError::ProgrammerError)
        );
        assert_eq!(dev.transport.transport_calls, calls_after_failure);
        assert_eq!(id, [0, 0, 0]);

        // 2. Dropped future mid-transfer must also poison the session.
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::SuspendOnceOnBulkIn;
        let mut dev = EzpDevice::init(mock).await.unwrap();

        {
            let mut id_buf = [0u8; 3];
            let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id_buf);
            let mut fut = Box::pin(dev.execute(&mut cmd));
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);
            assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
            // Drop `fut` while bulk_in is pending!
        }

        dev.transport.fault = Fault::None;
        let calls_after_drop = dev.transport.transport_calls;
        let mut id = [0u8; 3];
        assert_eq!(
            dev.execute(&mut SpiCommand::read_reg(opcodes::RDID, &mut id))
                .await,
            Err(CoreError::ProgrammerError)
        );
        assert_eq!(dev.transport.transport_calls, calls_after_drop);
    });
}

#[test]
fn polling_enforces_wall_clock_deadlines_with_usb_latency() {
    block_on(async {
        // 1. REQ_DONE poll with 150 ms control latency must expire at 500 ms (~4 polls),
        // not spin for 50 iterations (~7.5 s).
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::NeverDone;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.transport.control_latency = Duration::from_millis(150);

        let before = dev.transport.elapsed;
        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::PP,
                address: Some(0),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::Single,
                dummy_cycles: 0,
                write_data: &[0xAA; 16],
                read_buf: &mut [],
            })
            .await,
            Err(CoreError::Timeout)
        );
        let took = dev.transport.elapsed - before;
        assert!(
            dev.transport.done_poll_count <= 5,
            "expected wall-clock deadline (~4 polls), got {} polls ({took:?})",
            dev.transport.done_poll_count
        );

        // 2. CMD_STATUS poll with 2 s bulk latency per transfer must expire at the
        // 10 s block-erase deadline (~3 polls) rather than counting 1000 sleep intervals.
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.busy_polls_left = usize::MAX;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        dev.transport.bulk_latency = Duration::from_millis(1500);

        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::SE_20,
                address: Some(0),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::Single,
                dummy_cycles: 0,
                write_data: &[],
                read_buf: &mut [],
            })
            .await,
            Err(CoreError::Timeout)
        );
        assert!(
            dev.transport.status_poll_count <= 5,
            "expected wall-clock erase deadline (~3 polls), got {} polls",
            dev.transport.status_poll_count
        );
    });
}

#[test]
fn phase_specific_usb_faults_surface_clean_core_errors() {
    block_on(async {
        // Short bulk OUT
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::ShortOut;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        let mut id = [0u8; 3];
        assert_eq!(
            dev.execute(&mut SpiCommand::read_reg(opcodes::RDID, &mut id))
                .await,
            Err(CoreError::ProgrammerError)
        );

        // Empty bulk IN during ACK
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::EmptyBulkInAck;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::READ,
                address: Some(0),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::Single,
                dummy_cycles: 0,
                write_data: &[],
                read_buf: &mut buf,
            })
            .await,
            Err(CoreError::ProgrammerError)
        );

        // Empty bulk IN during payload (after full 64-byte ACK succeeded)
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::EmptyBulkInPayload;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        assert_eq!(
            dev.execute(&mut SpiCommand {
                opcode: opcodes::READ,
                address: Some(0),
                address_width: AddressWidth::ThreeByte,
                io_mode: IoMode::Single,
                dummy_cycles: 0,
                write_data: &[],
                read_buf: &mut buf,
            })
            .await,
            Err(CoreError::ProgrammerError)
        );

        // Invalid status byte during erase poll
        let mut mock = MockEzpTransport::new([0xEF, 0x40, 0x15], 4096, 256);
        mock.fault = Fault::BadStatus;
        let mut dev = EzpDevice::init(mock).await.unwrap();
        assert_eq!(
            dev.execute(&mut SpiCommand::simple(opcodes::CE_C7)).await,
            Err(CoreError::ProgrammerError)
        );
    });
}
