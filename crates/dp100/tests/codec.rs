//! Frame encoding against requests the real device has accepted, and
//! decoding of replies built the way the device builds them.

use dp100::frame::{DIR_DEVICE, REPORT_LEN, decode, encode};
use dp100::{FrameError, crc16, op};

fn bytes(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn head(r: &[u8; REPORT_LEN], n: usize) -> &[u8] {
    &r[..n]
}

#[test]
fn known_requests() {
    let cases: [(u8, &[u8], &str); 4] = [
        (op::BASIC_INFO, &[], "fb 30 00 00 31 0f"),
        (op::DEVICE_INFO, &[], "fb 10 00 00 30 c5"),
        (
            op::BASIC_SET,
            &[op::sub::READ_CURRENT],
            "fb 35 00 01 80 ce 28",
        ),
        (op::SYSTEM_INFO, &[], "fb 40 00 00 30 d4"),
    ];
    for (op, data, want) in cases {
        let want = bytes(want);
        let r = encode(op, 0, data);
        assert_eq!(head(&r, want.len()), want.as_slice(), "op {op:#04x}");
        assert!(r[want.len()..].iter().all(|&b| b == 0), "padding");
    }
}

fn device_reply(op: u8, data: &[u8]) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[0] = DIR_DEVICE;
    r[1] = op;
    r[3] = data.len() as u8;
    r[4..4 + data.len()].copy_from_slice(data);
    let end = 4 + data.len();
    let c = crc16(&r[..end]);
    r[end..end + 2].copy_from_slice(&c.to_le_bytes());
    r
}

#[test]
fn decodes_basic_info_reply() {
    let payload = bytes("94 23 00 00 00 00 6a 22 19 01 14 01 cf 13 02 00");
    let f = decode(&device_reply(op::BASIC_INFO, &payload)).unwrap();
    assert_eq!(f.op, op::BASIC_INFO);
    assert_eq!(f.data, payload);
}

#[test]
fn rejects_host_frames() {
    let r = encode(op::BASIC_INFO, 0, &[]);
    assert_eq!(decode(&r), Err(FrameError::BadDirection(0xFB)));
}

#[test]
fn rejects_corruption() {
    let mut r = device_reply(op::BASIC_INFO, &[1, 2, 3]);
    r[5] ^= 0x40;
    assert!(matches!(decode(&r), Err(FrameError::BadCrc { .. })));
}

#[test]
fn rejects_impossible_length() {
    let mut r = device_reply(op::BASIC_INFO, &[]);
    r[3] = 59;
    assert_eq!(decode(&r), Err(FrameError::BadLength(59)));
}
