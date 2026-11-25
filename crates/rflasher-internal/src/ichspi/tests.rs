use super::*;
use crate::host::PciConfigAccess;
use crate::host::tests::{FakeHost, FakeMmio};
use alloc::{vec, vec::Vec};

fn test_controller() -> IchSpiController<FakeHost> {
    IchSpiController {
        host: FakeHost::default(),
        spibar: FakeMmio::default(),
        generation: IchChipset::Series100SunrisePoint,
        config_bdf: PciAddress::new(0, 0, 0x1f, 0),
        locked: false,
        bios_writes_enabled: false,
        silvermont_bcr: false,
        restore_bcr_wpd: false,
        restore_pci_bwe: false,
        changes: [None; 12],
        change_count: 0,
        swseq_locked: false,
        dlock: 0,
        desc_valid: false,
        requested_mode: SpiMode::HardwareSequencing,
        mode: SpiMode::HardwareSequencing,
        swseq: SwseqRegs {
            ssfsc: PCH100_REG_SSFSC,
            preop: PCH100_REG_PREOP,
            optype: PCH100_REG_OPTYPE,
            opmenu: PCH100_REG_OPMENU,
        },
        ich7_swseq: Ich7SwseqRegs {
            spis: ICH7_REG_SPIS,
            spic: ICH7_REG_SPIC,
            spia: ICH7_REG_SPIA,
            spid0: ICH7_REG_SPID0,
            preop: ICH7_REG_PREOP,
            optype: ICH7_REG_OPTYPE,
            opmenu: ICH7_REG_OPMENU,
        },
        hwseq: HwseqData {
            addr_mask: PCH100_FADDR_FLA,
            only_4k: true,
            hsfc_fcycle: PCH100_HSFC_FCYCLE,
            size_comp0: 0,
            size_comp1: 0,
        },
        opcodes: None,
        pending_preop: None,
        rdsr2: false,
        bbar: 0,
    }
}

#[test]
fn init_read_and_drop_restores_menu_and_protection() {
    let mut c = test_controller();
    let mmio = c.spibar.clone();
    mmio.set32(ICH9_REG_HSFS, u32::from(HSFS_FDV));
    mmio.set32(PCH100_REG_PREOP, 0x1234);
    mmio.set32(PCH100_REG_FPR0, 0x8000_0001);
    c.wait_configuration_idle().unwrap();
    c.init().unwrap();
    c.hwseq_read(0, &mut [0; 4]).unwrap();
    drop(c);
    assert_eq!(mmio.read16(PCH100_REG_PREOP), 0x1234);
    assert_eq!(mmio.read32(PCH100_REG_FPR0), 0x8000_0001);
}

#[test]
fn init_failure_restores_configuration_in_reverse() {
    let mut c = test_controller();
    c.requested_mode = SpiMode::HardwareSequencing; // descriptor invalid
    let mmio = c.spibar.clone();
    mmio.set32(PCH100_REG_PREOP, 0x1234);
    mmio.set32(PCH100_REG_FPR0, 0x8000_0001);
    assert!(c.init().is_err());
    drop(c);
    assert_eq!(mmio.read16(PCH100_REG_PREOP), 0x1234);
    let writes = mmio.writes();
    let last = |offset| writes.iter().rposition(|w| w.0 == offset).unwrap();
    assert!(last(PCH100_REG_OPMENU + 4) < last(PCH100_REG_PREOP));
    // Opening never clears protected ranges.
    assert!(writes.iter().all(|w| w.0 != PCH100_REG_FPR0));
}

#[test]
fn restore_preserves_external_changes_and_bbar_unrelated_bits() {
    let mut c = test_controller();
    c.generation = IchChipset::Ich9;
    let mmio = c.spibar.clone();
    mmio.set32(ICH9_REG_BBAR, 0x123400 | 1);
    c.set_bbar(0);
    mmio.set32(ICH9_REG_BBAR, 2);
    mmio.set32(ICH9_REG_PR0, 0x8000_0001);
    c.set_pr(ICH9_REG_PR0, 0, false, false);
    mmio.set32(ICH9_REG_PR0, 0x0000_0002);
    drop(c);
    assert_eq!(mmio.read32(ICH9_REG_BBAR), 0x123402);
    assert_eq!(mmio.read32(ICH9_REG_PR0), 2);
}

#[test]
fn individual_locks_only_block_their_own_restoration() {
    let mut c = test_controller();
    let mmio = c.spibar.clone();
    let (pr0, pr1) = (pr(0, 0xfff, false, true), pr(0x1000, 0x1fff, true, true));
    mmio.set32(PCH100_REG_FPR0, pr0);
    mmio.set32(PCH100_REG_FPR0 + 4, pr1);
    c.clear_protected_ranges().unwrap();
    mmio.set32(PCH100_REG_DLOCK, DLOCK_PR0_LOCKDN | DLOCK_SSEQ_LOCKDN);
    drop(c);
    assert_eq!(mmio.read32(PCH100_REG_FPR0), pr(0, 0xfff, false, false));
    assert_eq!(mmio.read32(PCH100_REG_FPR0 + 4), pr1);
}

#[test]
fn restore_skips_locked_or_busy_controller() {
    for busy in [false, true] {
        let mut c = test_controller();
        let mmio = c.spibar.clone();
        mmio.set32(PCH100_REG_PREOP, 0x1234);
        c.change_register(PCH100_REG_PREOP, true, 0x5678, 0xffff);
        if busy {
            mmio.set32(PCH100_REG_SSFSC, SSFS_SCIP);
        } else {
            mmio.set32(ICH9_REG_HSFS, u32::from(HSFS_FLOCKDN));
        }
        drop(c);
        assert_eq!(mmio.read16(PCH100_REG_PREOP), 0x5678);
    }
}

#[test]
fn drop_clears_owned_write_enable_even_when_busy() {
    for silvermont in [false, true] {
        for busy in [0, 1, 2] {
            let mut c = test_controller();
            c.silvermont_bcr = silvermont;
            c.host.enable_write8_readback();
            let host = c.host.clone();
            let mmio = c.spibar.clone();
            let bdf = PciAddress::new(0, 0, 0x1f, 0);
            host.set_config32(bdf, PCI_REG_BIOS_CNTL as u16 & !3, 0);
            mmio.set32(SILVERMONT_REG_BCR, 0);
            mmio.set32(PCH100_REG_PREOP, 0x1234);
            c.change_register(PCH100_REG_PREOP, true, 0x5678, 0xffff);
            c.enable_bios_write().unwrap();
            if busy == 1 {
                mmio.set32(PCH100_REG_SSFSC, SSFS_SCIP | SSFS_FDONE | SSFS_FCERR);
            } else if busy == 2 {
                mmio.set32(
                    ICH9_REG_HSFS,
                    u32::from(HSFS_SCIP | HSFS_FDONE | HSFS_FCERR),
                );
            }
            let writes_before = mmio.writes().len();
            drop(c);
            assert_eq!(
                mmio.read16(PCH100_REG_PREOP),
                if busy == 0 { 0x1234 } else { 0x5678 }
            );
            if silvermont {
                assert_eq!(
                    mmio.read32(SILVERMONT_REG_BCR) & u32::from(BIOS_CNTL_BWE),
                    0
                );
            } else {
                assert_eq!(
                    host.read8(bdf, PCI_REG_BIOS_CNTL as u16).unwrap() & BIOS_CNTL_BWE,
                    0
                );
                assert_eq!(
                    host.writes().last().unwrap().2 & u32::from(BIOS_CNTL_BWE),
                    0
                );
            }
            assert!(
                mmio.writes()[writes_before..]
                    .iter()
                    .all(|&(offset, _, _)| offset != ICH9_REG_HSFS && offset != PCH100_REG_SSFSC)
            );
        }
    }
}

#[test]
fn swseq_address_limits_and_erase_geometry() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.mode = SpiMode::SoftwareSequencing;
        c.generation = if ich7 {
            IchChipset::Ich7
        } else {
            IchChipset::Ich9
        };
        c.opcodes = Some(Opcodes::default());
        let host = c.host.clone();
        let mmio = c.spibar.clone();
        let limit = 1 << 24;

        // A later chunk would wrap to zero; reject the entire operation.
        assert!(c.controller_read(limit - 64, &mut [0; 65], 0).is_err());
        assert!(c.controller_write(limit - 64, &[0; 65]).is_err());
        assert!(c.controller_read(limit, &mut [0; 1], 0).is_err());
        assert!(c.controller_write(u32::MAX, &[0; 1]).is_err());
        assert!(c.controller_erase(0, 4096).is_err());
        assert!(c.controller_erase(limit - 4096, 8192).is_err());

        let mut cmd = SpiCommand::write_3b(JEDEC_BYTE_PROGRAM, limit, &[1]);
        assert!(c.execute(&mut cmd).is_err());
        let mut cmd = SpiCommand::erase_3b(JEDEC_SE, limit);
        assert!(c.execute(&mut cmd).is_err());
        let mut buf = [0; 2];
        let mut cmd = SpiCommand::read_3b(JEDEC_READ, limit - 1, &mut buf);
        assert!(c.execute(&mut cmd).is_err());

        let crossing = [JEDEC_BYTE_PROGRAM, 0xff, 0xff, 0xff, 1, 2];
        let mut too_long = [0; 69];
        too_long[0] = JEDEC_BYTE_PROGRAM;
        for unchecked in [false, true] {
            for bytes in [&crossing[..], &too_long[..]] {
                // SAFETY: the fake controller is not connected to flash.
                let result = unsafe {
                    match (ich7, unchecked) {
                        (true, true) => c.ich7_swseq_send_command_unchecked(bytes, &mut []),
                        (false, true) => c.swseq_send_command_unchecked(bytes, &mut []),
                        (true, false) => c.ich7_swseq_send_command(bytes, &mut []),
                        (false, false) => c.swseq_send_command(bytes, &mut []),
                    }
                };
                assert!(result.is_err());
            }
        }
        assert!(host.writes().is_empty());
        assert!(mmio.writes().is_empty());
        assert!(IchSpiController::<FakeHost>::check_swseq_range(limit - 64, 64).is_ok());
    }
}

#[test]
fn chip_specific_erase_still_uses_raw_command() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.mode = SpiMode::SoftwareSequencing;
        c.generation = if ich7 {
            IchChipset::Ich7
        } else {
            IchChipset::Ich9
        };
        c.opcodes = Some(Opcodes::default());
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        let host = c.host.clone();
        let mmio = c.spibar.clone();
        c.execute(&mut SpiCommand::simple(JEDEC_WREN)).unwrap();
        let _ = c.execute(&mut SpiCommand::erase_3b(JEDEC_SE, 0xfff000));
        // The fake may report a cycle error, but the command was issued.
        assert!(!host.writes().is_empty());
        assert!(!mmio.writes().is_empty());
    }
}

#[test]
fn erase_blocks_use_the_callers_geometry() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.mode = SpiMode::SoftwareSequencing;
        c.generation = if ich7 {
            IchChipset::Ich7
        } else {
            IchChipset::Ich9
        };
        c.opcodes = Some(Opcodes::default());
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        let host = c.host.clone();
        let mmio = c.spibar.clone();

        // Rejected before any cycle: not an erase, misaligned, past 16 MiB.
        for (opcode, size, addr, len) in [
            (JEDEC_BYTE_PROGRAM, 4096, 0, 4096),
            (JEDEC_CE_C7, 4096, 0, 4096),
            (JEDEC_SE, 0, 0, 4096),
            (JEDEC_SE, 3000, 0, 6000),
            (JEDEC_SE, 4096, 2048, 4096),
            (JEDEC_SE, 4096, 0, 2048),
            (JEDEC_BE_D8, 65536, (1 << 24) - 65536, 2 * 65536),
        ] {
            assert!(c.swseq_erase_blocks(opcode, size, addr, len).is_err());
        }
        assert!(host.writes().is_empty());
        assert!(mmio.writes().is_empty());

        // The fake may report a cycle error, but the command was issued.
        let _ = c.swseq_erase_blocks(JEDEC_SE, 4096, 0xff0000, 0x10000);
        assert!(!host.writes().is_empty());
        assert!(!mmio.writes().is_empty());

        c.mode = SpiMode::HardwareSequencing;
        assert!(c.swseq_erase_blocks(JEDEC_SE, 4096, 0, 4096).is_err());
    }
}

#[test]
fn erase_blocks_preflight_the_entire_protected_range() {
    for generation in [
        IchChipset::Ich7,
        IchChipset::Ich9,
        IchChipset::Series100SunrisePoint,
    ] {
        // Protect either a later block's first page or a page inside it.
        for protected in [0x10000, 0x11000] {
            let mut c = test_controller();
            c.generation = generation;
            c.mode = SpiMode::SoftwareSequencing;
            c.locked = true;
            c.opcodes = Some(Opcodes::default());
            let index = c.find_opcode_index(JEDEC_SE).unwrap();
            c.opcodes.as_mut().unwrap().opcode[index].opcode = JEDEC_BE_D8;
            let protection = if generation == IchChipset::Ich7 {
                (protected >> 12) | (protected >> 12) << 12 | 1 << PR_WP_OFF
            } else {
                pr(protected, protected + 0xfff, false, true)
            };
            c.spibar.set32(c.pr_registers().0, protection);
            assert!(matches!(
                c.swseq_erase_blocks(JEDEC_BE_D8, 0x10000, 0, 0x20000),
                Err(InternalError::AccessDenied {
                    region: "protected range"
                })
            ));
            assert!(c.host.writes().is_empty());
            assert!(c.spibar.writes().is_empty());
        }
    }
}

#[test]
fn erase_blocks_require_status_polling_before_any_cycle() {
    for generation in [IchChipset::Ich7, IchChipset::Ich9] {
        for missing in [false, true] {
            let mut c = test_controller();
            c.generation = generation;
            c.mode = SpiMode::SoftwareSequencing;
            c.opcodes = Some(Opcodes::default());
            c.host.enable_write8_readback();
            c.host
                .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
            let index = c.find_opcode_index(JEDEC_RDSR).unwrap();
            let status = &mut c.opcodes.as_mut().unwrap().opcode[index];
            if missing {
                status.opcode = 0;
            } else {
                status.spi_type = SPI_OPCODE_TYPE_WRITE_NO_ADDRESS;
            }
            assert!(c.swseq_erase_blocks(JEDEC_SE, 4096, 0, 4096).is_err());
            assert!(c.host.writes().is_empty());
            assert!(c.spibar.writes().is_empty());
        }
    }
}

#[test]
fn erase_blocks_complete_each_block_and_leave_neighbors_untouched() {
    for (opcode, size) in [(JEDEC_SE, 4096), (JEDEC_BE_52, 32768), (JEDEC_BE_D8, 65536)] {
        let mut c = test_controller();
        c.generation = IchChipset::Ich9;
        c.mode = SpiMode::SoftwareSequencing;
        c.opcodes = Some(Opcodes::default());
        // Model a menu containing the caller's erase opcode.
        let index = c.find_opcode_index(JEDEC_SE).unwrap();
        c.opcodes.as_mut().unwrap().opcode[index].opcode = opcode;
        c.spibar.set_swseq_menu(
            c.swseq.ssfsc,
            c.opcodes.as_ref().unwrap().opcode.map(|o| o.opcode),
        );
        c.spibar.set_flash(vec![0; 4 * size as usize]);
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        c.swseq_erase_blocks(opcode, size, size, 2 * size).unwrap();
        let menu = c.opcodes.as_ref().unwrap().opcode.map(|o| o.opcode);
        let cycles: Vec<_> = c
            .spibar
            .writes()
            .iter()
            .filter(|&&(offset, value, _)| offset == c.swseq.ssfsc && value & SSFC_SCGO != 0)
            .map(|&(_, value, _)| menu[((value & SSFC_COP) >> SSFC_COP_OFF) as usize])
            .collect();
        assert_eq!(cycles, [opcode, JEDEC_RDSR, opcode, JEDEC_RDSR]);
        let flash = c.spibar.flash();
        assert!(flash[..size as usize].iter().all(|&b| b == 0));
        assert!(
            flash[size as usize..3 * size as usize]
                .iter()
                .all(|&b| b == 0xff)
        );
        assert!(flash[3 * size as usize..].iter().all(|&b| b == 0));
    }
}

#[test]
fn direct_paths_gate_mutations_and_restore() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        c.opcodes = Some(Opcodes::default());
        c.generation = if ich7 {
            IchChipset::Ich7
        } else {
            IchChipset::Ich9
        };
        let host = c.host.clone();
        // A refused command never enables writes, regardless of framing.
        c.opcodes.as_mut().unwrap().opcode[0].opcode = 0xaa;
        let send = |c: &mut IchSpiController<FakeHost>, cmd: &[u8], out: &mut [u8]| {
            if ich7 {
                c.ich7_swseq_send_command(cmd, out)
            } else {
                c.swseq_send_command(cmd, out)
            }
        };
        assert!(send(&mut c, &[0xaa, 0, 0, 0], &mut []).is_err());
        assert!(host.writes().is_empty());
        let _ = send(&mut c, &[JEDEC_RDSR], &mut [0]);
        assert!(host.writes().is_empty());
        // A wrongly framed mutation is rejected before enabling writes.
        c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
            opcode: JEDEC_CE_C7,
            spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            atomic: 0,
        };
        host.set_config32(
            c.config_bdf,
            PCI_REG_BIOS_CNTL as u16,
            u32::from(BIOS_CNTL_SMM_BWP),
        );
        assert!(matches!(
            send(&mut c, &[JEDEC_CE_C7], &mut [0]),
            Err(InternalError::NotSupported(_))
        ));
        assert!(host.writes().is_empty());
        host.set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        c.opcodes.as_mut().unwrap().opcode[0].spi_type = SPI_OPCODE_TYPE_WRITE_NO_ADDRESS;
        // Cycle may fail in the fake, but enable must precede it and be restored.
        let _ = send(&mut c, &[JEDEC_CE_C7], &mut []);
        assert_eq!(host.writes().len(), 1);
        drop(c);
        assert_eq!(host.writes().len(), 2);
        assert_eq!(host.writes()[1].2 & 1, 0);
    }
}

#[test]
fn locked_menu_refuses_missing_preop_and_bad_framing() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.locked = true;
        c.mode = SpiMode::SoftwareSequencing;
        c.generation = if ich7 {
            IchChipset::Ich7
        } else {
            IchChipset::Ich9
        };
        c.opcodes = Some(Opcodes::default());
        c.opcodes.as_mut().unwrap().preop = [0, 0];
        c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
            opcode: JEDEC_BYTE_PROGRAM,
            spi_type: SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS,
            atomic: 0,
        };
        let host = c.host.clone();
        let mmio = c.spibar.clone();
        for opcode in [JEDEC_WREN, JEDEC_EWSR] {
            let mut preop = SpiCommand::simple(opcode);
            assert!(c.execute(&mut preop).is_err());
            assert!(c.pending_preop.is_none());
        }
        let send = |c: &mut IchSpiController<FakeHost>| {
            if ich7 {
                c.ich7_swseq_send_command(&[JEDEC_BYTE_PROGRAM, 0, 0, 0, 0xaa], &mut [])
            } else {
                c.swseq_send_command(&[JEDEC_BYTE_PROGRAM, 0, 0, 0, 0xaa], &mut [])
            }
        };
        assert!(send(&mut c).is_err());
        c.opcodes.as_mut().unwrap().preop = [JEDEC_WREN, 0];
        c.opcodes.as_mut().unwrap().opcode[0].spi_type = SPI_OPCODE_TYPE_WRITE_NO_ADDRESS;
        assert!(!c.probe_opcode(JEDEC_BYTE_PROGRAM));
        assert!(send(&mut c).is_err());
        assert!(host.writes().is_empty());
        assert!(mmio.writes().is_empty());
    }
}

#[test]
fn locked_menu_identification_reads_and_ambiguous_opcode() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.locked = true;
        c.mode = SpiMode::SoftwareSequencing;
        c.generation = if ich7 {
            IchChipset::Ich7
        } else {
            IchChipset::Ich9
        };
        c.opcodes = Some(Opcodes::default());
        let host = c.host.clone();
        let send = |c: &mut IchSpiController<FakeHost>, cmd: &[u8], out: &mut [u8]| {
            if ich7 {
                c.ich7_swseq_send_command(cmd, out)
            } else {
                c.swseq_send_command(cmd, out)
            }
        };
        for opcode in [JEDEC_RES, 0x4b] {
            c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
                opcode,
                spi_type: SPI_OPCODE_TYPE_READ_WITH_ADDRESS,
                atomic: 0,
            };
            c.spibar.set32(ICH7_REG_OPMENU, u32::from(opcode));
            assert!(c.probe_opcode(opcode));
            let writes_before = c.spibar.writes().len();
            // The fake may report a cycle error; verify the cycle was issued.
            let _ = send(&mut c, &[opcode, 0, 0, 0], &mut [0; 2]);
            let control = if ich7 {
                ICH7_REG_SPIC
            } else {
                PCH100_REG_SSFSC
            };
            assert!(
                c.spibar.writes()[writes_before..]
                    .iter()
                    .any(|w| w.0 == control)
            );
            assert!(host.writes().is_empty());
        }
        c.opcodes.as_mut().unwrap().opcode[0].opcode = 0x35;
        assert!(!c.probe_opcode(0x35));
        assert!(matches!(
            send(&mut c, &[0x35, 0, 0, 0], &mut [0]),
            Err(InternalError::NotSupported("Unclassified SPI opcode"))
        ));
        assert!(host.writes().is_empty());
    }
}

#[test]
fn unsafe_unknown_command_is_always_treated_as_mutating() {
    let mut c = test_controller();
    c.host.enable_write8_readback();
    c.host
        .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    c.opcodes = Some(Opcodes::default());
    c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
        opcode: 0xaa,
        spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
        atomic: 0,
    };
    let host = c.host.clone();
    assert!(!c.probe_opcode(0xaa));
    // SAFETY: the fake controller has no physical flash.
    let _ = unsafe { c.swseq_send_command_unchecked(&[0xaa], &mut [0]) };
    assert_eq!(host.writes().len(), 1);
    drop(c);
    assert_eq!(host.writes().len(), 2);
}

#[test]
fn ambiguous_opcode_unchecked_still_obeys_write_protection() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        c.opcodes = Some(Opcodes::default());
        c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
            opcode: 0x35,
            spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            atomic: 0,
        };
        c.host.set_config32(
            c.config_bdf,
            PCI_REG_BIOS_CNTL as u16,
            u32::from(BIOS_CNTL_SMM_BWP),
        );
        // SAFETY: the fake controller has no physical flash.
        let result = unsafe {
            if ich7 {
                c.ich7_swseq_send_command_unchecked(&[0x35], &mut [0])
            } else {
                c.swseq_send_command_unchecked(&[0x35], &mut [0])
            }
        };
        assert!(matches!(result, Err(InternalError::ChipsetEnable(_))));
        assert_eq!(
            c.host.writes(),
            vec![(
                c.config_bdf,
                PCI_REG_BIOS_CNTL as u16,
                u32::from(BIOS_CNTL_BWE),
                1
            )]
        );
        assert!(c.spibar.writes().is_empty());
    }
}

#[test]
fn failed_lazy_enable_is_restored_and_status_write_is_gated() {
    let mut c = test_controller();
    c.opcodes = Some(Opcodes::default());
    c.host
        .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    let host = c.host.clone();
    assert!(c.swseq_send_command(&[JEDEC_WRSR, 0], &mut []).is_err());
    assert!(!c.writes_enabled());
    assert_eq!(host.writes().len(), 1);
    drop(c);
    assert_eq!(host.writes().len(), 2);
    assert_eq!(host.writes()[1].2 & 1, 0);
}

#[test]
fn read_probe_and_hwseq_mutations_are_lazy() {
    let mut c = test_controller();
    c.host.enable_write8_readback();
    c.host
        .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    let host = c.host.clone();
    let shared: &IchSpiController<FakeHost> = &c;
    shared.hwseq_read(0, &mut [0; 1]).unwrap();
    assert!(!c.probe_opcode(JEDEC_READ));
    assert!(host.writes().is_empty());
    c.hwseq_write(0, &[0]).unwrap();
    c.hwseq_erase(0, 4096).unwrap();
    assert_eq!(host.writes().len(), 1);
    drop(c);
    assert_eq!(host.writes().len(), 2);
}

#[test]
fn configuration_wait_polls_hardware_sequencer_without_status_writes() {
    let c = test_controller();
    c.spibar.set_hsfs_busy_reads(3);
    c.wait_configuration_idle().unwrap();
    assert!(c.spibar.writes().is_empty());
}

#[test]
fn full_journal_never_makes_untracked_changes() {
    let mut c = test_controller();
    let mmio = c.spibar.clone();
    for i in 0..c.changes.len() {
        c.change_register(0x200 + i * 4, false, 1, u32::MAX);
    }
    c.change_register(0x300, false, 1, u32::MAX);
    assert_eq!(mmio.read32(0x300), 0);
    drop(c);
    for i in 0..12 {
        assert_eq!(mmio.read32(0x200 + i * 4), 0);
    }
}

#[test]
fn descriptor_densities_and_controller_limits() {
    let mut c = test_controller();
    c.desc_valid = true;
    c.spibar.set32(0xb4, 0x20);
    // 8 MiB component, ignoring the unused second density field.
    c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 0, 0xf4);
    assert_eq!(c.descriptor_flash_size().unwrap(), Some(8 << 20));
    assert_eq!(c.spibar.read32(0xb4), 0x20);
    // 8 + 16 MiB, even if descriptor regions leave a trailing gap.
    c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 1 << 8, 0x54);
    assert_eq!(c.descriptor_flash_size().unwrap(), Some(24 << 20));
    c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 2 << 8, 0x54);
    assert!(c.descriptor_flash_size().is_err());
    c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 1 << 8, 0x84);
    assert!(c.descriptor_flash_size().is_err());
    c.spibar.set_descriptor(0xb4, 0, 0, 4);
    assert!(c.descriptor_flash_size().is_err());
    c.desc_valid = false;
    assert!(c.descriptor_flash_size().is_err());
    c.desc_valid = true;
    c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 1 << 8, 0x77);
    assert_eq!(c.descriptor_flash_size().unwrap(), Some(128 << 20));
    c.hwseq.addr_mask = ICH9_FADDR_FLA;
    assert!(c.descriptor_flash_size().is_err()); // 128 MiB > SPI limit

    c.generation = IchChipset::BayTrail;
    c.hwseq.addr_mask = ICH9_FADDR_FLA;
    c.spibar
        .set_descriptor(0xb0, 0x0ff0_a55a, 1 << 8, 4 | (5 << 3));
    assert_eq!(c.descriptor_flash_size().unwrap(), Some(24 << 20));
    c.spibar.set_descriptor(0xb0, 0x0ff0_a55a, 0, 6);
    assert!(c.descriptor_flash_size().is_err());
    c.generation = IchChipset::ApolloLake;
    assert_eq!(c.descriptor_flash_size().unwrap(), None);
}

#[test]
fn swseq_writes_use_the_preop_sent_before_them() {
    let mut controller = test_controller();
    controller.mode = SpiMode::SoftwareSequencing;
    controller.opcodes = Some(Opcodes::default());
    assert!(controller.probe_opcode(JEDEC_WREN));
    assert!(controller.probe_opcode(JEDEC_EWSR));
    // SST25VF064C: EWSR + WRSR selects the EWSR slot, not WREN.
    assert_eq!(
        controller
            .select_atomic(JEDEC_WRSR, Some(JEDEC_EWSR))
            .unwrap(),
        2
    );
    assert_eq!(
        controller
            .select_atomic(JEDEC_WRSR, Some(JEDEC_WREN))
            .unwrap(),
        1
    );
    assert_eq!(controller.select_atomic(JEDEC_WRSR, None).unwrap(), 1);
    // A locked table without EWSR must not silently fall back to WREN.
    controller.opcodes.as_mut().unwrap().preop = [JEDEC_WREN, 0];
    assert!(!controller.probe_opcode(JEDEC_EWSR));
    assert!(
        controller
            .select_atomic(JEDEC_WRSR, Some(JEDEC_EWSR))
            .is_err()
    );
}

#[test]
fn hwseq_rejects_unrepresentable_ranges_before_mmio() {
    for mask in [ICH9_FADDR_FLA, PCH100_FADDR_FLA] {
        let mut controller = test_controller();
        controller.hwseq.addr_mask = mask;
        assert!(controller.hwseq_check_range(mask, 1).is_ok());
        assert!(controller.hwseq_check_range(mask, 2).is_err());
        assert!(controller.hwseq_check_range(mask + 1, 0).is_err());
        assert!(controller.hwseq_read(mask, &mut [0; 2]).is_err());
        assert!(controller.hwseq_write(mask, &[0; 2]).is_err());
        assert!(controller.hwseq_erase(mask + 1, 4096).is_err());
        assert!(controller.hwseq_erase(mask - 4095, 8192).is_err());
    }

    let mut controller = test_controller();
    controller.hwseq.addr_mask = u32::MAX;
    assert!(controller.hwseq_check_range(u32::MAX, 1).is_ok());
    assert!(controller.hwseq_check_range(u32::MAX, 2).is_err());
    assert!(controller.hwseq_check_range(2, usize::MAX).is_err());
    assert!(controller.hwseq_check_range(0xffff_f000, 4096).is_ok());
}

#[test]
fn berase_decodes_every_hardware_erase_size() {
    for (encoding, size) in [(0u16, 256u32), (1, 4096), (2, 8192), (3, 65536)] {
        // Other HSFS bits (status flags, FDOPSS, ...) must not disturb the decode.
        let hsfs = (encoding << HSFS_BERASE_OFF) | HSFS_FDONE | HSFS_SCIP | (1 << 13);
        assert_eq!(berase_block_size(hsfs), size);
    }
}

#[test]
fn uniform_erase_size_handles_partitions() {
    const K: u32 = 1024;
    // One size everywhere, with or without a partition boundary.
    assert_eq!(uniform_erase_size(4 * K, 4 * K, 0), Some(4 * K));
    assert_eq!(uniform_erase_size(64 * K, 64 * K, 0x10_0000), Some(64 * K));
    // Different sizes: plan with the larger one if the partitions meet on
    // one of its blocks ...
    assert_eq!(uniform_erase_size(4 * K, 64 * K, 0x10_0000), Some(64 * K));
    assert_eq!(uniform_erase_size(64 * K, 4 * K, 0x10_0000), Some(64 * K));
    // ... and refuse when a block would straddle the boundary.
    assert_eq!(uniform_erase_size(4 * K, 64 * K, 0x10_1000), None);
}

/// An ICH9-style controller (BERASE-selected erase size) over a simulated
/// sequencer whose partitions report `low`/`high` BERASE encodings.
fn ich9_controller(low: u16, high: u16, fpb: u32) -> IchSpiController<FakeHost> {
    let mut controller = test_controller();
    controller.host.enable_write8_readback();
    controller
        .host
        .set_config32(controller.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    controller.generation = IchChipset::Ich9;
    controller.hwseq = HwseqData {
        addr_mask: ICH9_FADDR_FLA,
        only_4k: false,
        hsfc_fcycle: HSFC_FCYCLE,
        size_comp0: 0,
        size_comp1: 0,
    };
    controller.spibar.set_berase(low, high);
    controller.spibar.set32(ICH9_REG_FPB, fpb);
    controller
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn hwseq_flash_needs_address_erase_and_whole_blocks() {
    use crate::programmer::HwseqFlash;

    // 64 KiB hardware blocks.
    for size in [0, 4096, (1 << 20) + 4096] {
        let refused = HwseqFlash::with_size(ich9_controller(3, 3, 0), size);
        assert!(
            matches!(refused, Err(InternalError::NotSupported(_))),
            "{size}"
        );
    }
    // Software sequencing sends raw commands: the chip decides geometry.
    let refused = HwseqFlash::with_size(swseq_controller(vec![0; 1 << 20]), 1 << 20);
    assert!(matches!(refused, Err(InternalError::NotSupported(_))));
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn hwseq_flash_plans_with_the_controller_block_size() {
    use crate::programmer::HwseqFlash;
    use futures_lite::future::block_on;
    use rflasher_core::flash::FlashDevice;

    let controller = ich9_controller(3, 3, 0);
    controller.host.set_config32(
        controller.config_bdf,
        PCI_REG_BIOS_CNTL as u16,
        u32::from(BIOS_CNTL_SMM_BWP),
    );
    let mut flash = HwseqFlash::with_size(controller, 1 << 20).unwrap();
    assert_eq!(flash.erase_granularity(), 64 * 1024);
    assert_eq!(flash.erase_blocks()[0].total_size(), 1 << 20);
    // Writable SMM_BWP must not prevent the lazy enable.
    block_on(flash.erase(0, 64 * 1024)).unwrap();
    assert_eq!(
        block_on(flash.read(1 << 20, &mut [0])),
        Err(CoreError::AddressOutOfBounds)
    );
}

/// A PRx/FPRx value restricting `base..=limit`; its bits enable protection.
fn pr(base: u32, limit: u32, read: bool, write: bool) -> u32 {
    (base >> 12)
        | (limit >> 12) << 16
        | u32::from(read) << PR_RP_OFF
        | u32::from(write) << PR_WP_OFF
}

#[test]
fn access_checks_follow_each_restriction_and_its_polarity() {
    let mut c = test_controller();
    // Locked down, so no protected range is cleared before mutations.
    c.locked = true;
    let read = |c: &IchSpiController<FakeHost>, addr, len| {
        Controller::check_access(c, FlashAccess::Read, addr, len)
    };
    let modify = |c: &IchSpiController<FakeHost>, addr, len| {
        Controller::check_access(c, FlashAccess::Modify, addr, len)
    };
    // Protected range bits enable protection: read protection only
    // blocks reads.
    c.spibar
        .set32(PCH100_REG_GPR0, pr(0x8000, 0x8fff, true, false));
    assert_eq!(read(&c, 0x8fff, 1), Err(CoreError::RegionProtected));
    assert_eq!(read(&c, 0x7000, 0x1000), Ok(()));
    assert_eq!(modify(&c, 0x8000, 0x1000), Ok(()));
    c.spibar
        .set32(PCH100_REG_GPR0, pr(0x8000, 0x8fff, false, true));
    assert_eq!(modify(&c, 0x7fff, 2), Err(CoreError::RegionProtected));
    assert_eq!(read(&c, 0x8000, 0x1000), Ok(()));
    // A range whose limit is below its base protects nothing.
    c.spibar
        .set32(PCH100_REG_GPR0, pr(0x9000, 0x8000, true, true));
    assert_eq!(modify(&c, 0, 0x10000), Ok(()));

    // Descriptor permission bits grant access: region 1 (0x1000) may be
    // read but not written, region 2 (from 0x20000) written but not read.
    // They apply unless the override strap is set (FDOPSS reads 0).
    c.desc_valid = true;
    c.spibar.set32(ICH9_REG_FREG0 + 4, 0x0001_0001);
    c.spibar.set32(ICH9_REG_FREG0 + 8, 0x002f_0020);
    let (wap, rap) = (0b101, 0b011);
    c.spibar.set32(ICH9_REG_FRAP, wap << 8 | rap);
    c.spibar.set32(ICH9_REG_HSFS, u32::from(HSFS_FDOPSS));
    assert_eq!(modify(&c, 0x1000, 0x1000), Err(CoreError::RegionProtected));
    assert_eq!(read(&c, 0x1000, 0x1000), Ok(()));
    assert_eq!(read(&c, 0x20000, 1), Err(CoreError::RegionProtected));
    assert_eq!(modify(&c, 0x20000, 0x1000), Ok(()));
    assert_eq!(modify(&c, 0, 0x1000), Ok(()));
    // Newer chipsets grant access through BIOS_BM_RAP/WAP instead.
    c.generation = IchChipset::C740Emmitsburg;
    c.spibar.set32(BIOS_BM_WAP, wap);
    c.spibar.set32(BIOS_BM_RAP, rap);
    c.spibar.set32(ICH9_REG_FRAP, 0xffff);
    assert_eq!(modify(&c, 0x1000, 0x1000), Err(CoreError::RegionProtected));
    assert_eq!(read(&c, 0x20000, 1), Err(CoreError::RegionProtected));
    c.spibar.set32(ICH9_REG_HSFS, 0);
    assert_eq!(modify(&c, 0x1000, 0x1000), Ok(()));
    assert_eq!(read(&c, 0x20000, 1), Ok(()));

    // Software sequencing cannot reach below BBAR.
    c.bbar = 0x10000;
    assert_eq!(read(&c, 0xffff, 1), Ok(())); // hardware sequencing
    c.mode = SpiMode::SoftwareSequencing;
    assert_eq!(read(&c, 0xffff, 1), Err(CoreError::RegionProtected));
    assert_eq!(modify(&c, 0xf000, 0x1000), Err(CoreError::RegionProtected));
    assert_eq!(modify(&c, 0x10000, 0x1000), Ok(()));

    // ICH7 PBRs: 4 KiB pages, write protection only, never cleared.
    let mut c = test_controller();
    c.generation = IchChipset::Ich7;
    c.spibar
        .set32(ICH7_REG_PBR0 + 4, 0x21 | 0x22 << 12 | 1 << PR_WP_OFF);
    assert_eq!(modify(&c, 0x20fff, 1), Ok(()));
    assert_eq!(modify(&c, 0x22fff, 1), Err(CoreError::RegionProtected));
    assert_eq!(read(&c, 0x21000, 0x2000), Ok(()));
}

#[test]
fn unlocked_ranges_are_cleared_at_the_first_mutation_and_restored() {
    use FlashAccess::{Modify, Read};
    let mut c = test_controller();
    c.host.enable_write8_readback();
    c.host
        .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    let mmio = c.spibar.clone();
    mmio.set_flash(vec![0xff; 1 << 20]);
    mmio.set32(ICH9_REG_HSFS, u32::from(HSFS_FDV));
    let unlocked = pr(0x10000, 0x10fff, false, true);
    let locked = pr(0x20000, 0x20fff, false, true);
    let read_protected = pr(0x30000, 0x30fff, true, false);
    mmio.set32(PCH100_REG_FPR0, unlocked);
    mmio.set32(PCH100_REG_FPR0 + 4, locked);
    mmio.set32(PCH100_REG_FPR0 + 8, read_protected);
    mmio.set32(PCH100_REG_DLOCK, DLOCK_PR0_LOCKDN << 1);
    let check =
        |c: &IchSpiController<FakeHost>, access, addr| Controller::check_access(c, access, addr, 1);
    let ranges = PCH100_REG_FPR0..PCH100_REG_FPR0 + 6 * 4;
    c.wait_configuration_idle().unwrap();
    c.init().unwrap();
    // Opening, reading and probing leave the ranges alone...
    c.hwseq_read(0, &mut [0; 4]).unwrap();
    assert!(!c.probe_opcode(JEDEC_READ));
    assert!(mmio.writes().iter().all(|w| !ranges.contains(&w.0)));
    // ...but the checks know which ones the first mutation clears.
    // Read protection applies until then: reads come first.
    assert_eq!(check(&c, Modify, 0x10000), Ok(()));
    assert_eq!(check(&c, Modify, 0x30000), Ok(()));
    assert_eq!(check(&c, Read, 0x30000), Err(CoreError::RegionProtected));
    assert_eq!(check(&c, Modify, 0x20000), Err(CoreError::RegionProtected));

    c.hwseq_write(0x10000, &[0x5a]).unwrap();
    assert_eq!(mmio.flash()[0x10000], 0x5a);
    let cleared = |value| value & !(1 << PR_RP_OFF | 1 << PR_WP_OFF);
    assert_eq!(mmio.read32(PCH100_REG_FPR0), cleared(unlocked));
    assert_eq!(mmio.read32(PCH100_REG_FPR0 + 4), locked);
    assert_eq!(mmio.read32(PCH100_REG_FPR0 + 8), cleared(read_protected));
    assert_eq!(check(&c, Read, 0x30000), Ok(()));
    assert_eq!(check(&c, Modify, 0x20000), Err(CoreError::RegionProtected));
    drop(c);
    assert_eq!(mmio.read32(PCH100_REG_FPR0), unlocked);
    assert_eq!(mmio.read32(PCH100_REG_FPR0 + 4), locked);
    assert_eq!(mmio.read32(PCH100_REG_FPR0 + 8), read_protected);
}

/// A controller over all-zero flash, with BIOS writes enableable and
/// FPR0 and FPR1 write protected but clearable.
fn clearable_ranges_controller() -> (IchSpiController<FakeHost>, [u32; 2]) {
    let mut c = test_controller();
    c.host.enable_write8_readback();
    c.host
        .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    c.spibar.set_flash(vec![0; 1 << 20]);
    let ranges = [
        pr(0x10000, 0x10fff, true, true),
        pr(0x20000, 0x20fff, false, true),
    ];
    c.spibar.set32(PCH100_REG_FPR0, ranges[0]);
    c.spibar.set32(PCH100_REG_FPR0 + 4, ranges[1]);
    (c, ranges)
}

/// `c` refused a write: the flash is unchanged, writes were never
/// enabled and both ranges are as they were.
fn assert_untouched(c: &IchSpiController<FakeHost>, ranges: [u32; 2]) {
    assert!(c.spibar.flash().iter().all(|b| *b == 0));
    assert!(c.host.writes().is_empty());
    assert_eq!(c.spibar.read32(PCH100_REG_FPR0), ranges[0]);
    assert_eq!(c.spibar.read32(PCH100_REG_FPR0 + 4), ranges[1]);
    assert_eq!(c.change_count, 0);
}

#[test]
fn ranges_locked_after_open_refuse_the_mutation() {
    let flockdn = (ICH9_REG_HSFS, u32::from(HSFS_FLOCKDN));
    let pr1_lockdn = (PCH100_REG_DLOCK, DLOCK_PR0_LOCKDN << 1);
    // Locked after the access checks assumed them cleared...
    for (reg, lock) in [flockdn, pr1_lockdn] {
        let (mut c, ranges) = clearable_ranges_controller();
        let check = Controller::check_access(&c, FlashAccess::Modify, 0x20000, 1);
        assert_eq!(check, Ok(()));
        c.spibar.set32(reg, lock);
        // ...even when this write does not overlap them.
        assert!(matches!(
            c.hwseq_write(0x30000, &[0x5a]),
            Err(InternalError::AccessDenied { .. })
        ));
        assert_untouched(&c, ranges);
    }

    // Locked while clearing: FPR0 is cleared, then FPR1 is locked.
    for (reg, lock) in [flockdn, pr1_lockdn] {
        let (mut c, ranges) = clearable_ranges_controller();
        c.spibar.on_write32(move |mmio, offset, _, value| {
            if offset == PCH100_REG_FPR0 {
                mmio.set32(reg, mmio.read32(reg) | lock);
            }
            value
        });
        assert!(matches!(
            c.hwseq_write(0x30000, &[0x5a]),
            Err(InternalError::AccessDenied { .. })
        ));
        if reg == PCH100_REG_DLOCK {
            assert_untouched(&c, ranges);
            continue;
        }
        // Locked down, FPR0 cannot be restored: it stays journaled.
        assert!(c.spibar.flash().iter().all(|b| *b == 0));
        assert!(c.host.writes().is_empty());
        assert_eq!(c.spibar.read32(PCH100_REG_FPR0 + 4), ranges[1]);
        assert_eq!(c.change_count, 1);
    }
}

#[test]
fn partially_taken_clears_are_undone_bit_by_bit() {
    let rp = 1 << PR_RP_OFF;
    let wp = 1 << PR_WP_OFF;
    // FPR0 keeps its read protection; clearing its write protection is
    // undone.
    let (mut c, ranges) = clearable_ranges_controller();
    c.spibar.on_write32(move |_, offset, old, value| {
        if offset == PCH100_REG_FPR0 {
            old & rp | value & !rp
        } else {
            value
        }
    });
    assert!(c.hwseq_write(0x30000, &[0x5a]).is_err());
    assert_untouched(&c, ranges);

    // FPR1 cannot be cleared and FPR0's write protection cannot be set
    // again: that restore is reported and stays journaled.
    let (mut c, ranges) = clearable_ranges_controller();
    c.spibar
        .on_write32(move |_, offset, old, value| match offset {
            PCH100_REG_FPR0 => value & (old | !wp),
            o if o == PCH100_REG_FPR0 + 4 => old,
            _ => value,
        });
    assert!(c.hwseq_write(0x30000, &[0x5a]).is_err());
    assert!(c.spibar.flash().iter().all(|b| *b == 0));
    assert!(c.host.writes().is_empty());
    assert_eq!(c.spibar.read32(PCH100_REG_FPR0), ranges[0] & !wp);
    assert_eq!(c.change_count, 1);
}

#[test]
fn failed_write_enable_restores_cleared_ranges_at_once() {
    let bwe = u32::from(BIOS_CNTL_BWE);
    for (silvermont, protected) in [(false, false), (false, true), (true, false), (true, true)] {
        let (mut c, ranges) = clearable_ranges_controller();
        c.silvermont_bcr = silvermont;
        let initial = if protected {
            u32::from(BIOS_CNTL_SMM_BWP)
        } else {
            0
        };
        // Neither BIOS_CNTL.BWE nor BCR.WPD takes, with or without SMM_BWP.
        c.host = FakeHost::default();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, initial);
        c.spibar.set32(SILVERMONT_REG_BCR, initial);
        c.spibar.block_write32(SILVERMONT_REG_BCR);
        assert!(c.hwseq_write(0x30000, &[0x5a]).is_err());
        // The controller is still open, and nothing is left changed.
        assert!(c.spibar.flash().iter().all(|b| *b == 0));
        assert_eq!(c.spibar.read32(PCH100_REG_FPR0), ranges[0]);
        assert_eq!(c.spibar.read32(PCH100_REG_FPR0 + 4), ranges[1]);
        assert_eq!(c.change_count, 0);
        assert!(!c.writes_enabled());
        let bios_cntl = c.host.read8(c.config_bdf, PCI_REG_BIOS_CNTL as u16);
        assert_eq!(u32::from(bios_cntl.unwrap()) & bwe, 0);
        assert_eq!(c.spibar.read32(SILVERMONT_REG_BCR) & bwe, 0);
    }
}

#[test]
fn a_failed_clear_refuses_the_mutation_and_undoes_the_others() {
    for silvermont in [false, true] {
        let mut c = test_controller();
        c.silvermont_bcr = silvermont;
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        let (mmio, host) = (c.spibar.clone(), c.host.clone());
        mmio.set_flash(vec![0; 1 << 20]);
        let first = pr(0x10000, 0x10fff, false, true);
        mmio.set32(PCH100_REG_FPR0, first);
        mmio.set32(PCH100_REG_FPR0 + 4, pr(0x20000, 0x20fff, false, true));
        c.spibar.block_write32(PCH100_REG_FPR0 + 4);
        assert_eq!(
            Controller::check_access(&c, FlashAccess::Modify, 0x20000, 1),
            Ok(())
        );
        assert!(matches!(
            c.hwseq_write(0x30000, &[0x5a]),
            Err(InternalError::AccessDenied { .. })
        ));
        // Nothing is left changed: not the flash, not the range cleared
        // first, and writes were never enabled.
        assert!(mmio.flash().iter().all(|b| *b == 0));
        assert_eq!(mmio.read32(PCH100_REG_FPR0), first);
        assert!(host.writes().is_empty());
        assert!(mmio.writes().iter().all(|w| w.0 != SILVERMONT_REG_BCR));
        assert_eq!(c.change_count, 0);
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
fn layout(regions: &[(u32, u32)]) -> rflasher_core::layout::Layout {
    use rflasher_core::layout::{Layout, Region};
    let mut layout = Layout::new();
    for (i, &(start, end)) in regions.iter().enumerate() {
        let mut region = Region::new(format!("r{i}"), start, end);
        region.included = true;
        layout.add_region(region);
    }
    layout
}

/// Erase `regions` of `flash`, or smart write them from `image`.
#[cfg(all(feature = "std", target_os = "linux"))]
fn mutate<D: rflasher_core::flash::FlashDevice>(
    flash: &mut D,
    regions: &[(u32, u32)],
    image: Option<&[u8]>,
) -> CoreResult<()> {
    use futures_lite::future::block_on;
    use rflasher_core::flash::{MutationPolicy, unified};
    let mut policy = MutationPolicy::default();
    match image {
        None => block_on(unified::erase_by_layout_with_policy(
            flash,
            &layout(regions),
            &mut policy,
        )),
        Some(image) => block_on(unified::smart_write_by_layout_with_policy(
            flash,
            &layout(regions),
            image,
            &mut unified::NoProgress,
            &mut policy,
        ))
        .map(|_| ()),
    }
}

/// Erase or smart write `regions` of all-zero flash (changing `changed`
/// to 0x5a) and check the result: refused before anything changed, or
/// done with every neighbor restored.
#[cfg(all(feature = "std", target_os = "linux"))]
fn check_mutation<D: rflasher_core::flash::FlashDevice>(
    flash: &mut D,
    (mmio, host): (&FakeMmio, &FakeHost),
    regions: &[(u32, u32)],
    changed: &[(u32, u32)],
    erase: bool,
    refused: bool,
) {
    let mut image = vec![0; flash.size() as usize];
    for &(start, end) in changed {
        image[start as usize..=end as usize].fill(0x5a);
    }
    let result = mutate(flash, regions, (!erase).then_some(&image[..]));
    let contents = mmio.flash();
    if refused {
        assert_eq!(result, Err(CoreError::RegionProtected), "{regions:x?}");
        assert!(contents.iter().all(|b| *b == 0));
        // BIOS writes were never even enabled.
        assert!(host.writes().is_empty());
        return;
    }
    result.unwrap();
    if erase {
        image.fill(0);
        for &(start, end) in regions {
            image[start as usize..=end as usize].fill(0xff);
        }
    }
    assert!(contents == image, "{regions:x?}");
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn hwseq_mutations_check_planned_footprints_before_the_first_change() {
    use crate::programmer::HwseqFlash;

    // 64 KiB hardware erase blocks; 0x2f000-0x2ffff is protected.
    let write_protected = pr(0x2f000, 0x2ffff, false, true);
    let read_protected = pr(0x2f000, 0x2ffff, true, false);
    let spill = &[(0, 0xffff), (0x21000, 0x21fff)][..];
    let unprotected_spill = &[(0, 0xffff), (0x31000, 0x31fff)][..];
    // (regions, changed by the write, protection, erase/write refused)
    for (regions, changed, protection, refused) in [
        // A later region's block reaches the protected range.
        (spill, spill, write_protected, [true, true]),
        // Its neighbors are read to save and verify them.
        (spill, spill, read_protected, [true, true]),
        (
            unprotected_spill,
            unprotected_spill,
            write_protected,
            [false, false],
        ),
        // Unchanged protected data in a region is only erased by an erase.
        (
            &[(0x20000, 0x3ffff)][..],
            &[(0x30000, 0x3ffff)][..],
            write_protected,
            [true, false],
        ),
    ] {
        for (erase, refused) in [true, false].into_iter().zip(refused) {
            // Unless locked down, write protection is cleared first.
            for locked in [true, false] {
                let mut controller = ich9_controller(3, 3, 0);
                controller.locked = locked;
                let fake = (controller.spibar.clone(), controller.host.clone());
                fake.0.set_flash(vec![0; 1 << 20]);
                fake.0.set32(ICH9_REG_PR0, protection);
                let mut flash = HwseqFlash::with_size(controller, 1 << 20).unwrap();
                check_mutation(
                    &mut flash,
                    (&fake.0, &fake.1),
                    regions,
                    changed,
                    erase,
                    refused && (locked || protection == read_protected),
                );
                drop(flash);
                assert_eq!(fake.0.read32(ICH9_REG_PR0), protection);
            }
        }
    }
}

/// Opcode menu of the software-sequencing tests: the defaults, with a
/// 64 KiB block erase.
fn swseq_menu() -> Opcodes {
    let mut opcodes = Opcodes::default();
    opcodes.opcode[2].opcode = JEDEC_BE_D8;
    opcodes
}

/// A PCH100 controller in software sequencing over `flash`, with BIOS
/// writes enableable.
fn swseq_controller(flash: Vec<u8>) -> IchSpiController<FakeHost> {
    let mut c = test_controller();
    c.mode = SpiMode::SoftwareSequencing;
    c.host.enable_write8_readback();
    c.host
        .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
    let opcodes = swseq_menu();
    c.spibar.set_flash(flash);
    c.spibar.set_swseq_menu(
        PCH100_REG_SSFSC,
        core::array::from_fn(|i| opcodes.opcode[i].opcode),
    );
    c.opcodes = Some(opcodes);
    c
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn firmware_controller_uses_core_geometry_and_restores_bios_write_enable() {
    use futures_lite::future::block_on;
    use rflasher_core::flash::{FlashContext, FlashDevice, SpiFlashDevice};

    let db = rflasher_chips::ChipDatabase::new();
    let chip = db.find_by_name("SST25VF016B")[0].clone();
    let mut controller = swseq_controller(vec![0; chip.total_size as usize]);
    controller.locked = true;
    // Only 4-KiB sector erase is in this menu; the chip also advertises
    // 32/64-KiB erasers, so core must select supported geometry itself.
    controller.opcodes.as_mut().unwrap().opcode[2].opcode = JEDEC_SE;
    controller.spibar.set_swseq_menu(
        controller.swseq.ssfsc,
        controller
            .opcodes
            .as_ref()
            .unwrap()
            .opcode
            .map(|o| o.opcode),
    );
    let mmio = controller.spibar.clone();
    let host = controller.host.clone();
    let bdf = controller.config_bdf;
    let protected_register = controller.pr_registers().0;
    mmio.set32(protected_register, pr(0x18000, 0x18fff, false, true));
    // The concrete firmware controller is a SpiMaster: no Box or another
    // chip protocol implementation between it and the core.
    let mut flash = SpiFlashDevice::new(controller, FlashContext::new(chip));
    assert_eq!(
        block_on(flash.erase(0x10000, 0x10000)),
        Err(CoreError::RegionProtected)
    );
    assert!(mmio.writes().is_empty());
    assert!(host.writes().is_empty());

    mmio.set32(protected_register, 0);
    block_on(flash.erase(0x10000, 0x10000)).unwrap();
    block_on(flash.write(0x10000, &[1, 2, 3, 4])).unwrap();
    let mut read = [0; 4];
    block_on(flash.read(0x10000, &mut read)).unwrap();
    assert_eq!(read, [1, 2, 3, 4]);
    let contents = mmio.flash();
    assert!(contents[..0x10000].iter().all(|&b| b == 0));
    assert!(contents[0x10004..0x20000].iter().all(|&b| b == 0xff));
    assert!(contents[0x20000..].iter().all(|&b| b == 0));
    drop(flash);
    assert_eq!(
        host.read8(bdf, PCI_REG_BIOS_CNTL as u16).unwrap() & BIOS_CNTL_BWE,
        0
    );
}

/// Run the fake's cycles with the menu the controller now has.
#[cfg(all(feature = "std", target_os = "linux"))]
fn follow_menu(c: &IchSpiController<FakeHost>) {
    let menu = c.opcodes.as_ref().unwrap().opcode.map(|o| o.opcode);
    c.spibar.set_swseq_menu(c.swseq.ssfsc, menu);
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn an_identified_chip_gets_its_status_reads_and_can_be_written() {
    use futures_lite::future::block_on;
    use rflasher_core::flash::{FlashContext, FlashDevice, SpiFlashDevice};

    let db = rflasher_chips::ChipDatabase::new();
    let chip = |name: &str| {
        let found = db.find_by_name(name);
        (*found.iter().find(|c| c.name() == name).unwrap()).clone()
    };
    // Parts whose unprotect procedure reads SR2 (and SR3 with WPS).
    for (name, sr3) in [
        ("W25Q32.V", false),
        ("W25Q64.V", false),
        ("W25Q128.V", false),
        ("W25Q128FVJV-WP", true),
    ] {
        let chip = chip(name);
        let controller = swseq_controller(vec![0; chip.total_size as usize]);
        assert!(
            !controller.probe_opcode(0x35),
            "{name}: before identification"
        );
        let mmio = controller.spibar.clone();
        let menu_before = [
            mmio.read32(controller.swseq.opmenu),
            mmio.read32(controller.swseq.opmenu + 4),
        ];
        let opmenu = controller.swseq.opmenu;
        let mut flash = SpiFlashDevice::new(controller, FlashContext::new(chip));
        let c = flash.master();
        assert!(c.probe_opcode(0x35), "{name}");
        assert_eq!(c.probe_opcode(0x15), sr3, "{name}");
        // Identification opcodes made room; programming ones stayed.
        assert!(!c.probe_opcode(JEDEC_REMS), "{name}");
        for kept in [JEDEC_RDID, JEDEC_RDSR, JEDEC_WRSR, JEDEC_BYTE_PROGRAM] {
            assert!(c.probe_opcode(kept), "{name}: {kept:#04x}");
        }
        follow_menu(c);
        flash.check_writable().unwrap();
        block_on(flash.erase(0x10000, 0x10000)).unwrap();
        block_on(flash.write(0x10000, &[1, 2, 3, 4])).unwrap();
        assert_eq!(mmio.flash()[0x10000..0x10004], [1, 2, 3, 4], "{name}");
        // The menu is restored with the rest of the configuration.
        drop(flash);
        assert_eq!(
            [mmio.read32(opmenu), mmio.read32(opmenu + 4)],
            menu_before,
            "{name}"
        );
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn a_locked_menu_and_other_chips_do_not_gain_rdsr2() {
    use futures_lite::future::block_on;
    use rflasher_core::flash::{FlashContext, FlashDevice, SpiFlashDevice};

    let db = rflasher_chips::ChipDatabase::new();
    let winbond = db.find_by_name("W25Q64.V")[0].clone();
    let macronix = db.find_by_name("MX25L6405D")[0].clone();
    let with_rdsr2 = |c: &mut IchSpiController<FakeHost>| {
        c.opcodes.as_mut().unwrap().opcode[4] = Opcode {
            opcode: 0x35,
            spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            atomic: 0,
        };
        follow_menu(c);
    };

    // Locked without RDSR2: the menu stays as it is and CMP cannot be
    // checked, so the chip is written as found and read back.
    let mut c = swseq_controller(vec![0xff; winbond.total_size as usize]);
    c.locked = true;
    let mmio = c.spibar.clone();
    let menu = c.opcodes.as_ref().unwrap().opcode.map(|o| o.opcode);
    let mut flash = SpiFlashDevice::new(c, FlashContext::new(winbond.clone()));
    assert!(mmio.writes().is_empty());
    assert!(!flash.master().probe_opcode(0x35));
    flash.check_writable().unwrap();
    block_on(flash.write(0x10000, &[1, 2])).unwrap();
    assert_eq!(mmio.flash()[0x10000..0x10002], [1, 2]);
    let c = flash.master();
    assert_eq!(c.opcodes.as_ref().unwrap().opcode.map(|o| o.opcode), menu);

    // Locked with the firmware's own RDSR2 entry: usable for this chip.
    let mut c = swseq_controller(vec![0; winbond.total_size as usize]);
    c.locked = true;
    with_rdsr2(&mut c);
    let mut flash = SpiFlashDevice::new(c, FlashContext::new(winbond));
    flash.check_writable().unwrap();
    block_on(flash.write(0x10000, &[1])).unwrap();

    // The same entry stays refused for a part where 0x35 enters QPI,
    // which needs no status-register-2 read to be written.
    let mut c = swseq_controller(vec![0; macronix.total_size as usize]);
    with_rdsr2(&mut c);
    let mmio = c.spibar.clone();
    let writes = mmio.writes().len();
    let mut flash = SpiFlashDevice::new(c, FlashContext::new(macronix));
    assert_eq!(mmio.writes().len(), writes);
    assert!(!flash.master().probe_opcode(0x35));
    flash.check_writable().unwrap();
    block_on(flash.write(0x10000, &[1])).unwrap();
}

#[cfg(all(feature = "std", target_os = "linux"))]
#[test]
fn swseq_mutations_check_planned_footprints_before_the_first_change() {
    use crate::programmer::InternalSpi;
    use rflasher_core::flash::{FlashContext, SpiFlashDevice};

    let db = rflasher_chips::ChipDatabase::new();
    let mut chip = db.find_by_name("W25Q128.V")[0].clone();
    // 64 KiB blocks only, the erase in the menu.
    chip.erase_blocks
        .to_mut()
        .retain(|b| b.opcode == JEDEC_BE_D8);
    // This is about footprints; leave the status registers alone.
    chip.unlock = rflasher_core::chip::Unlock::None;
    let size = chip.total_size as usize;
    // The end of the 64 KiB block at 0x40000 is protected: the hardware
    // would allow erasing it, since it only checks the erase address.
    let spill = &[(0x10000, 0x1ffff), (0x41000, 0x41fff)][..];
    let unprotected_spill = &[(0x10000, 0x1ffff), (0x51000, 0x51fff)][..];
    // Only a range locked in DLOCK is not cleared first.
    for (regions, refused) in [(spill, true), (unprotected_spill, false)] {
        for (erase, dlock) in [true, false].into_iter().zip([0, DLOCK_PR0_LOCKDN]) {
            let refused = refused && dlock != 0;
            let mut c = swseq_controller(vec![0; size]);
            c.spibar
                .set32(PCH100_REG_FPR0, pr(0x4f000, 0x4ffff, false, true));
            c.spibar.set32(PCH100_REG_DLOCK, dlock);
            c.dlock = dlock;
            let fake = (c.spibar.clone(), c.host.clone());
            let mut flash = SpiFlashDevice::new(
                InternalSpi {
                    controller: Box::new(c),
                },
                FlashContext::new(chip.clone()),
            );
            check_mutation(
                &mut flash,
                (&fake.0, &fake.1),
                regions,
                regions,
                erase,
                refused,
            );
        }
    }
}

#[test]
fn raw_swseq_commands_are_checked_like_the_hardware_checks_cycles() {
    for ich7 in [false, true] {
        let mut c = swseq_controller(vec![0; 1 << 20]);
        // Locked down, so the protected range stays.
        c.locked = true;
        if ich7 {
            c.generation = IchChipset::Ich7;
            c.spibar
                .set32(ICH7_REG_PBR0, 0x20 | 0x20 << 12 | 1 << PR_WP_OFF);
        } else {
            c.spibar
                .set32(PCH100_REG_FPR0, pr(0x20000, 0x20fff, false, true));
        }
        c.opcodes.as_mut().unwrap().opcode[4] = Opcode {
            opcode: 0xaa,
            spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            atomic: 0,
        };
        let host = c.host.clone();
        for mut cmd in [
            SpiCommand::write_3b(JEDEC_BYTE_PROGRAM, 0x1fff0, &[0; 0x20]),
            SpiCommand::erase_3b(JEDEC_BE_D8, 0x20000),
            // Could erase anything; the chip size is unknown here.
            SpiCommand::simple(JEDEC_CE_C7),
        ] {
            assert_eq!(c.execute(&mut cmd), Err(CoreError::RegionProtected));
        }
        // SAFETY: the fake controller has no physical flash.
        let unknown = unsafe {
            if ich7 {
                c.ich7_swseq_send_command_unchecked(&[0xaa], &mut [0])
            } else {
                c.swseq_send_command_unchecked(&[0xaa], &mut [0])
            }
        };
        assert!(matches!(unknown, Err(InternalError::AccessDenied { .. })));
        assert!(host.writes().is_empty());
        // Status register writes do not touch the array, and an erase is
        // judged by its address, as by the hardware. The chip geometry
        // is checked before planned erases instead.
        for mut cmd in [
            SpiCommand::write_reg(JEDEC_WRSR, &[0]),
            SpiCommand::erase_3b(JEDEC_BE_D8, 0x10000),
        ] {
            assert_ne!(c.execute(&mut cmd), Err(CoreError::RegionProtected));
        }
        assert_eq!(host.writes().len(), 1);
    }
}

#[test]
fn ich9_erase_follows_berase_instead_of_assuming_4k() {
    // BERASE = 3: every cycle erases 64 KiB.
    let mut controller = ich9_controller(3, 3, 0);
    assert_eq!(
        controller.hwseq_erase_geometry(16 << 20).unwrap(),
        64 * 1024
    );

    // A 4 KiB request would wipe 60 KiB of neighbouring data: refuse it,
    // and start no cycle.
    for (addr, len) in [
        (0x1000, 0x1000),
        (0, 0x1000),
        (0x10000, 0x8000),
        (0x8000, 0x10000),
    ] {
        assert!(
            controller.hwseq_erase(addr, len).is_err(),
            "erase({addr:#x}, {len:#x}) must be refused"
        );
    }
    assert!(controller.spibar.erase_cycles().is_empty());

    // Whole blocks are erased one cycle per block.
    controller.hwseq_erase(0x10000, 0x20000).unwrap();
    assert_eq!(controller.spibar.erase_cycles(), vec![0x10000, 0x20000]);
}

#[test]
fn ich9_4k_erase_walks_every_block() {
    let mut controller = ich9_controller(1, 1, 0);
    assert_eq!(controller.hwseq_erase_geometry(8 << 20).unwrap(), 4096);
    controller.hwseq_erase(0x2000, 0x3000).unwrap();
    assert_eq!(
        controller.spibar.erase_cycles(),
        vec![0x2000, 0x3000, 0x4000]
    );
}

#[test]
fn ich9_partitions_with_different_erase_sizes() {
    // FPB = 0x100 puts the boundary at 1 MiB: 4 KiB blocks below, 64 KiB above.
    let mut controller = ich9_controller(1, 3, 0x100);
    assert_eq!(controller.hwseq_erase_geometry(8 << 20).unwrap(), 64 * 1024);

    // Each partition erases in its own block size ...
    controller.hwseq_erase(0xF_F000, 0x1000).unwrap();
    controller.hwseq_erase(0x10_0000, 0x1_0000).unwrap();
    assert_eq!(controller.spibar.erase_cycles(), vec![0xF_F000, 0x10_0000]);
    // ... and a 4 KiB request in the 64 KiB partition is refused.
    assert!(controller.hwseq_erase(0x11_0000, 0x1000).is_err());

    // A boundary that does not fall on a 64 KiB block has no uniform plan.
    let controller = ich9_controller(1, 3, 0x101);
    assert!(controller.hwseq_erase_geometry(8 << 20).is_err());
}

#[test]
fn pch100_erase_geometry_is_4k_without_touching_registers() {
    let controller = test_controller();
    assert_eq!(controller.hwseq_erase_geometry(16 << 20).unwrap(), 4096);
    assert_eq!(controller.hwseq_erase_block_size(0x12345), 4096);
}

#[test]
fn hwseq_erase_rejects_partial_blocks_before_any_cycle() {
    let mut controller = test_controller();
    // PCH100+: 4 KiB blocks. None of these is a sequence of whole blocks.
    for (addr, len) in [(1, 4096), (0, 4095), (4096, 100), (2048, 4096)] {
        assert!(
            matches!(controller.hwseq_erase(addr, len), Err(InternalError::Io(_))),
            "erase({addr:#x}, {len:#x}) must be refused"
        );
    }
}

#[test]
fn enable_bios_write_only_reports_verified_enable() {
    let mut controller = test_controller();
    let bdf = PciAddress::new(0, 0, 0x1f, 0);
    assert!(!controller.writes_enabled());

    // The fake host accepts writes without changing the PCI register.
    controller
        .host
        .set_config32(bdf, PCI_REG_BIOS_CNTL as u16, 0);
    assert!(matches!(
        controller.enable_bios_write(),
        Err(InternalError::ChipsetEnable(_))
    ));
    assert!(!controller.writes_enabled());

    controller
        .host
        .set_config32(bdf, PCI_REG_BIOS_CNTL as u16, BIOS_CNTL_BWE as u32);
    controller.enable_bios_write().unwrap();
    assert!(controller.writes_enabled());

    // SMM_BWP/EISS is cleared alongside BWE; only BWE decides success.
    let bios_cntl = BIOS_CNTL_SMM_BWP | BIOS_CNTL_BLE;
    controller
        .host
        .set_config32(bdf, PCI_REG_BIOS_CNTL as u16, bios_cntl as u32);
    assert!(matches!(
        controller.enable_bios_write(),
        Err(InternalError::ChipsetEnable(_))
    ));
    assert!(!controller.writes_enabled());
    let wanted = (BIOS_CNTL_BLE | BIOS_CNTL_BWE) as u32;
    assert_eq!(
        controller.host.writes().last(),
        Some(&(bdf, PCI_REG_BIOS_CNTL as u16, wanted, 1))
    );

    controller.host.set_config32(
        bdf,
        PCI_REG_BIOS_CNTL as u16,
        (bios_cntl | BIOS_CNTL_BWE) as u32,
    );
    controller.enable_bios_write().unwrap();
    assert!(controller.writes_enabled());
    let writes = controller.host.writes();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[2], (bdf, PCI_REG_BIOS_CNTL as u16, wanted, 1));
}
use crate::intel_pci::INTEL_CHIPSETS;

fn detected_with_generation(generation: IchChipset) -> DetectedChipset {
    let enable = INTEL_CHIPSETS
        .iter()
        .find(|chipset| chipset.chipset == generation && !is_visible_spi_id(chipset.device_id))
        .expect("Intel chipset table entry for generation");

    DetectedChipset {
        enable,
        domain: 0,
        bus: 0,
        device: 0x1f,
        function: 0,
        revision_id: 0,
    }
}

fn detected_id(id: u16) -> DetectedChipset {
    let enable = INTEL_CHIPSETS.iter().find(|e| e.device_id == id).unwrap();
    DetectedChipset {
        enable,
        domain: 3,
        bus: 7,
        device: 0x1f,
        function: 0,
        revision_id: 0,
    }
}

#[test]
fn pch_spi_mapping_and_identity() {
    for (id, target, spi_id) in [
        (0x9d48, (0x1f, 5), 0x9d24),
        (0xa146, (0x1f, 5), 0xa124),
        (0xa155, (0x1f, 5), 0xa124),
        (0xa2c5, (0x1f, 5), 0xa2a4),
        (0xa2cc, (0x1f, 5), 0xa2a4),
        (0xa1c0, (0x1f, 5), 0xa1a4),
        (0xa1cd, (0x1f, 5), 0xa1a4),
        (0xa240, (0x1f, 5), 0xa224),
        (0xa249, (0x1f, 5), 0xa224),
        (0xa3c8, (0x1f, 5), 0xa3a4),
        (0x5ae8, (0x0d, 2), 0x5a96),
        (0x31e8, (0x0d, 2), 0x3196),
    ] {
        let chipset = detected_id(id);
        let bdf = PciAddress::new(3, 7, target.0, target.1);
        let host = FakeHost::default();
        host.set_config32(bdf, 0, (spi_id << 16) | 0x8086);
        host.set_config32(bdf, 0x10, 0xfed1_c000);
        assert_eq!(intel_config_bdf(&chipset).unwrap(), bdf);
        assert_eq!(
            get_spibar_address_with_host(&host, &chipset).unwrap(),
            0xfed1_c000
        );
        host.set_config32(bdf, 0, 0x4b23_8086);
        assert!(get_spibar_address_with_host(&host, &chipset).is_err());
        assert!(host.writes().is_empty());
    }
    for id in [
        0xa124, 0xa1a4, 0xa224, 0xa324, 0x43a4, 0x4b24, 0x5a96, 0x3196, 0x7e23, 0xa823, 0xae23,
    ] {
        let mut chipset = detected_id(id);
        chipset.device = 12;
        chipset.function = 3;
        let bdf = PciAddress::new(3, 7, 12, 3);
        let host = FakeHost::default();
        host.set_config32(bdf, 0, (u32::from(id) << 16) | 0x8086);
        host.set_config32(bdf, 0x10, 0xfed1_c000);
        assert_eq!(intel_config_bdf(&chipset).unwrap(), bdf);
        assert_eq!(
            get_spibar_address_with_host(&host, &chipset).unwrap(),
            0xfed1_c000
        );
    }
    assert!(crate::intel_pci::find_chipset(0x8086, 0x4b23, None).is_none());
    for id in [0xa303, 0x4380] {
        assert!(intel_config_bdf(&detected_id(id)).is_err());
    }
}

#[test]
fn missing_spi_refuses_before_writes() {
    let host = FakeHost::default();
    let chipset = detected_id(0x5ae8);
    assert!(matches!(
        get_spibar_address_with_host(&host, &chipset),
        Err(InternalError::ChipsetEnable(_))
    ));
    assert!(host.writes().is_empty());
    let mut denied = host.clone();
    denied.deny_reads();
    assert!(matches!(
        get_spibar_address_with_host(&denied, &chipset),
        Err(InternalError::ChipsetEnable(_))
    ));
    assert!(denied.writes().is_empty());
}

#[test]
fn pch_bios_control_uses_and_restores_spi_bdf() {
    let mut host = FakeHost::default();
    host.enable_write8_readback();
    let chipset = detected_id(0x5ae8);
    let spi = PciAddress::new(3, 7, 0x0d, 2);
    host.set_config32(spi, 0, 0x5a96_8086);
    host.set_config32(spi, 0x10, 0xfed1_c000);
    host.set_config32(spi, 0xdc, 0);
    assert_eq!(
        get_spibar_address_with_host(&host, &chipset).unwrap(),
        0xfed1_c000
    );
    let mut controller = test_controller();
    controller.host = host.clone();
    controller.config_bdf = intel_config_bdf(&chipset).unwrap();
    controller.enable_bios_write().unwrap();
    drop(controller);
    let writes = host.writes();
    assert_eq!(writes.len(), 2);
    assert!(writes.iter().all(|w| w.0 == spi && w.1 == 0xdc));
    assert_eq!(writes[0].2 & 1, 1);
    assert_eq!(writes[1].2 & 1, 0);

    host.set_config32(spi, 0xdc, 0);
    let mut denied = host.clone();
    denied.deny_writes();
    let mut controller = test_controller();
    controller.host = denied;
    controller.config_bdf = spi;
    assert!(controller.enable_bios_write().is_err());
    assert!(!controller.writes_enabled());
    drop(controller);
    assert_eq!(host.writes().len(), 2);
}

#[test]
fn test_spimode_parse_is_no_alloc_case_insensitive() {
    assert_eq!(SpiMode::parse("AUTO"), Some(SpiMode::Auto));
    assert_eq!(SpiMode::parse("HwSeq"), Some(SpiMode::HardwareSequencing));
    assert_eq!(
        SpiMode::parse("software"),
        Some(SpiMode::SoftwareSequencing)
    );
    assert_eq!(SpiMode::parse("invalid"), None);
}

#[test]
fn test_get_spibar_address_with_host_pch100_hidden_spi_function() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::Series100SunrisePoint);
    let spi_bdf = PciAddress::new(chipset.domain, chipset.bus, chipset.device, 5);
    host.set_config32(spi_bdf, 0, 0x9d24_8086);
    host.set_config32(spi_bdf, PCI_REG_SPIBAR as u16, 0xfed1_c000);

    let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

    assert_eq!(spibar, 0xfed1_c000);
}

#[test]
fn hidden_spi_function_accepts_masked_ids_only_for_hidden_mappings() {
    for (id, target) in [
        (0x9d4e, (0x1f, 5)),
        (0xa1c0, (0x1f, 5)),
        (0xa3c8, (0x1f, 5)),
        (0x5ae8, (0x0d, 2)),
        (0x31e8, (0x0d, 2)),
    ] {
        let chipset = detected_id(id);
        let function = intel_config_function(&chipset).unwrap();
        assert!(function.hidden);
        assert_eq!(function.address, PciAddress::new(3, 7, target.0, target.1));
        let host = FakeHost::default();
        host.set_config32(function.address, 0, u32::MAX);
        host.set_config32(function.address, PCI_REG_SPIBAR as u16, 0xfed1_c000);
        assert_eq!(
            get_spibar_address_with_host(&host, &chipset).unwrap(),
            0xfed1_c000
        );
        // An absent function reads all-ones everywhere and is rejected.
        host.set_config32(function.address, PCI_REG_SPIBAR as u16, u32::MAX);
        assert!(get_spibar_address_with_host(&host, &chipset).is_err());
    }

    let mut chipset = detected_id(0xa324);
    chipset.device = 0x1f;
    chipset.function = 5;
    let function = intel_config_function(&chipset).unwrap();
    assert!(!function.hidden);
    let host = FakeHost::default();
    host.set_config32(function.address, 0, u32::MAX);
    host.set_config32(function.address, PCI_REG_SPIBAR as u16, 0xfed1_c000);
    assert!(get_spibar_address_with_host(&host, &chipset).is_err());
}

#[test]
fn test_get_spibar_address_with_host_rejects_all_ones_spibar() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::Series100SunrisePoint);
    let spi_bdf = PciAddress::new(chipset.domain, chipset.bus, chipset.device, 5);
    host.set_config32(spi_bdf, 0, 0x9d24_8086);
    host.set_config32(spi_bdf, PCI_REG_SPIBAR as u16, u32::MAX);

    let err = get_spibar_address_with_host(&host, &chipset).unwrap_err();

    assert!(matches!(err, InternalError::ChipsetEnable(_)));
}

#[test]
fn pch_spi_bar_rejects_io_reserved_and_unassigned() {
    let host = FakeHost::default();
    let chipset = detected_id(0xa324);
    let bdf = PciAddress::new(3, 7, 0x1f, 0);
    host.set_config32(bdf, 0, 0xa324_8086);
    for bar in [
        0xfed1_c001,
        0xfed1_c002,
        0xfed1_c004,
        0,
        0xffff_f000,
        u32::MAX,
    ] {
        host.set_config32(bdf, 0x10, bar);
        assert!(
            get_spibar_address_with_host(&host, &chipset).is_err(),
            "BAR {bar:#x}"
        );
    }
    assert!(host.writes().is_empty());
}

#[test]
fn silvermont_sbase_discovery_and_errors() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::BayTrail);
    let bdf = PciAddress::new(
        chipset.domain,
        chipset.bus,
        chipset.device,
        chipset.function,
    );
    assert!(matches!(
        get_spibar_address_with_host(&host, &chipset),
        Err(InternalError::PciAccess(_))
    ));
    for value in [0, u32::MAX] {
        host.set_config32(bdf, 0x54, value);
        assert!(matches!(
            get_spibar_address_with_host(&host, &chipset),
            Err(InternalError::ChipsetEnable(_))
        ));
    }
    host.set_config32(bdf, 0x54, 0xfed0_13ff);
    assert_eq!(
        get_spibar_address_with_host(&host, &chipset).unwrap(),
        0xfed0_1200
    );
}

#[test]
fn silvermont_device_scope_includes_braswell() {
    for id in [0x0f1c, 0x1f38, 0x229c] {
        let enable = INTEL_CHIPSETS
            .iter()
            .find(|entry| entry.device_id == id)
            .unwrap();
        let chipset = DetectedChipset {
            enable,
            ..detected_with_generation(IchChipset::BayTrail)
        };
        assert!(uses_silvermont_sbase(&chipset));
        let host = FakeHost::default();
        let bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        host.set_config32(bdf, 0x54, 0xfed0_1002);
        assert_eq!(
            get_spibar_address_with_host(&host, &chipset).unwrap(),
            0xfed0_1000
        );
    }
}

#[test]
fn silvermont_bcr_enable_and_restore() {
    let mut controller = test_controller();
    controller.silvermont_bcr = true;
    let mmio = controller.spibar.clone();
    mmio.set32(0xfc, 0x1234_5604);
    controller.enable_bios_write().unwrap();
    controller.enable_bios_write().unwrap();
    assert_eq!(mmio.read32(0xfc), 0x1234_5605);
    // Other bits can change during the controller lifetime.
    mmio.set32(0xfc, 0x1234_5645);
    drop(controller);
    assert_eq!(mmio.read32(0xfc), 0x1234_5644);
    assert_eq!(
        mmio.writes(),
        vec![(0xfc, 0x1234_5605, 4), (0xfc, 0x1234_5644, 4)]
    );
}

#[test]
fn silvermont_bcr_existing_enable_and_access_failures() {
    let mut controller = test_controller();
    controller.silvermont_bcr = true;
    let mmio = controller.spibar.clone();
    mmio.set32(0xfc, 1);
    controller.enable_bios_write().unwrap();
    drop(controller);
    assert!(mmio.writes().is_empty());

    for sticky in [false, true] {
        for already_enabled in [false, true] {
            let mut controller = test_controller();
            controller.silvermont_bcr = true;
            let mmio = controller.spibar.clone();
            let smm_bwp = u32::from(BIOS_CNTL_SMM_BWP);
            let bwe = u32::from(BIOS_CNTL_BWE);
            let unrelated = 0x1234_5602;
            mmio.set32(0xfc, unrelated | smm_bwp | u32::from(already_enabled));
            if sticky {
                controller.spibar.on_write32(move |_, offset, _, value| {
                    if offset == 0xfc {
                        value | smm_bwp
                    } else {
                        value
                    }
                });
            }
            controller.enable_bios_write().unwrap();
            assert!(controller.writes_enabled());
            let protection = if sticky { smm_bwp } else { 0 };
            assert_eq!(mmio.read32(0xfc), unrelated | protection | bwe);
            assert_eq!(mmio.writes(), vec![(0xfc, unrelated | bwe, 4)]);
            drop(controller);
            assert_eq!(
                mmio.read32(0xfc),
                unrelated | protection | u32::from(already_enabled)
            );
        }
    }

    let mut controller = test_controller();
    controller.silvermont_bcr = true;
    controller.spibar.block_write32(0xfc);
    let mmio = controller.spibar.clone();
    assert!(matches!(
        controller.enable_bios_write(),
        Err(InternalError::ChipsetEnable(_))
    ));
    drop(controller);
    assert_eq!(mmio.writes(), vec![(0xfc, 1, 4), (0xfc, 0, 4)]);
}

#[test]
fn test_get_spibar_address_with_host_ich7_legacy_rcba() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::Ich7);
    let lpc_bdf = PciAddress::new(
        chipset.domain,
        chipset.bus,
        chipset.device,
        chipset.function,
    );
    host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_8001);

    let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

    assert_eq!(spibar, 0xfed1_8000 + RCBA_SPI_OFFSET_ICH7 as u64);
}

#[test]
fn test_get_spibar_address_with_host_ich8_uses_ich7_rcba_offset() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::Ich8);
    let lpc_bdf = PciAddress::new(
        chipset.domain,
        chipset.bus,
        chipset.device,
        chipset.function,
    );
    host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_c001);

    let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

    assert_eq!(spibar, 0xfed1_c000 + RCBA_SPI_OFFSET_ICH7 as u64);
}

#[test]
fn test_get_spibar_address_with_host_ich9_uses_ich9_rcba_offset() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::Ich9);
    let lpc_bdf = PciAddress::new(
        chipset.domain,
        chipset.bus,
        chipset.device,
        chipset.function,
    );
    host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_c001);

    let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

    assert_eq!(spibar, 0xfed1_c000 + RCBA_SPI_OFFSET_ICH9 as u64);
}

#[test]
fn test_get_spibar_address_with_host_rejects_disabled_rcba() {
    let host = FakeHost::default();
    let chipset = detected_with_generation(IchChipset::Ich7);
    let lpc_bdf = PciAddress::new(
        chipset.domain,
        chipset.bus,
        chipset.device,
        chipset.function,
    );
    host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_8000);

    let err = get_spibar_address_with_host(&host, &chipset).unwrap_err();

    assert!(matches!(
        err,
        InternalError::ChipsetEnable("RCBA not enabled")
    ));
}

#[test]
fn swseq_read_refuses_fast_read_fallback_without_read_opcode() {
    for ich7 in [false, true] {
        let mut c = test_controller();
        if ich7 {
            c.generation = IchChipset::Ich7;
        }
        c.mode = SpiMode::SoftwareSequencing;
        let mut opcodes = Opcodes::default();
        let read_idx = opcodes
            .opcode
            .iter()
            .position(|op| op.opcode == JEDEC_READ)
            .unwrap();
        opcodes.opcode[read_idx].opcode = JEDEC_FAST_READ;
        c.opcodes = Some(opcodes);

        assert!(
            !Controller::probe_opcode(&c, JEDEC_FAST_READ),
            "FAST_READ must not be advertised by swseq probe_opcode"
        );

        let mut buf = [0u8; 4];
        let res = if ich7 {
            c.ich7_swseq_read(0, &mut buf)
        } else {
            c.swseq_read(0, &mut buf)
        };
        assert!(matches!(res, Err(InternalError::NotSupported(_))));

        // Raw swseq dispatcher must also reject FAST_READ even when present in OPMENU.
        let writearr = [JEDEC_FAST_READ, 0x00, 0x00, 0x00];
        let raw_res = if ich7 {
            c.ich7_swseq_send_command(&writearr, &mut buf)
        } else {
            c.swseq_send_command(&writearr, &mut buf)
        };
        assert!(matches!(raw_res, Err(InternalError::NotSupported(_))));

        let mut cmd = SpiCommand::read_3b(JEDEC_FAST_READ, 0, &mut buf);
        assert_eq!(
            Controller::execute(&mut c, &mut cmd),
            Err(CoreError::OpcodeNotSupported)
        );
    }
}

#[test]
fn gpr0_ignores_reserved_dlock_bit_13() {
    let mut c = test_controller();
    // DLOCK bit 13 (`DLOCK_PR0_LOCKDN << 5`) is reserved; only FPR0..=FPR4 have DLOCK bits.
    let reserved_bit_13 = DLOCK_PR0_LOCKDN << 5;
    c.dlock = reserved_bit_13;
    c.spibar.set32(PCH100_REG_DLOCK, reserved_bit_13);
    c.locked = false;
    c.spibar.set32(ICH9_REG_HSFS, 0);

    // GPR0 is index 5: neither pr_clearable nor pr_locked_now should inspect reserved bit 13.
    assert!(c.pr_clearable(5));
    assert!(!c.pr_locked_now(5));

    // FPR4 (index 4) IS governed by bit 12 (`DLOCK_PR0_LOCKDN << 4`).
    c.dlock = DLOCK_PR0_LOCKDN << 4;
    c.spibar.set32(PCH100_REG_DLOCK, DLOCK_PR0_LOCKDN << 4);
    assert!(!c.pr_clearable(4));
    assert!(c.pr_locked_now(4));
}
