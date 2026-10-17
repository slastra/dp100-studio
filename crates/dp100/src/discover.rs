//! Finds DP100 hidraw nodes through sysfs, with no libudev.

use std::fs;
use std::path::PathBuf;

pub const VENDOR_ID: u16 = 0x2E3C;
pub const PRODUCT_ID: u16 = 0xAF01;

/// What the kernel writes as `HID_ID` in the HID device's uevent: bus type
/// 0003 (USB), vendor, product.
const HID_ID: &str = "HID_ID=0003:00002E3C:0000AF01";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The `/dev/hidrawN` node.
    pub path: PathBuf,
    /// `HID_PHYS`, which identifies the USB port. Stable across replugs into
    /// the same socket, unlike the hidraw number.
    pub phys: Option<String>,
}

/// Every connected DP100, ordered by hidraw number.
pub fn enumerate() -> Vec<Found> {
    let Ok(dir) = fs::read_dir("/sys/class/hidraw") else {
        return Vec::new();
    };
    let mut found: Vec<Found> = dir
        .flatten()
        .filter_map(|entry| {
            let uevent = fs::read_to_string(entry.path().join("device/uevent")).ok()?;
            if !uevent.lines().any(|l| l.eq_ignore_ascii_case(HID_ID)) {
                return None;
            }
            let phys = uevent
                .lines()
                .find_map(|l| l.strip_prefix("HID_PHYS="))
                .map(str::to_owned);
            Some(Found {
                path: PathBuf::from("/dev").join(entry.file_name()),
                phys,
            })
        })
        .collect();
    found.sort_by_key(|f| {
        f.path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("hidraw"))
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    found
}
