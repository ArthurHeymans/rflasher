//! ATI/AMD Radeon GPU PCI device ID table
//!
//! Maps GPU PCI device IDs to their SPI interface type.
//! Ported from flashrom `ati_spi.c` (Luc Verhaegen, Jiajie Chen).

use crate::chipset::TestStatus;

/// ATI/AMD GPU SPI interface type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtiSpiType {
    /// R600 family (HD 2xxx/3xxx/4xxx) — BAR2, direct MMIO
    R600,
    /// RV730/RV740 — BAR2, slightly different clock divider
    Rv730,
    /// Evergreen (HD 5xxx: Cypress, Juniper, Redwood, Cedar) — BAR2
    Evergreen,
    /// Northern Island (HD 6xxx: Cayman, Barts, Turks, Caicos) — BAR2
    NorthernIsland,
    /// Southern Island (HD 7xxx: Lombok, Verde, Pitcairn) — BAR2
    SouthernIsland,
    /// Bonaire (GCN 1.1, first Sea Island) — BAR5, SMC indirect
    Bonaire,
    /// Hawaii (GCN 1.1) — BAR5, SMC indirect
    Hawaii,
    /// Iceland/Tonga/Fiji/Polaris (GCN 1.2+) — BAR5, SMC indirect
    Iceland,
}

impl AtiSpiType {
    /// Which PCI BAR contains the MMIO registers
    pub fn io_bar(self) -> u8 {
        match self {
            Self::R600
            | Self::Rv730
            | Self::Evergreen
            | Self::NorthernIsland
            | Self::SouthernIsland => 0x18, // PCI_BASE_ADDRESS_2
            Self::Bonaire | Self::Hawaii | Self::Iceland => 0x24, // PCI_BASE_ADDRESS_5
        }
    }

    /// Whether this type uses the CI (Sea Island+) register interface
    pub fn is_ci(self) -> bool {
        matches!(self, Self::Bonaire | Self::Hawaii | Self::Iceland)
    }

    /// Human-readable family name
    pub fn family_name(self) -> &'static str {
        match self {
            Self::R600 => "R600",
            Self::Rv730 => "RV730",
            Self::Evergreen => "Evergreen",
            Self::NorthernIsland => "Northern Island",
            Self::SouthernIsland => "Southern Island",
            Self::Bonaire => "Bonaire",
            Self::Hawaii => "Hawaii",
            Self::Iceland => "Iceland+",
        }
    }
}

/// ATI SPI PCI device entry
#[derive(Debug, Clone)]
pub struct AtiSpiDevice {
    pub vendor_id: u16,
    pub device_id: u16,
    pub spi_type: AtiSpiType,
    pub status: TestStatus,
    pub vendor_name: &'static str,
    pub device_name: &'static str,
}

/// ATI vendor ID
pub const ATI_VID: u16 = 0x1002;

/// Find an ATI SPI device by PCI vendor/device ID
pub fn find_ati_spi_device(vendor_id: u16, device_id: u16) -> Option<&'static AtiSpiDevice> {
    ATI_SPI_DEVICES
        .iter()
        .find(|d| d.vendor_id == vendor_id && d.device_id == device_id)
}

// Macro to reduce boilerplate in the device table
macro_rules! ati_dev {
    ($did:expr, $typ:expr, $status:expr, $name:expr) => {
        AtiSpiDevice {
            vendor_id: ATI_VID,
            device_id: $did,
            spi_type: $typ,
            status: $status,
            vendor_name: "AMD",
            device_name: $name,
        }
    };
}

use AtiSpiType::*;
use TestStatus::{Ok as TOk, Untested as NT};

/// Complete PCI device ID table for ATI/AMD GPU SPI flash access.
///
/// Covers R600 through Polaris (HD 2xxx → RX 5xx).
pub static ATI_SPI_DEVICES: &[AtiSpiDevice] = &[
    // === Bonaire (GCN 1.1 / Sea Islands) ===
    ati_dev!(0x6640, Bonaire, NT, "Saturn XT [FirePro M6100]"),
    ati_dev!(0x6641, Bonaire, NT, "Saturn PRO [Radeon HD 8930M]"),
    ati_dev!(0x6646, Bonaire, NT, "Bonaire XT [Radeon R9 M280X]"),
    ati_dev!(0x6647, Bonaire, NT, "Saturn PRO/XT [Radeon R9 M270X/M280X]"),
    ati_dev!(0x6649, Bonaire, NT, "Bonaire [FirePro W5100]"),
    ati_dev!(0x6650, Bonaire, NT, "Bonaire"),
    ati_dev!(0x6651, Bonaire, NT, "Bonaire"),
    ati_dev!(0x6658, Bonaire, NT, "Bonaire XTX [Radeon R7 260X/360]"),
    ati_dev!(0x665C, Bonaire, NT, "Bonaire XT [Radeon HD 7790/8770]"),
    ati_dev!(0x665D, Bonaire, NT, "Bonaire [Radeon R7 200 Series]"),
    ati_dev!(0x665F, Bonaire, TOk, "Tobago PRO [Radeon R7 360]"),
    // === Northern Island (HD 6xxx) ===
    ati_dev!(0x6704, NorthernIsland, NT, "Cayman PRO GL [FirePro V7900]"),
    ati_dev!(0x6707, NorthernIsland, NT, "Cayman LE GL [FirePro V5900]"),
    ati_dev!(0x6718, NorthernIsland, NT, "Cayman XT [Radeon HD 6970]"),
    ati_dev!(0x6719, NorthernIsland, NT, "Cayman PRO [Radeon HD 6950]"),
    ati_dev!(0x671C, NorthernIsland, NT, "Antilles [Radeon HD 6990]"),
    ati_dev!(0x671D, NorthernIsland, NT, "Antilles [Radeon HD 6990]"),
    ati_dev!(0x671F, NorthernIsland, NT, "Cayman CE [Radeon HD 6930]"),
    ati_dev!(
        0x6720,
        NorthernIsland,
        NT,
        "Blackcomb [Radeon HD 6970M/6990M]"
    ),
    ati_dev!(0x6738, NorthernIsland, NT, "Barts XT [Radeon HD 6870]"),
    ati_dev!(0x6739, NorthernIsland, NT, "Barts PRO [Radeon HD 6850]"),
    ati_dev!(0x673E, NorthernIsland, NT, "Barts LE [Radeon HD 6790]"),
    ati_dev!(
        0x6740,
        NorthernIsland,
        NT,
        "Whistler [Radeon HD 6730M/6770M]"
    ),
    ati_dev!(
        0x6741,
        NorthernIsland,
        NT,
        "Whistler [Radeon HD 6630M/6650M]"
    ),
    ati_dev!(0x6742, NorthernIsland, NT, "Whistler LE [Radeon HD 6610M]"),
    ati_dev!(0x6743, NorthernIsland, NT, "Whistler [Radeon E6760]"),
    ati_dev!(0x6749, NorthernIsland, NT, "Turks GL [FirePro V4900]"),
    ati_dev!(0x674A, NorthernIsland, NT, "Turks GL [FirePro V3900]"),
    ati_dev!(0x6750, NorthernIsland, NT, "Onega [Radeon HD 6650A/7650A]"),
    ati_dev!(0x6751, NorthernIsland, NT, "Turks [Radeon HD 7650A/7670A]"),
    ati_dev!(0x6758, NorthernIsland, NT, "Turks XT [Radeon HD 6670/7670]"),
    ati_dev!(
        0x6759,
        NorthernIsland,
        NT,
        "Turks PRO [Radeon HD 6570/7570]"
    ),
    ati_dev!(0x675b, NorthernIsland, NT, "Turks [Radeon HD 7600 Series]"),
    ati_dev!(0x675d, NorthernIsland, NT, "Turks PRO [Radeon HD 7570]"),
    ati_dev!(0x675f, NorthernIsland, NT, "Turks LE [Radeon HD 5570/6510]"),
    ati_dev!(0x6760, NorthernIsland, NT, "Seymour [Radeon HD 6400M]"),
    ati_dev!(0x6761, NorthernIsland, NT, "Seymour LP [Radeon HD 6430M]"),
    ati_dev!(0x6763, NorthernIsland, NT, "Seymour [Radeon E6460]"),
    ati_dev!(0x6764, NorthernIsland, NT, "Seymour [Radeon HD 6400M]"),
    ati_dev!(0x6765, NorthernIsland, NT, "Seymour [Radeon HD 6400M]"),
    ati_dev!(0x6766, NorthernIsland, NT, "Caicos"),
    ati_dev!(0x6767, NorthernIsland, NT, "Caicos"),
    ati_dev!(0x6768, NorthernIsland, NT, "Caicos"),
    ati_dev!(0x6770, NorthernIsland, NT, "Caicos [Radeon HD 6450A/7450A]"),
    ati_dev!(0x6771, NorthernIsland, NT, "Caicos XTX [Radeon HD 8490]"),
    ati_dev!(0x6772, NorthernIsland, NT, "Caicos [Radeon HD 7450A]"),
    ati_dev!(
        0x6778,
        NorthernIsland,
        NT,
        "Caicos XT [Radeon HD 7470/8470]"
    ),
    ati_dev!(
        0x6779,
        NorthernIsland,
        NT,
        "Caicos [Radeon HD 6450/7450/8450]"
    ),
    ati_dev!(0x677B, NorthernIsland, NT, "Caicos PRO [Radeon HD 7450]"),
    // === Hawaii (GCN 1.1) ===
    ati_dev!(0x67A0, Hawaii, NT, "Hawaii XT GL [FirePro W9100]"),
    ati_dev!(0x67A1, Hawaii, NT, "Hawaii PRO GL [FirePro W8100]"),
    ati_dev!(0x67A2, Hawaii, NT, "Hawaii GL"),
    ati_dev!(0x67A8, Hawaii, NT, "Hawaii"),
    ati_dev!(0x67A9, Hawaii, NT, "Hawaii"),
    ati_dev!(0x67AA, Hawaii, NT, "Hawaii"),
    ati_dev!(0x67B0, Hawaii, NT, "Hawaii XT [Radeon R9 290X/390X]"),
    ati_dev!(0x67B1, Hawaii, NT, "Hawaii PRO [Radeon R9 290/390]"),
    ati_dev!(0x67B9, Hawaii, NT, "Vesuvius [Radeon R9 295X2]"),
    ati_dev!(0x67BE, Hawaii, NT, "Hawaii LE"),
    // === Iceland/Tonga/Fiji/Polaris (GCN 1.2+) ===
    ati_dev!(0x67C0, Iceland, NT, "Ellesmere [Radeon Pro WX 7100 Mobile]"),
    ati_dev!(0x67C2, Iceland, NT, "Ellesmere [Radeon Pro V7300X]"),
    ati_dev!(0x67C4, Iceland, NT, "Ellesmere [Radeon Pro WX 7100]"),
    ati_dev!(0x67C7, Iceland, NT, "Ellesmere [Radeon Pro WX 5100]"),
    ati_dev!(0x67CA, Iceland, NT, "Ellesmere [Polaris10]"),
    ati_dev!(0x67CC, Iceland, NT, "Ellesmere [Polaris10]"),
    ati_dev!(0x67CF, Iceland, NT, "Ellesmere [Polaris10]"),
    ati_dev!(0x67D0, Iceland, NT, "Ellesmere [Radeon Pro V7300X]"),
    ati_dev!(0x67DF, Iceland, NT, "Ellesmere [Radeon RX 470/480/570/580]"),
    ati_dev!(0x67E0, Iceland, NT, "Baffin [Radeon Pro WX 4170]"),
    ati_dev!(0x67E1, Iceland, NT, "Baffin [Polaris11]"),
    ati_dev!(0x67E3, Iceland, NT, "Baffin [Radeon Pro WX 4100]"),
    ati_dev!(0x67E8, Iceland, NT, "Baffin [Radeon Pro WX 4130/4150]"),
    ati_dev!(0x67E9, Iceland, NT, "Baffin [Polaris11]"),
    ati_dev!(0x67EB, Iceland, NT, "Baffin [Radeon Pro V5300X]"),
    ati_dev!(0x67EF, Iceland, NT, "Baffin [Radeon RX 460/560D]"),
    ati_dev!(0x67FF, Iceland, NT, "Baffin [Radeon RX 550/560X]"),
    // === Southern Island (HD 7xxx) — Lombok ===
    ati_dev!(0x6840, SouthernIsland, NT, "Thames [Radeon HD 7500M/7600M]"),
    ati_dev!(0x6841, SouthernIsland, NT, "Thames [Radeon HD 7550M/7570M]"),
    ati_dev!(0x6842, SouthernIsland, NT, "Thames LE [Radeon HD 7000M]"),
    ati_dev!(0x6843, SouthernIsland, NT, "Thames [Radeon HD 7670M]"),
    // === Evergreen (HD 5xxx) ===
    ati_dev!(0x6880, Evergreen, NT, "Lexington [Radeon HD 6550M]"),
    ati_dev!(0x6888, Evergreen, NT, "Cypress XT [FirePro V8800]"),
    ati_dev!(0x6889, Evergreen, NT, "Cypress PRO [FirePro V7800]"),
    ati_dev!(0x688A, Evergreen, NT, "Cypress XT [FirePro V9800]"),
    ati_dev!(0x688C, Evergreen, NT, "Cypress XT GL [FireStream 9370]"),
    ati_dev!(0x688D, Evergreen, NT, "Cypress PRO GL [FireStream 9350]"),
    ati_dev!(0x6898, Evergreen, NT, "Cypress XT [Radeon HD 5870]"),
    ati_dev!(0x6899, Evergreen, NT, "Cypress PRO [Radeon HD 5850]"),
    ati_dev!(0x689B, Evergreen, NT, "Cypress PRO [Radeon HD 6800]"),
    ati_dev!(0x689C, Evergreen, NT, "Hemlock [Radeon HD 5970]"),
    ati_dev!(0x689D, Evergreen, NT, "Hemlock [Radeon HD 5970]"),
    ati_dev!(0x689E, Evergreen, NT, "Cypress LE [Radeon HD 5830]"),
    ati_dev!(0x68A0, Evergreen, NT, "Broadway XT [Mobility HD 5870]"),
    ati_dev!(0x68A1, Evergreen, NT, "Broadway PRO [Mobility HD 5850]"),
    ati_dev!(0x68A8, Evergreen, NT, "Granville [Radeon HD 6850M/6870M]"),
    ati_dev!(0x68A9, Evergreen, NT, "Juniper XT [FirePro V5800]"),
    ati_dev!(0x68B8, Evergreen, NT, "Juniper XT [Radeon HD 5770]"),
    ati_dev!(0x68B9, Evergreen, NT, "Juniper LE [Radeon HD 5670]"),
    ati_dev!(0x68BA, Evergreen, NT, "Juniper XT [Radeon HD 6770]"),
    ati_dev!(0x68BE, Evergreen, NT, "Juniper PRO [Radeon HD 5750]"),
    ati_dev!(0x68BF, Evergreen, NT, "Juniper PRO [Radeon HD 6750]"),
    ati_dev!(0x68C0, Evergreen, NT, "Madison [Mobility HD 5730/6570M]"),
    ati_dev!(0x68C1, Evergreen, NT, "Madison [Mobility HD 5650/5750]"),
    ati_dev!(0x68C7, Evergreen, NT, "Pinewood [Mobility HD 5570/6550A]"),
    ati_dev!(0x68C8, Evergreen, NT, "Redwood XT GL [FirePro V4800]"),
    ati_dev!(0x68C9, Evergreen, NT, "Redwood PRO GL [FirePro V3800]"),
    ati_dev!(0x68D8, Evergreen, NT, "Redwood XT [Radeon HD 5670/5690]"),
    ati_dev!(0x68D9, Evergreen, NT, "Redwood PRO [Radeon HD 5550/5570]"),
    ati_dev!(0x68DA, Evergreen, NT, "Redwood LE [Radeon HD 5550/5570]"),
    ati_dev!(0x68DE, Evergreen, NT, "Redwood"),
    ati_dev!(0x68E0, Evergreen, NT, "Park [Mobility HD 5430/5450/5470]"),
    ati_dev!(0x68E1, Evergreen, NT, "Park [Mobility HD 5430]"),
    ati_dev!(0x68E4, Evergreen, NT, "Robson CE [Radeon HD 6370M/7370M]"),
    ati_dev!(0x68E5, Evergreen, NT, "Robson LE [Radeon HD 6330M]"),
    ati_dev!(0x68E8, Evergreen, NT, "Cedar"),
    ati_dev!(0x68E9, Evergreen, NT, "Cedar [FirePro Graphics Adapter]"),
    ati_dev!(0x68F1, Evergreen, NT, "Cedar GL [FirePro 2460]"),
    ati_dev!(0x68F2, Evergreen, NT, "Cedar GL [FirePro 2270]"),
    ati_dev!(0x68F8, Evergreen, NT, "Cedar [Radeon HD 7300 Series]"),
    ati_dev!(0x68F9, Evergreen, NT, "Cedar [Radeon HD 5000/6000/7350]"),
    ati_dev!(0x68FA, Evergreen, NT, "Cedar [Radeon HD 7350/8350]"),
    ati_dev!(0x68FE, Evergreen, NT, "Cedar LE"),
    // === Iceland/Tonga (GCN 1.2) ===
    ati_dev!(0x6900, Iceland, NT, "Topaz XT [Radeon R7 M260/M265]"),
    ati_dev!(0x6901, Iceland, NT, "Topaz PRO [Radeon R5 M255]"),
    ati_dev!(0x6907, Iceland, NT, "Meso XT [Radeon R5 M315]"),
    ati_dev!(0x6921, Iceland, NT, "Amethyst XT [Radeon R9 M295X]"),
    ati_dev!(0x6929, Iceland, NT, "Tonga XT GL [FirePro S7150]"),
    ati_dev!(0x692B, Iceland, NT, "Tonga PRO GL [FirePro W7100]"),
    ati_dev!(0x692F, Iceland, NT, "Tonga XTV GL [FirePro S7150V]"),
    ati_dev!(0x6938, Iceland, NT, "Tonga XT [Radeon R9 380X]"),
    ati_dev!(0x6939, Iceland, NT, "Tonga PRO [Radeon R9 285/380]"),
    ati_dev!(0x694C, Iceland, NT, "Polaris 22 XT [RX Vega M GH]"),
    ati_dev!(0x694E, Iceland, NT, "Polaris 22 XL [RX Vega M GL]"),
    ati_dev!(0x6980, Iceland, NT, "Polaris12"),
    ati_dev!(0x6981, Iceland, NT, "Lexa XT [Radeon PRO WX 3200]"),
    ati_dev!(0x6985, Iceland, NT, "Lexa XT [Radeon PRO WX 3100]"),
    ati_dev!(0x6986, Iceland, NT, "Polaris12"),
    ati_dev!(0x6987, Iceland, NT, "Lexa [Radeon 540X/550X/630]"),
    ati_dev!(0x6995, Iceland, NT, "Lexa XT [Radeon PRO WX 2100]"),
    ati_dev!(0x699F, Iceland, NT, "Lexa PRO [Radeon 540/540X/550/550X]"),
    ati_dev!(0x7300, Iceland, NT, "Fiji [Radeon R9 FURY / NANO]"),
    // === R600 (HD 2xxx) ===
    ati_dev!(0x9400, R600, NT, "R600 [Radeon HD 2900 PRO/XT]"),
    ati_dev!(0x9401, R600, NT, "R600 [Radeon HD 2900 XT]"),
    ati_dev!(0x9402, R600, NT, "R600"),
    ati_dev!(0x9403, R600, NT, "R600 [Radeon HD 2900 PRO]"),
    ati_dev!(0x9405, R600, NT, "R600 [Radeon HD 2900 GT]"),
    ati_dev!(0x940A, R600, NT, "R600 GL [FireGL V8650]"),
    ati_dev!(0x940B, R600, NT, "R600 GL [FireGL V8600]"),
    ati_dev!(0x940F, R600, NT, "R600 GL [FireGL V7600]"),
    // === RV770 (HD 4xxx) ===
    ati_dev!(0x9440, R600, NT, "RV770 [Radeon HD 4870]"),
    ati_dev!(0x9441, R600, NT, "R700 [Radeon HD 4870 X2]"),
    ati_dev!(0x9442, R600, NT, "RV770 [Radeon HD 4850]"),
    ati_dev!(0x9443, R600, NT, "R700 [Radeon HD 4850 X2]"),
    ati_dev!(0x9444, R600, NT, "RV770 GL [FirePro V8750]"),
    ati_dev!(0x9446, R600, NT, "RV770 GL [FirePro V7760]"),
    ati_dev!(0x944A, R600, NT, "RV770 [Mobility HD 4850]"),
    ati_dev!(0x944B, R600, NT, "RV770 [Mobility HD 4850 X2]"),
    ati_dev!(0x944C, R600, NT, "RV770 LE [Radeon HD 4830]"),
    ati_dev!(0x944e, R600, NT, "RV770 CE [Radeon HD 4710]"),
    ati_dev!(0x9450, R600, NT, "RV770 GL [FireStream 9270]"),
    ati_dev!(0x9452, R600, NT, "RV770 GL [FireStream 9250]"),
    ati_dev!(0x9456, R600, NT, "RV770 GL [FirePro V8700]"),
    ati_dev!(0x945A, R600, NT, "RV770 [Mobility HD 4870]"),
    ati_dev!(0x9460, R600, NT, "RV790 [Radeon HD 4890]"),
    ati_dev!(0x9462, R600, NT, "RV790 [Radeon HD 4860]"),
    ati_dev!(0x946A, R600, NT, "RV770 GL [FirePro M7750]"),
    // === RV730/RV740 ===
    ati_dev!(0x9480, Rv730, NT, "RV730 [Mobility HD 4650/5165]"),
    ati_dev!(0x9488, Rv730, NT, "RV730 [Mobility HD 4670]"),
    ati_dev!(0x9489, Rv730, NT, "RV730 GL [Mobility FireGL V5725]"),
    ati_dev!(0x9490, Rv730, NT, "RV730 XT [Radeon HD 4670]"),
    ati_dev!(0x9491, Rv730, NT, "RV730 [Radeon E4690]"),
    ati_dev!(0x9495, Rv730, NT, "RV730 [Radeon HD 4600 AGP]"),
    ati_dev!(0x9498, Rv730, NT, "RV730 PRO [Radeon HD 4650]"),
    ati_dev!(0x949C, Rv730, NT, "RV730 GL [FirePro V7750]"),
    ati_dev!(0x949E, Rv730, NT, "RV730 GL [FirePro V5700]"),
    ati_dev!(0x949F, Rv730, NT, "RV730 GL [FirePro V3750]"),
    ati_dev!(0x94A0, Rv730, NT, "RV740 [Mobility HD 4830]"),
    ati_dev!(0x94A1, Rv730, NT, "RV740 [Mobility HD 4860]"),
    ati_dev!(0x94A3, Rv730, NT, "RV740 GL [FirePro M7740]"),
    ati_dev!(0x94B3, Rv730, NT, "RV740 PRO [Radeon HD 4770]"),
    ati_dev!(0x94B4, Rv730, NT, "RV740 PRO [Radeon HD 4750]"),
    // === RV610 (HD 2400) ===
    ati_dev!(0x94C1, R600, NT, "RV610 [Radeon HD 2400 PRO/XT]"),
    ati_dev!(0x94C3, R600, TOk, "RV610 [Radeon HD 2400 PRO]"),
    ati_dev!(0x94C4, R600, NT, "RV610 LE [Radeon HD 2400 PRO AGP]"),
    ati_dev!(0x94C5, R600, NT, "RV610 [Radeon HD 2400 LE]"),
    ati_dev!(0x94C6, R600, NT, "R600"),
    ati_dev!(0x94C7, R600, NT, "RV610 [Radeon HD 2350]"),
    ati_dev!(0x94C8, R600, NT, "RV610 [Mobility HD 2400 XT]"),
    ati_dev!(0x94C9, R600, NT, "RV610 [Mobility HD 2400]"),
    ati_dev!(0x94CB, R600, NT, "RV610 [Radeon E2400]"),
    ati_dev!(0x94CC, R600, NT, "RV610 LE [Radeon HD 2400 PRO PCI]"),
    // === RV670 (HD 3xxx) ===
    ati_dev!(0x9500, R600, NT, "RV670 [Radeon HD 3850 X2]"),
    ati_dev!(0x9501, R600, NT, "RV670 [Radeon HD 3870]"),
    ati_dev!(0x9504, R600, NT, "RV670 [Mobility HD 3850]"),
    ati_dev!(0x9505, R600, NT, "RV670 [Radeon HD 3690/3850]"),
    ati_dev!(0x9506, R600, NT, "RV670 [Mobility HD 3850 X2]"),
    ati_dev!(0x9507, R600, NT, "RV670 [Radeon HD 3830]"),
    ati_dev!(0x9508, R600, NT, "RV670 [Mobility HD 3870]"),
    ati_dev!(0x9509, R600, NT, "RV670 [Mobility HD 3870 X2]"),
    ati_dev!(0x950F, R600, NT, "R680 [Radeon HD 3870 X2]"),
    ati_dev!(0x9511, R600, TOk, "RV670 GL [FireGL V7700]"),
    ati_dev!(0x9513, R600, NT, "RV670 [Radeon HD 3850 X2]"),
    ati_dev!(0x9515, R600, NT, "RV670 PRO [Radeon HD 3850 AGP]"),
    ati_dev!(0x9519, R600, NT, "RV670 GL [FireStream 9170]"),
    // === RV710 ===
    ati_dev!(0x9540, R600, NT, "RV710 [Radeon HD 4550]"),
    ati_dev!(0x954F, R600, NT, "RV710 [Radeon HD 4350/4550]"),
    ati_dev!(0x9552, R600, NT, "RV710 [Mobility HD 4330/4350]"),
    ati_dev!(0x9553, R600, NT, "RV710 [Mobility HD 4530/4570]"),
    ati_dev!(0x9555, R600, NT, "RV711 [Mobility HD 4350/4550]"),
    ati_dev!(0x9557, R600, NT, "RV711 GL [FirePro RG220]"),
    ati_dev!(0x955F, R600, NT, "RV710 [Mobility HD 4330]"),
    // === RV630 (HD 2600) ===
    ati_dev!(0x9580, R600, NT, "RV630 [Radeon HD 2600 PRO]"),
    ati_dev!(0x9581, R600, NT, "RV630 [Mobility HD 2600]"),
    ati_dev!(0x9583, R600, NT, "RV630 [Mobility HD 2600 XT/2700]"),
    ati_dev!(0x9586, R600, NT, "RV630 XT [Radeon HD 2600 XT AGP]"),
    ati_dev!(0x9587, R600, NT, "RV630 PRO [Radeon HD 2600 PRO AGP]"),
    ati_dev!(0x9588, R600, NT, "RV630 XT [Radeon HD 2600 XT]"),
    ati_dev!(0x9589, R600, TOk, "RV630 PRO [Radeon HD 2600 PRO]"),
    ati_dev!(0x958A, R600, NT, "RV630 [Radeon HD 2600 X2]"),
    ati_dev!(0x958B, R600, NT, "RV630 [Mobility HD 2600 XT]"),
    ati_dev!(0x958C, R600, NT, "RV630 GL [FireGL V5600]"),
    ati_dev!(0x958D, R600, TOk, "RV630 GL [FireGL V3600]"),
    ati_dev!(0x958E, R600, NT, "R600"),
    // === RV635 (HD 3600) ===
    ati_dev!(0x9591, R600, NT, "RV635 [Mobility HD 3650]"),
    ati_dev!(0x9593, R600, NT, "RV635 [Mobility HD 3670]"),
    ati_dev!(0x9595, R600, NT, "RV635 GL [Mobility FireGL V5700]"),
    ati_dev!(0x9596, R600, NT, "RV635 PRO [Radeon HD 3650 AGP]"),
    ati_dev!(0x9597, R600, NT, "RV635 PRO [Radeon HD 3650 AGP]"),
    ati_dev!(0x9598, R600, NT, "RV635 [Radeon HD 3650/3750/4570]"),
    ati_dev!(0x9599, R600, NT, "RV635 PRO [Radeon HD 3650 AGP]"),
    // === RV620 (HD 3400) ===
    ati_dev!(0x95C0, R600, NT, "RV620 PRO [Radeon HD 3470]"),
    ati_dev!(0x95C2, R600, NT, "RV620 [Mobility HD 3410/3430]"),
    ati_dev!(0x95C4, R600, NT, "RV620 [Mobility HD 3450/3470]"),
    ati_dev!(0x95C5, R600, NT, "RV620 LE [Radeon HD 3450]"),
    ati_dev!(0x95C6, R600, NT, "RV620 LE [Radeon HD 3450 AGP]"),
    ati_dev!(0x95C9, R600, NT, "RV620 LE [Radeon HD 3450 PCI]"),
    ati_dev!(0x95CC, R600, NT, "RV620 GL [FirePro V3700]"),
    ati_dev!(0x95CD, R600, NT, "RV620 GL [FirePro 2450]"),
    ati_dev!(0x95CF, R600, NT, "RV620 GL [FirePro 2260]"),
];
