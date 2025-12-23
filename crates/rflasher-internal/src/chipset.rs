//! Intel chipset definitions and PCI ID tables

use core::fmt;

/// Test status for a chipset or device
///
/// Mirrors flashprog's `enum test_state`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    /// Tested and working
    Ok,
    /// Not tested yet
    Untested,
    /// Known to not work
    Bad,
    /// Support depends on configuration (e.g., Intel Flash Descriptor)
    Depends,
    /// Not applicable
    NotApplicable,
}

impl TestStatus {
    /// Returns true if this status should generate a warning
    pub fn should_warn(&self) -> bool {
        matches!(self, Self::Untested)
    }

    /// Returns true if this status indicates the device won't work
    pub fn is_bad(&self) -> bool {
        matches!(self, Self::Bad)
    }

    /// Returns a user-friendly message for this status
    pub fn message(&self) -> Option<&'static str> {
        match self {
            Self::Untested => Some(
                "This chipset is UNTESTED. If you are using an up-to-date version \
                 and were (not) able to successfully access flash with it, \
                 please report your results.",
            ),
            Self::Bad => Some("ERROR: This chipset is not supported."),
            Self::Depends => Some(
                "Support for this chipset depends on configuration \
                 (e.g., Intel Flash Descriptor settings).",
            ),
            _ => None,
        }
    }
}

impl fmt::Display for TestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => write!(f, "OK"),
            Self::Untested => write!(f, "UNTESTED"),
            Self::Bad => write!(f, "BAD"),
            Self::Depends => write!(f, "DEP"),
            Self::NotApplicable => write!(f, "N/A"),
        }
    }
}

/// Bus types supported by a chipset
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusType(u8);

impl BusType {
    /// Parallel flash
    pub const PARALLEL: Self = Self(1 << 0);
    /// LPC flash
    pub const LPC: Self = Self(1 << 1);
    /// FWH (Firmware Hub)
    pub const FWH: Self = Self(1 << 2);
    /// SPI flash
    pub const SPI: Self = Self(1 << 3);

    /// Parallel + FWH + LPC
    pub const NON_SPI: Self = Self(Self::PARALLEL.0 | Self::FWH.0 | Self::LPC.0);

    /// FWH + LPC + SPI (common for AMD chipsets)
    pub const FLS: Self = Self(Self::FWH.0 | Self::LPC.0 | Self::SPI.0);

    /// Check if SPI is supported
    pub fn supports_spi(self) -> bool {
        self.0 & Self::SPI.0 != 0
    }

    /// Check if any bus type is supported
    pub fn any(self) -> bool {
        self.0 != 0
    }
}

impl core::ops::BitOr for BusType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for BusType {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Intel chipset generation/type
///
/// This enum defines the SPI controller variants and their register layouts.
/// The ordering is significant: chipsets are grouped by compatible SPI engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum IchChipset {
    /// Unknown chipset
    Unknown = 0,
    /// Original ICH
    Ich,
    /// ICH2/ICH3/ICH4/ICH5
    Ich2345,
    /// ICH6
    Ich6,
    /// Intel SCH Poulsbo
    Poulsbo,
    /// Atom E6xx (Tunnel Creek)
    TunnelCreek,
    /// Atom S1220/S1240/S1260 (Centerton)
    Centerton,
    /// ICH7
    Ich7,

    // ======== ICH9 compatible from here on ========
    /// ICH8 (first ICH9-compatible SPI engine)
    Ich8,
    /// ICH9
    Ich9,
    /// ICH10
    Ich10,
    /// 5 Series (Ibex Peak)
    Series5IbexPeak,
    /// 6 Series (Cougar Point)
    Series6CougarPoint,
    /// 7 Series (Panther Point)
    Series7PantherPoint,
    /// Bay Trail / Avoton / Rangeley (Silvermont architecture)
    BayTrail,

    // ======== New component density from here on ========
    /// 8 Series (Lynx Point)
    Series8LynxPoint,
    /// 8 Series LP (Lynx Point LP)
    Series8LynxPointLp,
    /// 8 Series (Wellsburg)
    Series8Wellsburg,
    /// 9 Series (Wildcat Point)
    Series9WildcatPoint,
    /// 9 Series LP (Wildcat Point LP)
    Series9WildcatPointLp,

    // ======== PCH100 compatible from here on ========
    /// 100 Series (Sunrise Point)
    Series100SunrisePoint,
    /// C620 Series (Lewisburg)
    C620Lewisburg,
    /// 300 Series (Cannon Point)
    Series300CannonPoint,
    /// 500 Series (Tiger Point)
    Series500TigerPoint,
    /// Apollo Lake
    ApolloLake,
    /// Gemini Lake
    GeminiLake,
    /// Elkhart Lake
    ElkhartLake,

    // ======== New access permissions from here on ========
    /// C740 Series (Emmitsburg)
    C740Emmitsburg,
    /// Snow Ridge
    SnowRidge,
    /// Meteor Lake
    MeteorLake,
    /// Lunar Lake
    LunarLake,
    /// Arrow Lake
    ArrowLake,
}

impl IchChipset {
    /// Marker for ICH9-compatible SPI engine
    pub const SPI_ENGINE_ICH9: Self = Self::Ich8;

    /// Marker for new component density support
    pub const HAS_NEW_COMPONENT_DENSITY: Self = Self::Series8LynxPoint;

    /// Marker for PCH100-compatible SPI engine
    pub const SPI_ENGINE_PCH100: Self = Self::Series100SunrisePoint;

    /// Marker for new access permission registers (BM_RAP/WAP)
    pub const HAS_NEW_ACCESS_PERM: Self = Self::C740Emmitsburg;

    /// Returns true if this chipset uses ICH9-compatible SPI engine
    pub fn is_ich9_compatible(self) -> bool {
        self >= Self::SPI_ENGINE_ICH9
    }

    /// Returns true if this chipset uses PCH100-compatible SPI engine
    pub fn is_pch100_compatible(self) -> bool {
        self >= Self::SPI_ENGINE_PCH100
    }

    /// Returns true if this chipset has new component density support
    pub fn has_new_component_density(self) -> bool {
        self >= Self::HAS_NEW_COMPONENT_DENSITY
    }

    /// Returns true if this chipset has new access permission registers
    pub fn has_new_access_perm(self) -> bool {
        self >= Self::HAS_NEW_ACCESS_PERM
    }

    /// Returns true if this chipset supports hardware sequencing (hwseq)
    ///
    /// Hardware sequencing was introduced with ICH8. ICH7 only supports
    /// software sequencing.
    pub fn supports_hwseq(self) -> bool {
        self >= Self::SPI_ENGINE_ICH9
    }

    /// Returns true if this chipset supports software sequencing (swseq)
    ///
    /// Software sequencing is supported on all chipsets, but may be locked
    /// on some platforms (check SSEQ_LOCKDN in DLOCK register for PCH100+).
    /// Apollo Lake and later mobile platforms often lock swseq.
    pub fn supports_swseq(self) -> bool {
        // All chipsets support swseq in principle, but PCH100+ can lock it
        // via DLOCK.SSEQ_LOCKDN. This is checked at runtime.
        true
    }

    /// Returns true if this chipset defaults to hwseq when in auto mode
    ///
    /// PCH100+ series defaults to hwseq because swseq is often locked
    /// and hwseq provides better compatibility.
    pub fn defaults_to_hwseq(self) -> bool {
        self >= Self::SPI_ENGINE_PCH100
    }

    /// Returns true if this chipset is Apollo Lake or similar
    ///
    /// Apollo Lake and similar platforms (Gemini Lake, Elkhart Lake) often
    /// have restricted software sequencing capabilities.
    pub fn is_apollo_lake_like(self) -> bool {
        matches!(
            self,
            Self::ApolloLake | Self::GeminiLake | Self::ElkhartLake
        )
    }
}

impl fmt::Display for IchChipset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "Unknown"),
            Self::Ich => write!(f, "ICH"),
            Self::Ich2345 => write!(f, "ICH2/3/4/5"),
            Self::Ich6 => write!(f, "ICH6"),
            Self::Poulsbo => write!(f, "SCH Poulsbo"),
            Self::TunnelCreek => write!(f, "Tunnel Creek"),
            Self::Centerton => write!(f, "Centerton"),
            Self::Ich7 => write!(f, "ICH7"),
            Self::Ich8 => write!(f, "ICH8"),
            Self::Ich9 => write!(f, "ICH9"),
            Self::Ich10 => write!(f, "ICH10"),
            Self::Series5IbexPeak => write!(f, "5 Series (Ibex Peak)"),
            Self::Series6CougarPoint => write!(f, "6 Series (Cougar Point)"),
            Self::Series7PantherPoint => write!(f, "7 Series (Panther Point)"),
            Self::BayTrail => write!(f, "Bay Trail"),
            Self::Series8LynxPoint => write!(f, "8 Series (Lynx Point)"),
            Self::Series8LynxPointLp => write!(f, "8 Series LP (Lynx Point LP)"),
            Self::Series8Wellsburg => write!(f, "8 Series (Wellsburg)"),
            Self::Series9WildcatPoint => write!(f, "9 Series (Wildcat Point)"),
            Self::Series9WildcatPointLp => write!(f, "9 Series LP (Wildcat Point LP)"),
            Self::Series100SunrisePoint => write!(f, "100 Series (Sunrise Point)"),
            Self::C620Lewisburg => write!(f, "C620 (Lewisburg)"),
            Self::Series300CannonPoint => write!(f, "300 Series (Cannon Point)"),
            Self::Series500TigerPoint => write!(f, "500 Series (Tiger Point)"),
            Self::ApolloLake => write!(f, "Apollo Lake"),
            Self::GeminiLake => write!(f, "Gemini Lake"),
            Self::ElkhartLake => write!(f, "Elkhart Lake"),
            Self::C740Emmitsburg => write!(f, "C740 (Emmitsburg)"),
            Self::SnowRidge => write!(f, "Snow Ridge"),
            Self::MeteorLake => write!(f, "Meteor Lake"),
            Self::LunarLake => write!(f, "Lunar Lake"),
            Self::ArrowLake => write!(f, "Arrow Lake"),
        }
    }
}

/// A chipset enable entry in the PCI ID table
///
/// This mirrors flashprog's `struct penable`
#[derive(Debug, Clone)]
pub struct ChipsetEnable {
    /// PCI vendor ID
    pub vendor_id: u16,
    /// PCI device ID
    pub device_id: u16,
    /// Whether to match a specific revision ID
    pub revision: Option<u8>,
    /// Supported bus types
    pub buses: BusType,
    /// Test status
    pub status: TestStatus,
    /// Vendor name
    pub vendor_name: &'static str,
    /// Device/chipset name
    pub device_name: &'static str,
    /// Chipset type (determines which enable function to use)
    pub chipset: IchChipset,
}

impl ChipsetEnable {
    /// Create a new chipset enable entry
    pub const fn new(
        vendor_id: u16,
        device_id: u16,
        buses: BusType,
        status: TestStatus,
        vendor_name: &'static str,
        device_name: &'static str,
        chipset: IchChipset,
    ) -> Self {
        Self {
            vendor_id,
            device_id,
            revision: None,
            buses,
            status,
            vendor_name,
            device_name,
            chipset,
        }
    }

    /// Create a new chipset enable entry with revision matching
    #[allow(clippy::too_many_arguments)]
    pub const fn with_revision(
        vendor_id: u16,
        device_id: u16,
        revision: u8,
        buses: BusType,
        status: TestStatus,
        vendor_name: &'static str,
        device_name: &'static str,
        chipset: IchChipset,
    ) -> Self {
        Self {
            vendor_id,
            device_id,
            revision: Some(revision),
            buses,
            status,
            vendor_name,
            device_name,
            chipset,
        }
    }
}

// Convenience constants for bus type combinations (matching flashprog macros)
/// Parallel only
pub const B_P: BusType = BusType::PARALLEL;
/// Parallel + FWH + LPC
pub const B_PFL: BusType = BusType(BusType::PARALLEL.0 | BusType::FWH.0 | BusType::LPC.0);
/// Parallel + FWH + LPC + SPI
pub const B_PFLS: BusType =
    BusType(BusType::PARALLEL.0 | BusType::FWH.0 | BusType::LPC.0 | BusType::SPI.0);
/// FWH + LPC
pub const B_FL: BusType = BusType(BusType::FWH.0 | BusType::LPC.0);
/// FWH + LPC + SPI
pub const B_FLS: BusType = BusType(BusType::FWH.0 | BusType::LPC.0 | BusType::SPI.0);
/// FWH + SPI
pub const B_FS: BusType = BusType(BusType::FWH.0 | BusType::SPI.0);
/// LPC only
pub const B_L: BusType = BusType::LPC;
/// LPC + SPI
pub const B_LS: BusType = BusType(BusType::LPC.0 | BusType::SPI.0);
/// SPI only
pub const B_S: BusType = BusType::SPI;
