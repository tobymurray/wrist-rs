//! The PMIC and the fuel gauge, read only (`HARDWARE.md`, "Power and reset domain").

use embassy_stm32::i2c::{Error, I2c, Master};
use embassy_stm32::mode::Async;

use crate::uart::Uart;

/// Blocking calls only before the first async transfer (`HARDWARE.md`, "Debug and boot").
pub type AsyncBus = I2c<'static, Async, Master>;

const PCA9420: u8 = wrist_board::i2c::PCA9420_PMIC;
const MAX17262: u8 = wrist_board::i2c::MAX17262_GAUGE;

/// `None` is a read that failed.
#[derive(Clone, Copy, Default)]
pub struct Snapshot {
    pub dev_info: Option<u8>,
    pub top_cntl0: Option<u8>,
    pub top_cntl1: Option<u8>,
    pub top_cntl2: Option<u8>,
    pub top_cntl3: Option<u8>,
    pub chg_status0: Option<u8>,
    pub chg_status1: Option<u8>,
    pub reg_status: Option<u8>,
    pub modecfg0_3: Option<u8>,
    pub reset_monitor: Option<u8>,
    pub vcell_raw: Option<u16>,
    pub soc_raw: Option<u16>,
    pub temp_raw: Option<u16>,
    pub current_raw: Option<u16>,
    pub avg_current_raw: Option<u16>,
    pub tte_raw: Option<u16>,
    pub status2: Option<u16>,
    pub hib_cfg: Option<u16>,
    /// In 0.5 mAh.
    pub full_cap_raw: Option<u16>,
    pub design_cap_raw: Option<u16>,
}

/// (chip, register, two bytes wide), in [`Snapshot`]'s order.
const READS: [(u8, u8, bool); 20] = [
    (PCA9420, 0x00, false),
    (PCA9420, 0x09, false),
    (PCA9420, 0x0A, false),
    (PCA9420, 0x0B, false),
    (PCA9420, 0x0C, false),
    (PCA9420, 0x18, false),
    (PCA9420, 0x19, false),
    (PCA9420, 0x20, false),
    (PCA9420, 0x25, false),
    (PCA9420, 0x71, false),
    (MAX17262, 0x09, true),
    (MAX17262, 0x06, true),
    (MAX17262, 0x08, true),
    (MAX17262, 0x0A, true),
    (MAX17262, 0x0B, true),
    (MAX17262, 0x11, true),
    (MAX17262, 0xB0, true),
    (MAX17262, 0xBA, true),
    (MAX17262, 0x10, true),
    (MAX17262, 0x18, true),
];

fn assemble(v: &[Option<u16>; READS.len()]) -> Snapshot {
    let b = |i: usize| v[i].and_then(|x| u8::try_from(x).ok());
    Snapshot {
        dev_info: b(0),
        top_cntl0: b(1),
        top_cntl1: b(2),
        top_cntl2: b(3),
        top_cntl3: b(4),
        chg_status0: b(5),
        chg_status1: b(6),
        reg_status: b(7),
        modecfg0_3: b(8),
        reset_monitor: b(9),
        vcell_raw: v[10],
        soc_raw: v[11],
        temp_raw: v[12],
        current_raw: v[13],
        avg_current_raw: v[14],
        tte_raw: v[15],
        status2: v[16],
        hib_cfg: v[17],
        full_cap_raw: v[18],
        design_cap_raw: v[19],
    }
}

fn value(b: [u8; 2], wide: bool) -> u16 {
    if wide {
        u16::from_le_bytes(b)
    } else {
        u16::from(b[0])
    }
}

pub fn snapshot_blocking(bus: &mut AsyncBus) -> Snapshot {
    let mut v = [None; READS.len()];
    for (slot, &(addr, reg, wide)) in v.iter_mut().zip(READS.iter()) {
        let mut b = [0u8; 2];
        let n = if wide { 2 } else { 1 };
        *slot = bus.blocking_write_read(addr, &[reg], &mut b[..n]).ok().map(|()| value(b, wide));
    }
    assemble(&v)
}

pub async fn snapshot(bus: &mut AsyncBus) -> Snapshot {
    let mut v = [None; READS.len()];
    for (slot, &(addr, reg, wide)) in v.iter_mut().zip(READS.iter()) {
        let mut b = [0u8; 2];
        let n = if wide { 2 } else { 1 };
        let r: Result<(), Error> = bus.write_read(addr, &[reg], &mut b[..n]).await;
        *slot = r.ok().map(|()| value(b, wide));
    }
    assemble(&v)
}

fn hex_or_dashes(uart: Uart, v: Option<u8>) {
    match v {
        Some(b) => uart.write_hex8(b),
        None => uart.write_str("--"),
    }
}

/// Tenths of a percent, at most 1000.
#[must_use]
pub fn soc_tenths(raw: u16) -> u32 {
    (u32::from(raw) * 10 / 256).min(1000)
}

#[must_use]
pub fn soc_percent(raw: u16) -> u8 {
    u8::try_from(u32::from(raw) / 256).unwrap_or(100).min(100)
}

pub fn vcell_mv(raw: u16) -> u32 {
    u32::from(raw) * 625 / 8000
}

/// Positive into the battery.
pub fn current_ua(raw: u16) -> i32 {
    i32::from(raw.cast_signed()) * 625 / 4
}

fn write_ua(uart: Uart, raw: Option<u16>) {
    match raw {
        Some(r) => {
            let ua = current_ua(r);
            if ua < 0 {
                uart.write_str("-");
            }
            uart.write_u32(ua.unsigned_abs());
            uart.write_str(" uA");
        }
        None => uart.write_str("--"),
    }
}

pub fn write_summary(uart: Uart, s: &Snapshot) {
    uart.write_str("[pwr] ");
    match s.vcell_raw {
        Some(raw) => {
            uart.write_u32(vcell_mv(raw));
            uart.write_str(" mV");
        }
        None => uart.write_str("gauge --"),
    }
    if let Some(raw) = s.soc_raw {
        let tenths = soc_tenths(raw);
        uart.write_str("  soc ");
        uart.write_u32(tenths / 10);
        uart.write_str(".");
        uart.write_u32(tenths % 10);
        uart.write_str("%");
    }
    if let Some(raw) = s.temp_raw {
        let t = i32::from(raw.cast_signed());
        uart.write_str("  ");
        if t < 0 {
            uart.write_str("-");
        }
        uart.write_u32(t.unsigned_abs() / 256);
        uart.write_str(" C");
    }
    uart.write_str("  now ");
    write_ua(uart, s.current_raw);
    uart.write_str("  avg ");
    write_ua(uart, s.avg_current_raw);
    uart.write_str("  rails ");
    match s.reg_status {
        Some(r) => uart.write_str(if r & 0xF0 == 0xF0 {
            "all good"
        } else {
            "NOT ALL GOOD"
        }),
        None => uart.write_str("--"),
    }
    uart.write_str("  CHG_STATUS ");
    hex_or_dashes(uart, s.chg_status0);
    uart.write_str(" ");
    hex_or_dashes(uart, s.chg_status1);
    uart.write_str("  gauge ");
    uart.write_str(match s.status2 {
        Some(st) if wrist_power::hibernating(st) => "hibernating",
        Some(_) => "active",
        None => "--",
    });
    for (label, raw) in [(", Status2 ", s.status2), (", HibCfg ", s.hib_cfg)] {
        uart.write_str(label);
        match raw {
            Some(c) => {
                let [hi, lo] = c.to_be_bytes();
                uart.write_hex8(hi);
                uart.write_hex8(lo);
            }
            None => uart.write_str("--"),
        }
    }
    for (label, raw) in [(", full ", s.full_cap_raw), (" of ", s.design_cap_raw)] {
        uart.write_str(label);
        match raw {
            Some(r) => uart.write_u32(u32::from(r) / 2),
            None => uart.write_str("--"),
        }
    }
    uart.write_line(" mAh");
}

pub fn write_report(uart: Uart, s: &Snapshot) {
    uart.write_line("-- power chips on I2C1 (read only) --");
    uart.write_str("PCA9420 DEV_INFO ");
    hex_or_dashes(uart, s.dev_info);
    uart.write_str("  TOP_CNTL0..3 ");
    for v in [s.top_cntl0, s.top_cntl1, s.top_cntl2, s.top_cntl3] {
        hex_or_dashes(uart, v);
        uart.write_str(" ");
    }
    uart.write_line("");
    if let Some(c0) = s.top_cntl0 {
        uart.write_str("  watchdog on expiry: ");
        uart.write_line(if c0 & (1 << 2) != 0 {
            "SYSRSTn + charging suspended"
        } else {
            "SYSRSTn, charging continues"
        });
    }
    if let Some(c1) = s.top_cntl1 {
        uart.write_str("  VIN UVLO ");
        uart.write_str(["2.9", "3.1", "3.3", "3.5"][usize::from(c1 & 3)]);
        uart.write_line(" V");
    }
    if let Some(c3) = s.top_cntl3 {
        uart.write_str("  ON long press ");
        uart.write_u32(4 * (1 + u32::from(c3 & 3)));
        uart.write_str(" s, mode ");
        uart.write_u32(u32::from((c3 >> 3) & 3));
        uart.write_line("");
    }
    if let Some(m) = s.modecfg0_3 {
        uart.write_str("  mode 0 WD_TIMER: ");
        uart.write_line(["disabled", "16 s", "32 s", "64 s"][usize::from(m >> 6)]);
    }
    if let Some(r) = s.reg_status {
        uart.write_str("  REG_STATUS ");
        uart.write_hex8(r);
        uart.write_str("  SW1 ");
        uart.write_str(if r & 0x80 != 0 { "ok" } else { "BAD" });
        uart.write_str("  SW2 ");
        uart.write_str(if r & 0x40 != 0 { "ok" } else { "BAD" });
        uart.write_str("  LDO1 ");
        uart.write_str(if r & 0x20 != 0 { "ok" } else { "BAD" });
        uart.write_str("  LDO2 ");
        uart.write_line(if r & 0x10 != 0 { "ok" } else { "BAD" });
    }
    if let Some(m) = s.reset_monitor {
        uart.write_str("  reset monitor 0x71 ");
        uart.write_hex8(m);
        uart.write_str(":");
        for (bit, name) in [
            (7, " ON-pin-high"),
            (4, " THM_SHDN_DONE"),
            (3, " SHIP_EXIT_DONE"),
            (2, " SW_RST_DONE"),
            (1, " WD_RST_DONE"),
            (0, " LONG_ON_DONE"),
        ] {
            if m & (1 << bit) != 0 {
                uart.write_str(name);
            }
        }
        uart.write_line("");
    }
}
