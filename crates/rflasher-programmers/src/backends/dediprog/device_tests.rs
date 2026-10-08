use super::*;
use futures_lite::future::block_on;
use rflasher_core::chip::{
    ChipTestStatus, EraseBlock, Features, FlashChip, Unlock, WriteGranularity,
};
use rflasher_core::flash::{FlashContext, FlashDevice, HybridFlashDevice};
use std::sync::{Arc, Mutex};

#[test]
fn only_exact_status_reads_retry_control_stalls() {
    block_on(async {
        for (command, read_len) in [
            (&[0x05][..], 1),
            (&[0x05][..], 2),
            (&[0x05, 0][..], 1),
            (&[0x35][..], 1),
            (&[0x06][..], 0),
            (&[0x04][..], 0),
            (&[0x01, 0][..], 0),
            (&[0x02, 0, 0, 0, 0][..], 0),
            (&[0x20, 0, 0, 0][..], 0),
        ] {
            let mut attempts = 0;
            let result =
                transceive_with_status_retry(command, read_len, STATUS_ACCESS_TIMEOUT, || {
                    attempts += 1;
                    std::future::ready(if attempts == 1 {
                        Err(DediprogError::UsbTransfer(TransferError::Stall))
                    } else {
                        Ok(vec![0; read_len])
                    })
                })
                .await;
            if command == [0x05] && read_len == 1 {
                assert!(result.is_ok());
                assert_eq!(attempts, 2);
            } else {
                assert!(matches!(
                    result,
                    Err(DediprogError::UsbTransfer(TransferError::Stall))
                ));
                assert_eq!(attempts, 1);
            }
        }
    });
}

#[test]
fn failed_status_reads_stay_bounded_and_do_not_hide_other_errors() {
    block_on(async {
        let mut attempts = 0;
        let result = transceive_with_status_retry(&[0x05], 1, Duration::from_millis(15), || {
            attempts += 1;
            std::future::ready(Err(DediprogError::UsbTransfer(TransferError::Stall)))
        })
        .await;
        assert!(matches!(result, Err(DediprogError::Timeout)));
        assert!((1..=2).contains(&attempts));

        for error in [
            TransferError::Disconnected,
            TransferError::Cancelled,
            TransferError::Fault,
            TransferError::InvalidArgument,
            TransferError::Unknown(42),
        ] {
            let mut attempts = 0;
            let result = transceive_with_status_retry(&[0x05], 1, STATUS_ACCESS_TIMEOUT, || {
                attempts += 1;
                std::future::ready(Err(DediprogError::UsbTransfer(error)))
            })
            .await;
            assert!(matches!(result, Err(DediprogError::UsbTransfer(e)) if e == error));
            assert_eq!(attempts, 1);
        }
        let result = transceive_with_status_retry(&[0x05], 1, STATUS_ACCESS_TIMEOUT, || {
            std::future::ready(Err(DediprogError::TransferFailed(
                "unclassified failure".into(),
            )))
        })
        .await;
        assert!(matches!(result, Err(DediprogError::TransferFailed(_))));
        let result = transceive_with_status_retry(&[0x05], 1, STATUS_ACCESS_TIMEOUT, || {
            std::future::ready(Ok(vec![]))
        })
        .await;
        assert!(matches!(result, Err(DediprogError::InvalidResponse(_))));
    });
}

struct PendingProgram {
    status_stalls_remaining: usize,
    addr: usize,
    data: Vec<u8>,
}
struct BridgeState {
    sr1: u8,
    wel: bool,
    pending: Option<PendingProgram>,
    data: Vec<u8>,
    status_stalls: usize,
    status_writes: Vec<u8>,
    fail_bulk: bool,
    bulk_writes: usize,
}
impl BridgeState {
    fn transfer(&mut self, command: &[u8]) -> Result<Vec<u8>> {
        if let Some(pending) = &mut self.pending {
            if pending.status_stalls_remaining != 0 {
                pending.status_stalls_remaining -= 1;
                // Like Dedibridge Shared::control: reject control SPI until the
                // page worker and its post-write settling have released the bus.
                assert_eq!(command, &[0x05], "a mutation reached the busy worker");
                self.status_stalls += 1;
                return Err(DediprogError::UsbTransfer(TransferError::Stall));
            }
            let pending = self.pending.take().unwrap();
            self.data[pending.addr..pending.addr + pending.data.len()]
                .copy_from_slice(&pending.data);
        }
        match command[0] {
            0x05 => Ok(vec![self.sr1]),
            0x06 => {
                self.wel = true;
                Ok(vec![])
            }
            0x01 => {
                assert!(self.wel);
                self.wel = false;
                let value = command[1];
                self.status_writes.push(value);
                // Atmel global unprotect/protect; mixed SWP leaves sectors alone.
                let swp = match value & 0x3c {
                    0 => 0,
                    0x3c => 0x0c,
                    _ => self.sr1 & 0x0c,
                };
                self.sr1 = (value & 0x80) | 0x10 | swp;
                Ok(vec![])
            }
            _ => panic!("unexpected SPI opcode {:02x}", command[0]),
        }
    }
}
#[derive(Clone)]
struct BusyBridge(Arc<Mutex<BridgeState>>);
impl SpiMaster for BusyBridge {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }
    fn max_read_len(&self) -> usize {
        16
    }
    fn max_write_len(&self) -> usize {
        11
    }
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        let mut command = vec![0; cmd.header_len() + cmd.write_data.len()];
        cmd.encode_header(&mut command);
        command[cmd.header_len()..].copy_from_slice(cmd.write_data);
        let response = transceive_with_status_retry(
            &command,
            cmd.read_buf.len(),
            STATUS_ACCESS_TIMEOUT,
            || std::future::ready(self.0.lock().unwrap().transfer(&command)),
        )
        .await
        .map_err(|_| CoreError::ProgrammerError)?;
        cmd.read_buf.copy_from_slice(&response);
        Ok(())
    }
    async fn delay_us(&mut self, _: u32) {}
}
impl OpaqueMaster for BusyBridge {
    fn size(&self) -> usize {
        self.0.lock().unwrap().data.len()
    }
    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        let state = self.0.lock().unwrap();
        assert!(state.pending.is_none());
        buf.copy_from_slice(&state.data[addr as usize..addr as usize + buf.len()]);
        Ok(())
    }
    async fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        let mut state = self.0.lock().unwrap();
        assert_eq!(state.sr1 & 0x8c, 0, "programming started without unlocking");
        state.bulk_writes += 1;
        state.pending = Some(PendingProgram {
            status_stalls_remaining: 3,
            addr: addr as usize,
            data: data[..if state.fail_bulk {
                data.len() / 2
            } else {
                data.len()
            }]
                .to_vec(),
        });
        // USB delivery completes (or fails after partial acceptance) before
        // buffered flash programming is finished; it is not a readiness barrier.
        if state.fail_bulk {
            Err(CoreError::WriteError { addr })
        } else {
            Ok(())
        }
    }
    async fn erase(&mut self, _: u32, _: u32) -> CoreResult<()> {
        panic!("no opaque erase")
    }
}

#[test]
fn hybrid_bulk_completion_waits_before_status_checks_and_protection_restore() {
    block_on(async {
        for fail_bulk in [false, true] {
            let state = Arc::new(Mutex::new(BridgeState {
                sr1: 0x9c,
                wel: false,
                pending: None,
                data: vec![0xff; 8192],
                status_stalls: 0,
                status_writes: vec![],
                fail_bulk,
                bulk_writes: 0,
            }));
            let ctx = FlashContext::new(FlashChip {
                vendor: "Test".into(),
                name: "AT25DF321-shaped".into(),
                jedec_manufacturer: 0x1f,
                jedec_device: 0x4700,
                total_size: 8192,
                page_size: 256,
                features: Features::WRSR_WREN,
                unlock: Unlock::At2x,
                voltage_min_mv: 2700,
                voltage_max_mv: 3600,
                write_granularity: WriteGranularity::Page,
                erase_blocks: vec![EraseBlock::new(0x20, 4096)],
                tested: ChipTestStatus::default(),
            });
            let mut device = crate::ErasedFlashDevice::new(HybridFlashDevice::new(
                BusyBridge(state.clone()),
                ctx,
            ));
            let result = device.write(4096, &[0x5f; 256]).await;
            assert_eq!(
                result,
                if fail_bulk {
                    Err(CoreError::WriteError { addr: 4096 })
                } else {
                    Ok(())
                }
            );
            let mut readback = [0; 256];
            device.read(4096, &mut readback).await.unwrap();
            let state = state.lock().unwrap();
            assert_eq!(state.sr1, 0x9c);
            assert!(state.pending.is_none());
            assert!(state.status_stalls > 0);
            assert_eq!(state.bulk_writes, 1);
            assert_eq!(state.status_writes, [0x04, 0, 0x04, 0xbc]);
            let changed = if fail_bulk { 128 } else { 256 };
            assert!(readback[..changed].iter().all(|b| *b == 0x5f));
            assert!(readback[changed..].iter().all(|b| *b == 0xff));
            assert!(state.data[..4096].iter().all(|b| *b == 0xff));
            assert!(state.data[4352..].iter().all(|b| *b == 0xff));
        }
    });
}
