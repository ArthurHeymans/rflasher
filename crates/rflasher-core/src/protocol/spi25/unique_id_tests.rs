use super::*;
use crate::chip::{ChipTestStatus, Features, FlashChip, WriteGranularity};
use crate::flash::{FlashContext, FlashDevice, HybridFlashDevice, SpiFlashDevice};
use crate::programmer::OpaqueMaster;
use alloc::{vec, vec::Vec};
use futures_lite::future::block_on;

struct UidMaster {
    response: [u8; 8],
    fail: bool,
    commands: usize,
}

impl SpiMaster for UidMaster {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }
    fn max_read_len(&self) -> usize {
        8
    }
    fn max_write_len(&self) -> usize {
        256
    }
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        self.commands += 1;
        assert_eq!(cmd.opcode, opcodes::RDUID);
        assert_eq!(cmd.address, None);
        assert_eq!(cmd.address_width, AddressWidth::None);
        assert_eq!(cmd.io_mode, IoMode::Single);
        assert_eq!(cmd.dummy_cycles, 32);
        assert!(cmd.write_data.is_empty());
        assert_eq!(cmd.read_buf.len(), 8);
        if self.fail {
            return Err(Error::SpiTransferFailed);
        }
        cmd.read_buf.copy_from_slice(&self.response);
        Ok(())
    }
    async fn delay_us(&mut self, _us: u32) {}
}

impl OpaqueMaster for UidMaster {
    fn size(&self) -> usize {
        8 * 1024 * 1024
    }
    async fn read(&mut self, _addr: u32, _buf: &mut [u8]) -> Result<()> {
        panic!("UID must use SPI, not opaque reads")
    }
    async fn write(&mut self, _addr: u32, _data: &[u8]) -> Result<()> {
        panic!("UID must not write flash")
    }
    async fn erase(&mut self, _addr: u32, _len: u32) -> Result<()> {
        panic!("UID must not erase flash")
    }
}

fn chip(manufacturer: u8, device: u16) -> FlashChip {
    FlashChip {
        vendor: "test".into(),
        name: "test".into(),
        jedec_manufacturer: manufacturer,
        jedec_device: device,
        total_size: 8 * 1024 * 1024,
        page_size: 256,
        features: Features::empty(),
        voltage_min_mv: 2700,
        voltage_max_mv: 3600,
        write_granularity: WriteGranularity::Byte,
        erase_blocks: vec![],
        tested: ChipTestStatus::default(),
    }
}

fn read_device_uid(hybrid: bool, chip: FlashChip, master: UidMaster) -> (Result<Vec<u8>>, usize) {
    let ctx = FlashContext::new(chip);
    if hybrid {
        let mut device = HybridFlashDevice::new(master, ctx);
        let result = block_on(device.read_unique_id());
        (result, device.into_parts().0.commands)
    } else {
        let mut device = SpiFlashDevice::new(master, ctx);
        let result = block_on(device.read_unique_id());
        (result, device.into_parts().0.commands)
    }
}

#[test]
fn supported_profiles_read_complete_uid_through_spi_and_hybrid() {
    let response = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
    for hybrid in [false, true] {
        for family in [0x4000, 0x6000, 0x7000] {
            for density in 0x14..=0x18 {
                let (result, commands) = read_device_uid(
                    hybrid,
                    chip(0xEF, family | density),
                    UidMaster {
                        response,
                        fail: false,
                        commands: 0,
                    },
                );
                assert_eq!(result.unwrap(), response);
                assert_eq!(commands, 1);
            }
        }
    }
}

#[test]
fn unsupported_uid_formats_do_not_send_commands() {
    // GD25Q64E's 128-bit UID shares a JEDEC ID with older GD25Q64 parts.
    // Macronix uses other UID commands; larger Winbond chips vary by address mode.
    for hybrid in [false, true] {
        for (manufacturer, device) in [
            (0xC8, 0x4017),
            (0xC2, 0x2017),
            (0xEF, 0x3017),
            (0xEF, 0x4019),
        ] {
            let (result, commands) = read_device_uid(
                hybrid,
                chip(manufacturer, device),
                UidMaster {
                    response: [0; 8],
                    fail: false,
                    commands: 0,
                },
            );
            assert!(matches!(result, Err(Error::ChipNotSupported)));
            assert_eq!(commands, 0);
        }
    }
}

#[test]
fn floating_response_is_unsupported_but_transfer_errors_are_preserved() {
    for hybrid in [false, true] {
        for fail in [false, true] {
            let (result, commands) = read_device_uid(
                hybrid,
                chip(0xEF, 0x4017),
                UidMaster {
                    response: [0xFF; 8],
                    fail,
                    commands: 0,
                },
            );
            if fail {
                assert!(matches!(result, Err(Error::SpiTransferFailed)));
            } else {
                assert!(matches!(result, Err(Error::ChipNotSupported)));
            }
            assert_eq!(commands, 1);
        }
    }
}
