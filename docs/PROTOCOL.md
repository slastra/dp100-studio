# DP100 USB HID protocol

ALIENTEK publishes no wire-protocol documentation for the DP100, only a
Windows .NET DLL. This document is our own description, assembled from the
community's work (see [Credits](#credits)) and checked against real hardware.

Each fact carries a confidence mark:

| Mark | Meaning |
|---|---|
| **HW** | Verified on our unit (hw 1.4, fw 1.5) |
| **M** | Several independent sources agree |
| **S** | A single source |
| **I** | Inferred, untested |

## Transport

- USB HID, VID `0x2E3C`, PID `0xAF01`, product string `ATK-MDP100`. **HW**
- One vendor-defined interface with 64-byte input and output reports and no
  report IDs. On Linux hidraw, each write is a `0x00` report-ID byte followed
  by 64 bytes; each read returns 64 bytes. **HW**
- Strict request/reply. There is no unsolicited or streaming data. **M**
- The unit must be in USBD mode (double-tap ◀ on the panel). In that mode the
  front USB-A port supplies no 5 V. **S** (manual)
- **Polling locks the front panel.** While a host polls at 20 Hz the unit
  refuses button presses with a beep. Once the host stops talking, the buttons
  work again; no command is needed to release them. **HW**

## Frame

```
offset  0     1    2    3    4 .. 4+len-1   4+len  5+len
        dir   op   seq  len  data           crc_lo crc_hi   (zero padding to 64)
```

| Field | Notes | |
|---|---|---|
| `dir` | `0xFB` host to device, `0xFA` device to host | **HW** |
| `op` | opcode, echoed in the reply | **HW** |
| `seq` | sequence byte. `0` always works; whether the device echoes other values is untested (E1) | **HW** for 0 |
| `len` | data length, at most 58 | **HW** |
| `crc` | CRC-16/MODBUS (init `0xFFFF`, reflected poly `0xA001`) over `dir` through the last data byte, little-endian | **HW** |

Write replies carry one data byte: `0x01` accepted, `0x00` rejected. **M**

Known request frames, all accepted by the device: **HW**

```
fb 30 00 00 31 0f          BASIC_INFO
fb 10 00 00 30 c5          DEVICE_INFO
fb 35 00 01 80 ce 28       BASIC_SET, read live output
fb 40 00 00 30 d4          SYSTEM_INFO
```

## Opcodes

| Op | Name | Request | Reply | |
|---|---|---|---|---|
| `0x10` | DEVICE_INFO | empty | 40 bytes | **HW** |
| `0x11`–`0x15` | firmware update | | | **M** (not used by this project) |
| `0x30` | BASIC_INFO | empty | 16 bytes | **HW** |
| `0x35` | BASIC_SET | sub-op + optional 9 bytes | 10 bytes or ACK | **HW** |
| `0x40` | SYSTEM_INFO | empty to read, 8 bytes to write | 8 bytes or ACK | **HW**; writes persist |
| `0x45` | SYSTEM_SET | | ignored by the device | **S** |
| `0x50` | SCAN_OUT | 12 bytes | ACK | **M** layout, untested |
| `0x55` | SERIAL_OUT | 10 bytes | ACK | **M** layout, untested |
| `0x80` | DISCONNECT | empty | ? | **I**, never sent by this project |

## DEVICE_INFO (40 bytes)

| Off | Type | Field | |
|---|---|---|---|
| 0 | `char[16]` | model, NUL-terminated, `0xFF` padded (`ATK-DP100`) | **HW** |
| 16 | u16 | hardware version × 10 | **HW** |
| 18 | u16 | firmware version × 10 | **HW** |
| 20 | u16 | bootloader version × 10 | **HW** |
| 22 | u16 | `0xAA` when the application is running, otherwise bootloader | **HW** |
| 24 | `u8[12]` | serial | **M** |
| 36 | u16 | year of manufacture | **HW** |
| 38 | u8 | month | **HW** |
| 39 | u8 | day | **HW** |

## BASIC_INFO (16 bytes)

| Off | Type | Field | Unit | |
|---|---|---|---|---|
| 0 | u16 | input voltage | mV | **HW** |
| 2 | u16 | output voltage | mV | **HW** |
| 4 | u16 | output current | mA | **HW** |
| 6 | u16 | highest output voltage the input allows | mV | **HW** |
| 8 | u16 | temperature 1 | 0.1 °C | **HW** |
| 10 | u16 | temperature 2 | 0.1 °C | **HW** |
| 12 | u16 | internal 5 V rail | mV | **HW** |
| 14 | u8 | mode: `0` CC, `1` CV, `2` off, `0x82` no input | | **HW** for 1, 2; **M** for 0; **S** for 0x82 |
| 15 | u8 | fault: `0` none, `1` OVP, `2` OCP, `3` OPP, `4` OTP, `5` reverse polarity, `6` UVP | | **S**, names match the manual |

Polling BASIC_INFO at 100 Hz is sustainable; requests take 1–2 ms. **HW**

## BASIC_SET (`0x35`)

The first data byte selects a sub-operation (high nibble) and a preset
(low nibble). The 10-byte struct used for reads and writes:

| Off | Type | Field |
|---|---|---|
| 0 | u8 | sub-op / index |
| 1 | u8 | output on (`1`) or off (`0`) |
| 2 | u16 | voltage setpoint, mV |
| 4 | u16 | current limit, mA |
| 6 | u16 | OVP, mV |
| 8 | u16 | OCP, mA |

| First byte | Length | Meaning | |
|---|---|---|---|
| `0x00 \| n` | 1 | read stored preset `n` | **HW** |
| `0x80` | 1 | read the live output; reply index is the active preset. The low nibble is ignored | **HW** |
| `0x20 \| n` | 10 | set the live output of the active preset `n`: on/off, voltage, current. **The OVP and OCP fields are ignored** (acknowledged, not applied). Volatile | **HW** (n = 0, 9) |
| `0x40 \| n` | 10 | save preset `n` to flash, any of 0–9. Saving the **active** preset also replaces its live record, OVP/OCP included, which take effect at once. This is the only way to change protection limits | **HW** (persists across power-cycle) |
| `0x60 \| n` | 10 | named "SaveGroup" in a decompile; not needed and never sent | **S** |
| `0xA0 \| n` | 10, zeros after byte 0 | make preset `n` active, 0 included. Its stored values become the live output; the output stays as it was | **HW** |

## SYSTEM_INFO (8 bytes)

| Off | Type | Field | |
|---|---|---|---|
| 0 | u16 | over-temperature trip, °C (50–80) | **HW** |
| 2 | u16 | over-power trip, 0.1 W (at most 1050) | **HW** |
| 4 | u8 | backlight, 0–4 | **HW** |
| 5 | u8 | volume, 0–4 | **HW** |
| 6 | u8 | reverse-polarity protection | **HW** |
| 7 | u8 | output on at power-up | **HW** |

Firmware before 1.3 likely returns only the first six bytes. **I**

## Firmware behaviour to work around

1. **The on/off byte from `0x80` can be wrong.** Saving the active preset
   rewrites the live record, on/off byte included, without switching the
   output. We saw `0x80` report off while BASIC_INFO showed the output
   regulating in CC. Take the output state from BASIC_INFO's mode (CC or CV
   means on). **HW**
2. **Protection limits only change through a save.** The live-output write
   ignores OVP and OCP. Save the active preset (`0x40 | active`) with its
   current setpoints and the new limits. They apply immediately: an OVP below
   the output voltage tripped within 150 ms. This also stores them in flash.
   **HW**
3. **The live-output write replaces every field it honours.** Read, change
   what you mean to, write it all back. Partial or zero fields glitch the
   output. **M**
4. **Writes apply asynchronously.** Reading back sooner than about 100 ms
   after a write can return the old values. **M**
5. **Turning off.** The DLL writes off with zero setpoints, waits 40 ms, then
   writes off again with the real setpoints. We do the same, and the
   setpoints survive it. **HW**
6. **The preset-0 bug.** Others report that activating a preset while preset
   0 is active turns the output on. On firmware 1.5 we could not reproduce it:
   twice from preset 0 with the output off (once straight after power-up),
   and once activating a preset whose stored on/off byte was 1. We still
   check the output after every activation and turn it off if it came on.
   **S** for the bug, **HW** for its absence on 1.5
7. **Switching back to preset 0 works** on firmware 1.5, despite reports
   that it needs a reboot. Each preset's live values persist in RAM across
   switches until power-cycle. **HW**
8. **Clearing a latched fault** takes a live write with the output off, plus
   about 150 ms. **HW**
9. **Stale replies.** After a timeout, the late reply can arrive in answer to
   the next request. Drain before each request and match replies by opcode. **M**
10. **After power-up** the unit appears on USB for about two seconds and then
    disconnects until it is put in USBD mode (double-tap ◀). It boots into
    preset 0 with the output off unless auto-on is set. Presets and system
    settings persist. **HW**

## Credits

- [jjcarrier/vicon](https://github.com/jjcarrier/vicon) (MIT): the most
  complete reconstruction of the official DLL, including a Wireshark
  dissector. Our primary reference.
- [weigu1/dp100_manipulator](https://github.com/weigu1/dp100_manipulator):
  USB captures of the official software, independent confirmation of several
  writes.
- [aIeXoid/DP100-Lab](https://github.com/aIeXoid/DP100-Lab),
  [codingjoe/DP100-WebApp](https://github.com/codingjoe/DP100-WebApp),
  [scottbez1/webdp100](https://github.com/scottbez1/webdp100),
  [AndySze/dp100-cli](https://github.com/AndySze/dp100-cli),
  [BrokenClient/DP100_ESP32_WebApp](https://github.com/BrokenClient/DP100_ESP32_WebApp),
  [ElluIFX/DP100-PyQt5-GUI](https://github.com/ElluIFX/DP100-PyQt5-GUI).
