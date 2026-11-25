//! CH341A protocol constants and helpers
//!
//! This module contains the USB protocol constants and bit manipulation
//! functions needed to communicate with the CH341A.

// Allow unused constants - these are register/command definitions for documentation
// and potential future use
#![allow(dead_code)]

// USB device identifiers
pub const CH341A_USB_VENDOR: u16 = 0x1A86;
pub const CH341A_USB_PRODUCT: u16 = 0x5512;

// USB endpoints
pub const WRITE_EP: u8 = 0x02;
pub const READ_EP: u8 = 0x82;

// USB timeout in milliseconds
pub const USB_TIMEOUT_MS: u64 = 1000;

// Packet sizes
pub const CH341_PACKET_LENGTH: usize = 0x20; // 32 bytes
pub const CH341_MAX_PACKETS: usize = 256;
pub const CH341_MAX_PACKET_LEN: usize = CH341_PACKET_LENGTH * CH341_MAX_PACKETS;

// Command codes
pub const CH341A_CMD_SET_OUTPUT: u8 = 0xA1;
pub const CH341A_CMD_IO_ADDR: u8 = 0xA2;
pub const CH341A_CMD_PRINT_OUT: u8 = 0xA3;
pub const CH341A_CMD_SPI_STREAM: u8 = 0xA8;
pub const CH341A_CMD_SIO_STREAM: u8 = 0xA9;
pub const CH341A_CMD_I2C_STREAM: u8 = 0xAA;
pub const CH341A_CMD_UIO_STREAM: u8 = 0xAB;

// I2C stream sub-commands
pub const CH341A_CMD_I2C_STM_START: u8 = 0x74;
pub const CH341A_CMD_I2C_STM_STOP: u8 = 0x75;
pub const CH341A_CMD_I2C_STM_OUT: u8 = 0x80;
pub const CH341A_CMD_I2C_STM_IN: u8 = 0xC0;
pub const CH341A_CMD_I2C_STM_SET: u8 = 0x60; // bit 2: SPI with two data pairs D5,D4=out, D7,D6=in
pub const CH341A_CMD_I2C_STM_US: u8 = 0x40;
pub const CH341A_CMD_I2C_STM_MS: u8 = 0x50;
pub const CH341A_CMD_I2C_STM_DLY: u8 = 0x0F;
pub const CH341A_CMD_I2C_STM_END: u8 = 0x00;

// UIO stream sub-commands
pub const CH341A_CMD_UIO_STM_IN: u8 = 0x00;
pub const CH341A_CMD_UIO_STM_DIR: u8 = 0x40;
pub const CH341A_CMD_UIO_STM_OUT: u8 = 0x80;
pub const CH341A_CMD_UIO_STM_US: u8 = 0xC0;
pub const CH341A_CMD_UIO_STM_END: u8 = 0x20;

// I2C/SPI speed settings
pub const CH341A_STM_I2C_20K: u8 = 0x00;
pub const CH341A_STM_I2C_100K: u8 = 0x01;
pub const CH341A_STM_I2C_400K: u8 = 0x02;
pub const CH341A_STM_I2C_750K: u8 = 0x03;
pub const CH341A_STM_SPI_DBL: u8 = 0x04;

// Pin mappings (from CH341A to SPI):
// UIO  CH341A  SPI     CH341A SPI name
// 0    D0/15   CS/1    (CS0)
// 1    D1/16   unused  (CS1)
// 2    D2/17   unused  (CS2)
// 3    D3/18   SCK/6   (DCK)
// 4    D4/19   unused  (DOUT2)
// 5    D5/20   SI/5    (DOUT)
// 6    D6/21   unused  (DIN2)
// 7    D7/22   SO/2    (DIN)

/// CS high, SCK=0, DOUT*=1
pub const UIO_CS_DEASSERT: u8 = 0x37;
/// CS low, SCK=0, DOUT*=1
pub const UIO_CS_ASSERT: u8 = 0x36;
/// All pins as output
pub const UIO_DIR_OUTPUT: u8 = 0x3F;
/// All pins as input (disabled)
pub const UIO_DIR_INPUT: u8 = 0x00;

/// Reverse the bits in a byte
///
/// The CH341A requires bit-reversed data for SPI transfers.
/// This is because of how the hardware is wired internally.
#[inline]
pub const fn reverse_byte(b: u8) -> u8 {
    b.reverse_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reverse_byte() {
        assert_eq!(reverse_byte(0x00), 0x00);
        assert_eq!(reverse_byte(0xFF), 0xFF);
        assert_eq!(reverse_byte(0x01), 0x80);
        assert_eq!(reverse_byte(0x80), 0x01);
        assert_eq!(reverse_byte(0x0F), 0xF0);
        assert_eq!(reverse_byte(0xF0), 0x0F);
        assert_eq!(reverse_byte(0xAA), 0x55);
        assert_eq!(reverse_byte(0x55), 0xAA);
        // Double reverse should give original
        for b in 0..=255u8 {
            assert_eq!(reverse_byte(reverse_byte(b)), b);
        }
    }
}
