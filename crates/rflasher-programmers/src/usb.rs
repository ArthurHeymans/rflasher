//! Selecting and opening one of the connected USB programmers.
//!
//! Each USB backend implements [`UsbProgrammer`]: it lists the connected
//! devices it supports that match the selector options in its config (serial
//! number, index, ...), and opens one of them. Frontends decide between
//! several matches through a [`ChooseUsbDevice`] callback.

use std::fmt;

/// A connected USB device, described for a frontend's device picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDeviceSummary {
    /// USB product string
    pub product: Option<String>,
    /// USB vendor ID
    pub vendor_id: u16,
    /// USB product ID
    pub product_id: u16,
    /// USB bus identifier (platform-defined; integer string on Linux)
    pub bus_id: String,
    /// USB device address
    pub address: u8,
    /// USB serial number
    pub serial: Option<String>,
}

impl fmt::Display for UsbDeviceSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({:04x}:{:04x}) at bus {} address {}",
            self.product.as_deref().unwrap_or("USB device"),
            self.vendor_id,
            self.product_id,
            self.bus_id,
            self.address
        )?;
        if let Some(serial) = &self.serial {
            write!(f, " serial={serial}")?;
        }
        Ok(())
    }
}

/// Picks one of several matching devices, returning its index.
///
/// Only called when at least two devices match.
pub type ChooseUsbDevice<'a> = &'a dyn Fn(&[UsbDeviceSummary]) -> Result<usize, String>;

#[cfg(feature = "usb")]
impl From<&nusb::DeviceInfo> for UsbDeviceSummary {
    fn from(info: &nusb::DeviceInfo) -> Self {
        Self {
            product: info.product_string().map(str::to_string),
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            bus_id: info.bus_id().to_string(),
            address: info.device_address(),
            serial: info.serial_number().map(str::to_string),
        }
    }
}

/// A programmer reached over native USB.
#[cfg(feature = "usb")]
pub trait UsbProgrammer: Sized {
    /// Name used in messages, e.g. "CH341A".
    const NAME: &'static str;
    /// Backend configuration, including any device selector options.
    type Config;
    /// Backend error type.
    type Error: std::error::Error + 'static;

    /// Connected devices this backend supports that match the selector
    /// options in `config`.
    async fn candidates(config: &Self::Config) -> Result<Vec<nusb::DeviceInfo>, Self::Error>;

    /// Open one of the devices returned by [`Self::candidates`].
    async fn open_device(
        device: nusb::DeviceInfo,
        config: Self::Config,
    ) -> Result<Self, Self::Error>;

    /// Open the device matching `config`.
    ///
    /// When several devices match, `choose` picks one; without it, this
    /// fails rather than guessing.
    async fn open_matching(
        config: Self::Config,
        choose: Option<ChooseUsbDevice<'_>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut devices = Self::candidates(&config).await?;
        let summaries: Vec<_> = devices.iter().map(UsbDeviceSummary::from).collect();
        let index = choose_index(Self::NAME, &summaries, choose)?;
        log::info!("Opening {} {}", Self::NAME, summaries[index]);
        Ok(Self::open_device(devices.swap_remove(index), config).await?)
    }
}

/// Connected devices matching any of `selectors`.
#[cfg(feature = "usb")]
pub(crate) async fn list_devices(
    selectors: &[nusb::DeviceSelector],
) -> Result<Vec<nusb::DeviceInfo>, nusb::Error> {
    Ok(nusb::list_devices()
        .await?
        .filter(|info| selectors.iter().any(|s| info.matches(s)))
        .collect())
}

#[cfg_attr(not(feature = "usb"), allow(dead_code))]
fn choose_index(
    name: &str,
    devices: &[UsbDeviceSummary],
    choose: Option<ChooseUsbDevice<'_>>,
) -> Result<usize, String> {
    match (devices.len(), choose) {
        (0, _) => Err(format!("No {name} device found")),
        (1, _) => Ok(0),
        (_, None) => Err(devices.iter().enumerate().fold(
            format!(
                "Multiple {name} devices found; select one with a programmer option \
                 or from an interactive terminal:"
            ),
            |msg, (i, device)| format!("{msg}\n  {i}: {device}"),
        )),
        (len, Some(choose)) => match choose(devices)? {
            index if index < len => Ok(index),
            index => Err(format!("Invalid device selection: {index}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(address: u8) -> UsbDeviceSummary {
        UsbDeviceSummary {
            product: None,
            vendor_id: 0x1a86,
            product_id: 0x5512,
            bus_id: "1".into(),
            address,
            serial: None,
        }
    }

    #[test]
    fn choose_is_only_asked_between_several_devices() {
        let refuse: ChooseUsbDevice = &|_| panic!("asked to choose");
        assert!(choose_index("X", &[], Some(refuse)).is_err());
        assert_eq!(choose_index("X", &[device(1)], Some(refuse)), Ok(0));

        let second: ChooseUsbDevice = &|_| Ok(1);
        assert_eq!(
            choose_index("X", &[device(1), device(2)], Some(second)),
            Ok(1)
        );
    }

    #[test]
    fn several_devices_without_choose_fail_listing_them() {
        let err = choose_index("X", &[device(1), device(2)], None).unwrap_err();
        assert!(err.contains("0: USB device (1a86:5512) at bus 1 address 1"));
        assert!(err.contains("1: USB device (1a86:5512) at bus 1 address 2"));
    }

    #[test]
    fn out_of_range_choice_is_rejected() {
        let third: ChooseUsbDevice = &|_| Ok(2);
        assert!(choose_index("X", &[device(1), device(2)], Some(third)).is_err());
    }
}
