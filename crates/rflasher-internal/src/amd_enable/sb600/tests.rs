use super::*;
use crate::amd_pci::{AMD_CHIPSETS, find_chipset};
use crate::host::tests::FakeHost;
use std::sync::{Arc, Mutex};

const PROTECT0: u32 = 0xffc0_0000 | (63 << 2) | 1;
const STRAP_SPI: u32 = 0x0000_1b23;

#[derive(Default)]
struct DiscoveryHost {
    pci: FakeHost,
    acpi: Arc<Mutex<Vec<u8>>>,
    mmio_writes: Arc<Mutex<Vec<(usize, u8)>>>,
    maps: Mutex<Vec<(u64, usize)>>,
}

struct AcpiWindow {
    bytes: Arc<Mutex<Vec<u8>>>,
    writes: Arc<Mutex<Vec<(usize, u8)>>>,
}

impl MmioAccess for AcpiWindow {
    fn read8(&self, offset: usize) -> u8 {
        self.bytes.lock().unwrap()[offset]
    }
    fn read16(&self, offset: usize) -> u16 {
        let b = self.bytes.lock().unwrap();
        u16::from_le_bytes([b[offset], b[offset + 1]])
    }
    fn read32(&self, offset: usize) -> u32 {
        let b = self.bytes.lock().unwrap();
        u32::from_le_bytes(core::array::from_fn(|i| b[offset + i]))
    }
    fn write8(&self, offset: usize, value: u8) {
        self.writes.lock().unwrap().push((offset, value));
    }
    fn write16(&self, offset: usize, value: u16) {
        self.writes.lock().unwrap().push((offset, value as u8));
    }
    fn write32(&self, offset: usize, value: u32) {
        self.writes.lock().unwrap().push((offset, value as u8));
    }
}

impl PciConfigAccess for DiscoveryHost {
    type Error = InternalError;
    fn read8(&self, a: PciAddress, o: u16) -> Result<u8, InternalError> {
        self.pci.read8(a, o)
    }
    fn read16(&self, a: PciAddress, o: u16) -> Result<u16, InternalError> {
        self.pci.read16(a, o)
    }
    fn read32(&self, a: PciAddress, o: u16) -> Result<u32, InternalError> {
        self.pci.read32(a, o)
    }
    fn write8(&self, a: PciAddress, o: u16, v: u8) -> Result<(), InternalError> {
        self.pci.write8(a, o, v)
    }
    fn write16(&self, a: PciAddress, o: u16, v: u16) -> Result<(), InternalError> {
        self.pci.write16(a, o, v)
    }
    fn write32(&self, a: PciAddress, o: u16, v: u32) -> Result<(), InternalError> {
        self.pci.write32(a, o, v)
    }
}

impl HostAccess for DiscoveryHost {
    type MmioRegion = AcpiWindow;

    unsafe fn map_mmio(&self, phys_addr: u64, size: usize) -> Result<AcpiWindow, InternalError> {
        self.maps.lock().unwrap().push((phys_addr, size));
        Ok(AcpiWindow {
            bytes: self.acpi.clone(),
            writes: self.mmio_writes.clone(),
        })
    }

    fn delay_us(&self, _us: u32) {}
}

impl DiscoveryHost {
    fn set_acpi32(&self, offset: usize, value: u32) {
        self.acpi.lock().unwrap()[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_acpi8(&self, offset: usize, value: u8) {
        self.acpi.lock().unwrap()[offset] = value;
    }

    fn maps(&self) -> Vec<(u64, usize)> {
        self.maps.lock().unwrap().clone()
    }

    fn assert_read_only(&self) {
        assert!(self.pci.writes().is_empty(), "{:?}", self.pci.writes());
        assert!(self.mmio_writes.lock().unwrap().is_empty());
    }
}

fn lpc() -> PciAddress {
    PciAddress::new(0, 0, 0x14, AMD_LPC_FUNCTION)
}

fn smbus() -> PciAddress {
    PciAddress::new(0, 0, 0x14, 0)
}

fn sb9xx_enable() -> &'static AmdChipsetEnable {
    find_chipset(0x1002, 0x439d, 0).unwrap()
}

fn sb950() -> DiscoveryHost {
    let host = DiscoveryHost {
        acpi: Arc::new(Mutex::new(vec![0; ACPI_MMIO_SIZE])),
        ..DiscoveryHost::default()
    };
    host.pci.set_config32(lpc(), 0x00, 0x439d_1002);
    host.pci.set_config32(smbus(), 0x00, 0x4385_1002);
    host.pci.set_config32(smbus(), 0x08, 0x0c05_0042);
    host.pci.set_config32(lpc(), 0x40, 0x0000_0004);
    host.pci.set_config32(lpc(), 0x50, PROTECT0);
    host.pci.set_config32(lpc(), 0x54, 0);
    host.pci.set_config32(lpc(), 0x58, 0);
    host.pci.set_config32(lpc(), 0x5c, 0);
    host.pci.set_config32(lpc(), 0xa0, 0xfec1_0002);
    host.pci.set_config32(lpc(), 0xb8, 0);
    host.set_acpi32(PM_ACPI_MMIO_EN, 0xfed8_0001);
    host.set_acpi32(MISC_STRAP_STATUS, STRAP_SPI);
    host
}

fn sb7xx() -> DiscoveryHost {
    let host = sb950();
    host.pci.set_config32(smbus(), 0x08, 0x0c05_003d);
    host.pci.set_config32(smbus(), 0x80, 0);
    host.pci.set_config32(smbus(), 0xa4, 0);
    host.pci.set_config32(smbus(), 0xa8, 0);
    host
}

fn discover(host: &DiscoveryHost, board: &Sb600BoardConfig) -> Result<AmdSb600Info, InternalError> {
    discover_with(host, sb9xx_enable(), lpc(), board)
}

fn discover_with(
    host: &DiscoveryHost,
    enable: &'static AmdChipsetEnable,
    device: PciAddress,
    board: &Sb600BoardConfig,
) -> Result<AmdSb600Info, InternalError> {
    // SAFETY: DiscoveryHost performs no real physical accesses.
    unsafe { enable_amd_sb600_with_host(host, enable, device, board) }
}

#[track_caller]
fn assert_refused(host: &DiscoveryHost, board: &Sb600BoardConfig, expected: &str) {
    let error = discover(host, board).unwrap_err();
    assert!(format!("{error}").contains(expected), "{error}");
    host.assert_read_only();
}

#[test]
fn sb950_discovery_is_read_only_and_preserves_rom_protect() {
    let host = sb950();
    let info = discover(&host, &Sb600BoardConfig::default()).unwrap();
    assert_eq!(info.generation(), Sb600Generation::Sb89xx);
    assert_eq!(info.spibar_addr, 0xfec1_0000);
    assert_eq!(info.config.imc, Sb600Imc::StrappedOff);
    assert_eq!(info.config.rom_protect[0], RomProtect::from_raw(PROTECT0));
    assert_eq!(&info.config.rom_protect[1..], &[RomProtect::NONE; 3]);
    assert_eq!(host.maps(), vec![(SB800_ACPI_MMIO_DEFAULT, ACPI_MMIO_SIZE)]);
    host.assert_read_only();
}

#[test]
fn hardware_mismatch_and_untested_fchs_refused_before_mmio() {
    let mut host = sb950();
    host.pci.deny_reads();
    let spi100 = AMD_CHIPSETS
        .iter()
        .find(|e| e.chipset.uses_spi100())
        .unwrap();
    assert!(matches!(
        discover_with(&host, spi100, smbus(), &Sb600BoardConfig::default()),
        Err(InternalError::NotSupported(_))
    ));

    // Wrong SMBus device paired with LPC bridge.
    let host = sb950();
    host.pci.set_config32(smbus(), 0x00, 0x780b_1022);
    assert_refused(
        &host,
        &Sb600BoardConfig::default(),
        "SMBus device not found",
    );
    assert!(host.maps().is_empty());

    // Hudson, Bolton, Yangtze, Promontory detected and refused as untested.
    for (lpc_id, smbus_id, rev, entry) in [
        (
            0x780e_1022u32,
            0x780b_1022u32,
            0x14u32,
            (0x1022, 0x780e, 0x00),
        ),
        (0x780e_1022, 0x780b_1022, 0x16, (0x1022, 0x780e, 0x00)),
        (0x790e_1022, 0x790b_1022, 0x4a, (0x1022, 0x790b, 0x4a)),
        (0x790e_1022, 0x790b_1022, 0x4b, (0x1022, 0x790b, 0x4b)),
    ] {
        let host = sb950();
        host.pci.set_config32(lpc(), 0x00, lpc_id);
        host.pci.set_config32(smbus(), 0x00, smbus_id);
        host.pci.set_config32(smbus(), 0x08, 0x0c05_0000 | rev);
        let enable = find_chipset(entry.0, entry.1, entry.2).unwrap();
        let dev = if entry.1 == 0x790b { smbus() } else { lpc() };
        let err = discover_with(&host, enable, dev, &Sb600BoardConfig::default()).unwrap_err();
        assert!(format!("{err}").contains("untested"), "{err}");
        assert!(host.maps().is_empty());
    }
}

#[test]
fn disabled_spi_bar_and_rom_enable_refused() {
    let board = Sb600BoardConfig::default();
    for (bar, expected) in [
        (0x0000_0002, "SB600 SPI not in use"),
        (0xfec1_0000, "SB600 SPI not in use"),
        (0xffff_ffff, "all 0xff"),
    ] {
        let host = sb950();
        host.pci.set_config32(lpc(), 0xa0, bar);
        assert_refused(&host, &board, expected);
        assert!(host.maps().is_empty());
    }
}

#[test]
fn sb8xx_acpi_decode_iomux_and_rom_straps() {
    let board = Sb600BoardConfig::default();

    // Unmapped or relocated ACPI MMIO refused.
    for en in [0xffff_ffff, 0xfed9_0001, 0xfed8_0003] {
        let host = sb950();
        host.set_acpi32(PM_ACPI_MMIO_EN, en);
        assert_refused(&host, &board, "ACPI MMIO is not decoded");
    }

    // Any SPI pin muxed away from function 0 refused.
    for (offset, _) in IOMUX_SPI_PINS {
        let host = sb950();
        host.set_acpi8(offset, 1);
        assert_refused(&host, &board, "SPI pins are used as GPIOs");
    }

    // Non-SPI StrapStatus refused, unless PM_Reg C8h UseBypassRom selects SPI (0b11).
    let host = sb950();
    host.set_acpi32(MISC_STRAP_STATUS, STRAP_SPI & !0b11);
    assert_refused(&host, &board, "not strapped to SPI ROM");
    host.set_acpi32(
        PM_MISC,
        PM_MISC_USE_BYPASS_ROM | (0b11 << PM_MISC_BYPASS_ROM_SEL_SHIFT),
    );
    discover(&host, &board).unwrap();

    // UseBypassRom selecting LPC overrides SPI StrapStatus and is refused.
    let host = sb950();
    host.set_acpi32(PM_MISC, PM_MISC_USE_BYPASS_ROM);
    assert_refused(&host, &board, "not strapped to SPI ROM");

    // Enabled StrapOverride selecting LPC overrides SPI StrapStatus (when UseBypassRom is clear).
    let host = sb950();
    host.set_acpi32(MISC_STRAP_OVERRIDE, STRAP_OVERRIDE_EN);
    assert_refused(&host, &board, "not strapped to SPI ROM");
    // UseBypassRom selecting SPI still takes precedence over an LPC StrapOverride.
    host.set_acpi32(
        PM_MISC,
        PM_MISC_USE_BYPASS_ROM | (0b11 << PM_MISC_BYPASS_ROM_SEL_SHIFT),
    );
    discover(&host, &board).unwrap();

    // Enabled StrapOverride selecting SPI overrides non-SPI StrapStatus.
    let host = sb950();
    host.set_acpi32(MISC_STRAP_STATUS, STRAP_SPI & !0b11);
    host.set_acpi32(MISC_STRAP_OVERRIDE, STRAP_OVERRIDE_EN | STRAP_ROM_SPI_MASK);
    discover(&host, &board).unwrap();

    // Upper/undocumented bits in StrapStatus must not spuriously reject SB8xx/SB9xx.
    let host = sb950();
    host.set_acpi32(MISC_STRAP_STATUS, STRAP_SPI | (1 << 20));
    discover(&host, &board).unwrap();
}

#[test]
fn imc_and_sb600_sb7xx_pin_checks() {
    let board = Sb600BoardConfig::default();

    // SB8xx: strapped IMC, strap-override IMC, or running IMC all refused.
    let host = sb950();
    host.set_acpi32(MISC_STRAP_STATUS, STRAP_SPI | STRAP_IMC_ENABLE);
    assert_refused(&host, &board, "IMC is enabled");

    let host = sb950();
    host.set_acpi32(
        MISC_STRAP_OVERRIDE,
        STRAP_OVERRIDE_EN | STRAP_ROM_SPI_MASK | STRAP_IMC_ENABLE,
    );
    assert_refused(&host, &board, "IMC is enabled");

    let host = sb950();
    host.pci
        .set_config32(lpc(), 0x40, u32::from(PCI_CONTROL_IMC_PRESENT));
    assert_refused(&host, &board, "IMC is enabled");

    // SB7xx: requires sb7xx_imc_strapped_off, still refuses running IMC, checks SMBus GPIO pins.
    let host = sb7xx();
    assert_refused(&host, &board, "IMC strap unknown");
    let asserted = Sb600BoardConfig {
        sb7xx_imc_strapped_off: true,
        ..board
    };
    let info = discover(&host, &asserted).unwrap();
    assert_eq!(info.generation(), Sb600Generation::Sb7xx);
    assert!(host.maps().is_empty());

    let host = sb7xx();
    host.pci
        .set_config32(lpc(), 0x40, u32::from(PCI_CONTROL_IMC_PRESENT));
    assert_refused(&host, &asserted, "IMC is enabled");

    // SPI_HOLD as GPIO is allowed; SPI_CS/CLK/DO/DI as GPIO are refused.
    let host = sb7xx();
    host.pci.set_config32(smbus(), 0x80, 0x4000_0000);
    discover(&host, &asserted).unwrap();
    for (offset, val) in [
        (0x80, 0x8000_0000u32),
        (0xa4, 0x4000_0000),
        (0xa8, 0x4000_0000),
        (0xa8, 0x8000_0000),
    ] {
        let host = sb7xx();
        host.pci.set_config32(smbus(), offset, val);
        assert_refused(&host, &asserted, "SPI pins are used as GPIOs");
    }

    // SB600 has no IMC regardless of LPC 0x40[7].
    let host = sb7xx();
    host.pci.set_config32(lpc(), 0x00, 0x438d_1002);
    host.pci
        .set_config32(lpc(), 0x40, u32::from(PCI_CONTROL_IMC_PRESENT));
    let sb600 = find_chipset(0x1002, 0x438d, 0).unwrap();
    let info = discover_with(&host, sb600, lpc(), &board).unwrap();
    assert_eq!(info.generation(), Sb600Generation::Sb6xx);
    assert_eq!(info.config.imc, Sb600Imc::NotPresent);
}
