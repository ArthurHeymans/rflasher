//! Quick test to see what nusb finds

use nusb::MaybeFuture;

fn main() {
    println!("Listing all USB devices with VID 0x18D1 (Google)...\n");

    for dev_info in nusb::list_devices().wait().unwrap() {
        if dev_info.vendor_id() != 0x18D1 {
            continue;
        }

        println!(
            "Device: {:04x}:{:04x}",
            dev_info.vendor_id(),
            dev_info.product_id()
        );
        println!(
            "  Bus: {}, Address: {}",
            dev_info.busnum(),
            dev_info.device_address()
        );
        println!("  Product: {:?}", dev_info.product_string());
        println!("  Serial: {:?}", dev_info.serial_number());
        println!(
            "  Class: {:02x}, Subclass: {:02x}, Protocol: {:02x}",
            dev_info.class(),
            dev_info.subclass(),
            dev_info.protocol()
        );

        println!("  Interfaces from DeviceInfo::interfaces():");
        for iface in dev_info.interfaces() {
            let is_raiden = iface.class() == 0xFF && iface.subclass() == 0x51;
            println!(
                "    Interface {}: class={:02x} subclass={:02x} protocol={:02x}{}",
                iface.interface_number(),
                iface.class(),
                iface.subclass(),
                iface.protocol(),
                if is_raiden { " <-- RAIDEN SPI" } else { "" }
            );
        }

        // Try to open and get config descriptor
        println!("  Interfaces from active_configuration():");
        match dev_info.open().wait() {
            Ok(device) => match device.active_configuration() {
                Ok(config) => {
                    for iface in config.interface_alt_settings() {
                        let is_raiden = iface.class() == 0xFF && iface.subclass() == 0x51;
                        println!(
                            "    Interface {} alt {}: class={:02x} subclass={:02x} protocol={:02x}{}",
                            iface.interface_number(),
                            iface.alternate_setting(),
                            iface.class(),
                            iface.subclass(),
                            iface.protocol(),
                            if is_raiden { " <-- RAIDEN SPI" } else { "" }
                        );
                        for ep in iface.endpoints() {
                            println!("      EP {:02x}: {:?}", ep.address(), ep.transfer_type());
                        }
                    }
                }
                Err(e) => println!("    Failed to get config: {}", e),
            },
            Err(e) => println!("    Failed to open: {}", e),
        }
        println!();
    }
}
