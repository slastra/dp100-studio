//! CRC-16/MODBUS: init 0xFFFF, reflected polynomial 0xA001, no final XOR.

pub fn crc16(buf: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &b in buf {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modbus_check_value() {
        // The catalogue check value for CRC-16/MODBUS.
        assert_eq!(crc16(b"123456789"), 0x4B37);
    }
}
