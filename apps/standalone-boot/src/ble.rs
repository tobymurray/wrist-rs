//! The BlueNRG-2 on the link1 module (Sensor 1), driven by ST's ACI: the console and
//! application updates over the air, for a bonded central only (`ble` feature).
//!
//! The radio is off until `ble on`, and `ble off` holds it in reset again; the choice is kept
//! in `BKP16R` across resets (`rtc::radio_wanted`), so an image that arrived over the air
//! comes back with its radio on. When on, the task resets the chip, gives it an address,
//! brings up ST's GATT and GAP, and advertises as `UNA`.
//!
//! Security. Pairing is LE Secure Connections with numeric comparison, MITM required: the
//! central shows a number, `ble` on the USB console shows the same one, and `ble yes`
//! answers it (`ble no`, or 60 s, rejects). With MITM required no unauthenticated key
//! exists, so an encrypted link is a bonded central's. The console's RX characteristic asks
//! the stack for an authenticated, encrypted link, and the watch checks too: nothing written
//! over a link that is not encrypted is acted on, and the console's output is mirrored only to
//! an encrypted link. The loader is never accepted over the air. The watch asks nothing of a
//! central at connect, and asks it to pair only once it has left the link unencrypted for a
//! while (`wrist_wake::ble::PairAsk`).
//!
//! Power. On a cable the radio is as responsive as the link allows; on battery it
//! advertises about once a second and asks for a slow, latent link, fast only while a
//! transfer lasts (`wrist_wake::ble::timing`). The loop has no tick: it waits for the chip's
//! IRQ, a request, a signal, a power change or its earliest deadline (`wrist_wake::ble::due`).
//!
//! Pins: SPI1 `SCK` PG2, `MISO` PG3, `MOSI` PB5; `CS` PA15 as a GPIO; `BLE_RST_L` PA8
//! (active low); `BLE_BOOT_SPI_IRQ` PA9, the chip's `DIO7`, which is both the transport's
//! "data ready / slave ready" line and the bootloader-entry pin if high at reset, so it is an
//! input with a pull-down and never driven. SPI1, CS and IRQ exist only while the radio is on.
//!
//! The SPI handshake is `wrist_bluenrg2::transport`.
//!
//! Restarts. The chip sends that event after any reset, and one the driver's start did not ask
//! for means it restarted by itself (`wrist_wake::ble::restart`): the link and everything on it
//! is dropped, no command goes to it, and it is reset through its pin and brought up again, which
//! also leaves its updater. More than three in an hour and it is held in reset.
//!
//! Address. A static random address from the MCU's unique id
//! (`wrist_bluenrg2::bonds::static_random_address`), so bonds survive updates.

#[cfg(feature = "ble-probe")]
mod probe;
#[cfg(feature = "ble-trace")]
mod trace;
mod scan;

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

use embassy_futures::select::{select, select4, Either4};
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::Async;
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{bind_interrupts, dma, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_sync::pipe::Pipe;
use embassy_sync::signal::Signal;
use embassy_time::{Delay, Duration, Instant, Timer};
use bluenrg2::opcode::{
    ACI_GAP_ALLOW_REBOND, ACI_GAP_CLEAR_SECURITY_DB, ACI_GAP_GET_BONDED_DEVICES, ACI_GAP_REMOVE_BONDED_DEVICE, ACI_GAP_INIT,
    ACI_GAP_NUMERIC_COMPARISON_CONFIRM, ACI_GAP_PASS_KEY_RESP,
    ACI_GAP_SET_AUTHENTICATION_REQUIREMENT, ACI_GAP_SET_DISCOVERABLE, ACI_GAP_SET_IO_CAPABILITY,
    ACI_GAP_SET_NON_DISCOVERABLE, ACI_GAP_SLAVE_SECURITY_REQ, ACI_GAP_TERMINATE, ACI_GATT_ADD_CHAR,
    ACI_GATT_ADD_SERVICE, ACI_GATT_INIT,
    ACI_GATT_UPDATE_CHAR_VALUE, ACI_HAL_GET_FW_BUILD_NUMBER, ACI_HAL_SET_TX_POWER_LEVEL, ACI_HAL_WRITE_CONFIG_DATA,
    ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ, HCI_LE_RAND, HCI_READ_LOCAL_VERSION, HCI_SET_EVENT_MASK,
};
use bluenrg2::{
    adv_filter_policy, adv_type, boot_reason, config, enc_key_size, event_code, evt_mask, gap_role, pairing_status, permission, privacy, props, service_type,
    uuid_type, value_length, vendor, AddrKind, AttributeModified, Event, BleStatus, L2capUpdateResponse, ATT_HEADER_LEN,
    MAX_COMMAND, RETURN_PARAMS_MAX,
};
use wrist_bluenrg2::transport::{self, Counts, Read, Transport};
use wrist_pages::{OldKey, Pairing, Prompt};
use wrist_wake::ble::restart::{Boot, Restarts};
use wrist_wake::ble::{timing, IntervalAsk, Lines, Outbox, PairAsk, Power, Radio, Scan, Sink, Timing};

use crate::console::Target;
use crate::uart::Uart;
use crate::update::FlashMutex;

bind_interrupts!(struct Irqs {
    EXTI9 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI9>;
    GPDMA1_CHANNEL2 => dma::InterruptHandler<peripherals::GPDMA1_CH2>;
    GPDMA1_CHANNEL3 => dma::InterruptHandler<peripherals::GPDMA1_CH3>;
});

pub struct Pins {
    pub spi: Peri<'static, peripherals::SPI1>,
    pub sck: Peri<'static, peripherals::PG2>,
    pub miso: Peri<'static, peripherals::PG3>,
    pub mosi: Peri<'static, peripherals::PB5>,
    pub cs: Peri<'static, peripherals::PA15>,
    pub irq: Peri<'static, peripherals::PA9>,
    pub irq_exti: Peri<'static, peripherals::EXTI9>,
    pub rst: Peri<'static, peripherals::PA8>,
    pub tx_dma: Peri<'static, peripherals::GPDMA1_CH2>,
    pub rx_dma: Peri<'static, peripherals::GPDMA1_CH3>,
}

const BOOT_TIMEOUT: Duration = Duration::from_millis(3000);
const REPLY_TIMEOUT: Duration = Duration::from_millis(1000);
/// A transfer with no bytes for this long is abandoned, as over USB.
const TRANSFER_IDLE: Duration = Duration::from_secs(5);

const DEVICE_NAME: &[u8] = b"UNA";
/// `ACI_GAP_INIT`'s `device_name_char_len`.
const GAP_NAME_LEN: u8 = 7;
/// `ACI_HAL_SET_TX_POWER_LEVEL`'s `En_High_Power` and `PA_Level`.
const TX_POWER: [u8; 2] = [0x01, 0x04];
/// `HCI_SET_EVENT_MASK` (Core Vol 4 Part E 7.3.1).
const EVENT_MASK: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xBF, 0x3D];
// Bluetooth SIG assigned numbers: the Device Information service.
const UUID_DEVICE_INFORMATION: u16 = 0x180A;
const UUID_MANUFACTURER_NAME: u16 = 0x2A29;
const UUID_MODEL_NUMBER: u16 = 0x2A24;
const UUID_FIRMWARE_REVISION: u16 = 0x2A26;
const MANUFACTURER: &[u8] = b"wrist-rs";
const MODEL: &[u8] = b"UNA Watch";
// The Nordic UART Service shape, which nRF Connect's UART tab recognises:
// 6E40000x-B5A3-F393-E0A9-E50E24DCCA9E, little-endian as the chip wants it.
const NUS_BASE: [u8; 16] = [0x9E, 0xCA, 0xDC, 0x24, 0x0E, 0xE5, 0xA9, 0xE0, 0x93, 0xF3, 0xA3, 0xB5, 0x00, 0x00, 0x40, 0x6E];
const NUS_SERVICE: u8 = 0x01;
/// The central writes commands and image bytes here.
const NUS_RX: u8 = 0x02;
/// The watch notifies console output here.
const NUS_TX: u8 = 0x03;
/// The largest ATT MTU the console takes.
const MAX_MTU: usize = 247;
/// The characteristics' value size: the most [`MAX_MTU`] carries.
const NUS_VALUE_LEN: usize = MAX_MTU - ATT_HEADER_LEN;
/// Until an exchange raises it.
const DEFAULT_MTU: usize = bluenrg2::ATT_DEFAULT_MTU;
/// The value a default-MTU write or notification carries.
const DEFAULT_VALUE_LEN: usize = DEFAULT_MTU - ATT_HEADER_LEN;
/// `IO_CAP_DISPLAY_YES_NO`: the console shows the number, `ble yes` answers.
const IO_CAP_DISPLAY_YES_NO: u8 = 0x01;
/// Console notifications at most this often.
const CONSOLE_PACE: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    On,
    Off,
    Version,
    Yes,
    No,
    Bonds,
    Forget,
    /// `ble forget <address>`: one bond, by its address and type.
    ForgetOne(bluenrg2::Address),
    Disconnect,
    /// Ask for this interval range (1.25 ms units) on the current connection.
    IntervalNow(u16, u16),
    /// `ble scan`: seconds, and a duty in place of the power source's.
    Scan(u8, Option<Scan>),
    #[cfg(feature = "ble-probe")]
    Probe,
    /// `ble probe gatt services|records|values`: fill the GATT table until the chip refuses.
    #[cfg(feature = "ble-probe")]
    ProbeGatt(probe::Gatt),
    /// `ble restart [updater]`: the chip restarted under the driver, through its pin or its
    /// updater's command.
    #[cfg(feature = "ble-probe")]
    Restart(bool),
    /// `ble probe adv`: `ACI_GAP_SET_DISCOVERABLE` sent while already advertising.
    #[cfg(feature = "ble-probe")]
    ProbeAdvertising,
    /// `ble probe drop <hex>`: `ACI_GAP_TERMINATE` with this reason.
    #[cfg(feature = "ble-probe")]
    ProbeDrop(u8),
}

static REQUESTS: Channel<CriticalSectionRawMutex, Request, 4> = Channel::new();
/// Raised by other tasks that leave the loop work it cannot see coming: console output for a
/// subscribed central. The loop clears it before it reads that state.
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Console output for the central, filled while [`MIRROR`] is set.
static OUT: Pipe<CriticalSectionRawMutex, 2048> = Pipe::new();
/// Console output on its way into [`OUT`], a whole line at a time.
static LINES: Mutex<CriticalSectionRawMutex, RefCell<Lines<256>>> = Mutex::new(RefCell::new(Lines::new()));
/// Lines the mirror dropped because [`OUT`] was full.
static LINES_DROPPED: AtomicU32 = AtomicU32::new(0);
/// A central is subscribed to TX over an encrypted link.
static MIRROR: AtomicBool = AtomicBool::new(false);

const STATE_OFF: u8 = Radio::Off as u8;
const STATE_STARTING: u8 = Radio::Starting as u8;
const STATE_IDLE: u8 = Radio::Idle as u8;
const STATE_ADVERTISING: u8 = Radio::Advertising as u8;
const STATE_CONNECTED: u8 = Radio::Connected as u8;
const STATE_FAILED: u8 = Radio::Failed as u8;
static STATE: AtomicU8 = AtomicU8::new(STATE_OFF);
static ENCRYPTED: AtomicBool = AtomicBool::new(false);
/// An encrypted link was up at some point this boot: a way to send the next image.
static PROVEN: AtomicBool = AtomicBool::new(false);
static CONN_HANDLE: AtomicU16 = AtomicU16::new(0);
static TRANSPORT: Counts = Counts::new();
/// Console writes that arrived with a nonzero offset (fragments).
static FRAGMENTS: AtomicU32 = AtomicU32::new(0);
/// Writes dropped because the link was not encrypted.
static UNSECURED: AtomicU32 = AtomicU32::new(0);
/// The link's connection interval (1.25 ms units) and slave latency.
static INTERVAL: AtomicU16 = AtomicU16::new(0);
static LATENCY: AtomicU16 = AtomicU16::new(0);
/// Console writes identical to the one before (a write delivered twice).
static REPEATS: AtomicU32 = AtomicU32::new(0);
/// Events that carried an error status, and the last update failure's status.
static CONNECT_FAILURES: AtomicU32 = AtomicU32::new(0);
static UPDATE_FAILURES: AtomicU32 = AtomicU32::new(0);
static LAST_UPDATE_FAILURE: AtomicU8 = AtomicU8::new(0);
/// The central's L2CAP answers refusing the watch's parameter request.
static UPDATES_REFUSED: AtomicU32 = AtomicU32::new(0);
static DISCONNECT_FAILURES: AtomicU32 = AtomicU32::new(0);
static ENCRYPTION_FAILURES: AtomicU32 = AtomicU32::new(0);
/// Packets the image does not act on: other events, unhandled vendor events, malformed packets.
static UNKNOWN: AtomicU32 = AtomicU32::new(0);
/// Times the chip restarted by itself, and the last one's `ACI_BLUE_INITIALIZED` reason.
static RESTARTS: AtomicU32 = AtomicU32::new(0);
static LAST_RESTART: AtomicU8 = AtomicU8::new(0);
static BONDED: AtomicU8 = AtomicU8::new(0);
static BONDED_READ: AtomicBool = AtomicBool::new(false);
/// Last pairing outcome: 0 none, else 1 + status (so 1 is success).
static LAST_PAIRING: AtomicU8 = AtomicU8::new(0);
static LAST_PAIRING_REASON: AtomicU8 = AtomicU8::new(0);
static PROMPT: Mutex<CriticalSectionRawMutex, Cell<Prompt>> = Mutex::new(Cell::new(Prompt::None));
static OLD_KEY: Mutex<CriticalSectionRawMutex, Cell<OldKey<bluenrg2::Address>>> = Mutex::new(Cell::new(OldKey::NONE));
/// The address written into the chip, little-endian as the chip holds it.
static ADDRESS: [AtomicU8; 6] = [const { AtomicU8::new(0) }; 6];
/// `ble profile`: 0 follows the power source, 1 always the cable's, 2 always the battery's.
static PROFILE: AtomicU8 = AtomicU8::new(0);

/// Signalled when [`PROVEN`] first becomes true.
static PROVEN_NOW: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// An encrypted link has been up this boot (the heartbeat keeps an update on it).
pub fn proven() -> bool {
    PROVEN.load(Ordering::Relaxed)
}

/// Until an encrypted link is first up this boot, for a heartbeat asleep on battery.
pub async fn proven_changed() {
    if !proven() {
        PROVEN_NOW.wait().await;
    }
}

struct OutSink;

impl Sink for OutSink {
    fn free(&self) -> usize {
        OUT.free_capacity()
    }

    fn write(&mut self, bytes: &[u8]) -> usize {
        OUT.try_write(bytes).unwrap_or(0)
    }
}

/// Empties [`OUT`] and forgets the line in hand; `cut`: the central was left inside a line.
fn clear_out(cut: bool) {
    LINES.lock(|lines| {
        OUT.clear();
        if let Ok(mut l) = lines.try_borrow_mut() {
            l.clear(cut);
        }
    });
}

/// Console output, mirrored to a subscribed central over an encrypted link. Never blocks:
/// a line that does not fit is dropped whole. The lock spans the room check and the write;
/// a fault handler that interrupts it finds the lines borrowed and mirrors nothing.
pub fn tee(bytes: &[u8]) {
    if !MIRROR.load(Ordering::Relaxed) {
        return;
    }
    let lost = LINES.lock(|lines| lines.try_borrow_mut().map_or(0, |mut l| l.feed(bytes, &mut OutSink)));
    if lost > 0 {
        LINES_DROPPED.fetch_add(lost, Ordering::Relaxed);
    }
    if !OUT.is_empty() {
        WAKE.signal(());
    }
}

fn power() -> Power {
    match PROFILE.load(Ordering::Relaxed) {
        1 => Power::Cable,
        2 => Power::Battery,
        _ if crate::power::on_battery() => Power::Battery,
        _ => Power::Cable,
    }
}

/// `ble`, `ble on|off|version|yes|no|bonds|forget [address]|disconnect|profile auto|cable|battery|interval now <min> <max>|scan [s [interval window]]`.
pub fn command(args: &[u8], uart: Uart) {
    let request = match args {
        b"" => return write_status(uart),
        b"on" => Request::On,
        b"off" => Request::Off,
        b"version" => Request::Version,
        b"yes" => Request::Yes,
        b"no" => Request::No,
        b"bonds" => Request::Bonds,
        b"forget" => Request::Forget,
        rest if rest.starts_with(b"forget ") => {
            match core::str::from_utf8(&rest[7..]).ok().and_then(bluenrg2::parse_address) {
                Some(addr) => Request::ForgetOne(addr),
                None => return uart.write_line("[ble] usage: ble forget [<address>, as `ble bonds` lists it]"),
            }
        }
        b"disconnect" => Request::Disconnect,
        #[cfg(feature = "ble-probe")]
        b"probe" => Request::Probe,
        #[cfg(feature = "ble-probe")]
        b"probe gatt services" => Request::ProbeGatt(probe::Gatt::Services),
        #[cfg(feature = "ble-probe")]
        b"probe gatt records" => Request::ProbeGatt(probe::Gatt::Records),
        #[cfg(feature = "ble-probe")]
        b"probe gatt values" => Request::ProbeGatt(probe::Gatt::Values),
        #[cfg(feature = "ble-probe")]
        b"restart" => Request::Restart(false),
        #[cfg(feature = "ble-probe")]
        b"restart updater" => Request::Restart(true),
        #[cfg(feature = "ble-probe")]
        b"probe adv" => Request::ProbeAdvertising,
        #[cfg(feature = "ble-probe")]
        rest if rest.starts_with(b"probe drop ") => {
            match core::str::from_utf8(&rest[11..]).ok().and_then(|t| u8::from_str_radix(t.trim(), 16).ok()) {
                Some(reason) => Request::ProbeDrop(reason),
                None => return uart.write_line("[ble] usage: ble probe drop <reason, hex>"),
            }
        }
        rest if rest.starts_with(b"profile") => {
            let (code, word) = match &rest[7..] {
                b" auto" => (0, "follows the power source"),
                b" cable" => (1, "the cable's, whatever the power source"),
                b" battery" => (2, "the battery's, whatever the power source"),
                _ => return uart.write_line("[ble] usage: ble profile auto|cable|battery"),
            };
            PROFILE.store(code, Ordering::Relaxed);
            WAKE.signal(());
            uart.write_str("[ble] timing: ");
            return uart.write_line(word);
        }
        rest if rest.starts_with(b"interval now") => {
            let text = core::str::from_utf8(&rest[12..]).unwrap_or("");
            let mut units = text.split_whitespace().map(|w| w.parse::<u16>().ok());
            match (units.next(), units.next()) {
                (Some(Some(min)), Some(Some(max))) if wrist_wake::ble::interval_now(min, max) => {
                    Request::IntervalNow(min, max)
                }
                _ => return uart.write_line("[ble] usage: ble interval now <min> <max> (1.25 ms units, 6 to 1999)"),
            }
        }
        rest if rest == b"scan" || rest.starts_with(b"scan ") => match scan::parse(&rest[4..]) {
            Some(request) => request,
            None => return uart.write_line(scan::USAGE),
        },
        _ => {
            return uart.write_line(
                "[ble] usage: ble [on|off|version|yes|no|bonds|forget [address]|disconnect|profile auto|cable|battery|interval now <min> <max>|scan [s [interval window]]]",
            )
        }
    };
    if REQUESTS.try_send(request).is_err() {
        uart.write_line("[ble] not queued: four already waiting");
    }
}

pub fn write_status(uart: Uart) {
    uart.write_str("[ble] ");
    uart.write_str(match STATE.load(Ordering::Relaxed) {
        STATE_OFF => "off (chip in reset)",
        STATE_STARTING => "starting",
        STATE_IDLE => "up, not advertising",
        STATE_ADVERTISING => "advertising as UNA",
        STATE_CONNECTED => "connected",
        _ => "failed to start (see the log; `ble on` retries)",
    });
    if STATE.load(Ordering::Relaxed) == STATE_CONNECTED {
        uart.write_str(if ENCRYPTED.load(Ordering::Relaxed) { ", encrypted" } else { ", NOT encrypted" });
        uart.write_str(", interval ");
        write_interval(uart, INTERVAL.load(Ordering::Relaxed));
        uart.write_str(" latency ");
        uart.write_u32(u32::from(LATENCY.load(Ordering::Relaxed)));
    }
    uart.write_str(", timing ");
    uart.write_str(match (PROFILE.load(Ordering::Relaxed), power()) {
        (0, Power::Cable) => "cable",
        (0, Power::Battery) => "battery",
        (_, Power::Cable) => "cable (forced)",
        (_, Power::Battery) => "battery (forced)",
    });
    uart.write_str(", address ");
    write_address(uart, bluenrg2::Address { kind: AddrKind::RANDOM, bytes: ADDRESS.each_ref().map(|b| b.load(Ordering::Relaxed)) });
    uart.write_str(", bonded ");
    uart.write_u32(u32::from(BONDED.load(Ordering::Relaxed)));
    let n = |label: &str, v: &AtomicU32| {
        uart.write_str(label);
        uart.write_u32(v.load(Ordering::Relaxed));
    };
    n(", events ", &TRANSPORT.reads);
    n(", cut short ", &TRANSPORT.cut_short);
    n(", fragments ", &FRAGMENTS);
    n(", repeats ", &REPEATS);
    n(", mirror lines dropped ", &LINES_DROPPED);
    n(", unsecured writes dropped ", &UNSECURED);
    n(", irq stuck after read ", &TRANSPORT.irq_stuck_read);
    n(" write ", &TRANSPORT.irq_stuck_write);
    n(", connect failures ", &CONNECT_FAILURES);
    n(", update failures ", &UPDATE_FAILURES);
    if UPDATE_FAILURES.load(Ordering::Relaxed) > 0 {
        uart.write_str(" (last status ");
        uart.write_hex8(LAST_UPDATE_FAILURE.load(Ordering::Relaxed));
        uart.write_str(")");
    }
    n(", updates refused ", &UPDATES_REFUSED);
    n(", disconnect failures ", &DISCONNECT_FAILURES);
    n(", encryption failures ", &ENCRYPTION_FAILURES);
    n(", unknown events ", &UNKNOWN);
    n(", chip restarts ", &RESTARTS);
    if RESTARTS.load(Ordering::Relaxed) > 0 {
        uart.write_str(" (last reason ");
        uart.write_hex8(LAST_RESTART.load(Ordering::Relaxed));
        uart.write_str(")");
    }
    match LAST_PAIRING.load(Ordering::Relaxed) {
        0 => {}
        1 => uart.write_str(", last pairing succeeded"),
        s => {
            uart.write_str(", last pairing failed: status ");
            uart.write_hex8(s - 1);
            uart.write_str(" reason ");
            uart.write_hex8(LAST_PAIRING_REASON.load(Ordering::Relaxed));
        }
    }
    if let Some((address, n)) = OLD_KEY.lock(Cell::get).central() {
        uart.write_str("; refused for an old key: ");
        write_address(uart, address);
        uart.write_str(", ");
        uart.write_u32(n);
        uart.write_str(if n == 1 { " time" } else { " times" });
        uart.write_str("; forget ");
        uart.write_bytes(DEVICE_NAME);
        uart.write_str(" on it, then pair again");
    }
    let Some((pairing, value)) = waiting() else { return uart.write_line("") };
    uart.write_str(match pairing {
        Pairing::Passkey => "; TYPE this on the central: ",
        Pairing::Compare => "; COMPARE with the central's number, then `ble yes` or `ble no`: ",
    });
    write_six_digits(uart, value);
    uart.write_line("");
}

/// What the Bluetooth page shows.
pub fn page() -> wrist_glass::pages::Bluetooth {
    use wrist_glass::pages::{Bluetooth, Prompt as Digits, Radio as Shown};
    let radio = match Radio::from_u8(STATE.load(Ordering::Relaxed)) {
        Radio::Off => Shown::Off,
        Radio::Starting => Shown::Starting,
        Radio::Idle => Shown::On,
        Radio::Advertising => Shown::Discoverable,
        Radio::Connected if ENCRYPTED.load(Ordering::Relaxed) => Shown::Secure,
        Radio::Connected => Shown::Connected,
        Radio::Failed => Shown::Failed,
    };
    let prompt = waiting().map(|(pairing, value)| match pairing {
        Pairing::Passkey => Digits::Passkey(value),
        Pairing::Compare => Digits::Compare(value),
    });
    let old_key = OLD_KEY.lock(Cell::get).central().is_some();
    Bluetooth { radio, prompt, bonded: BONDED_READ.load(Ordering::Relaxed).then(|| BONDED.load(Ordering::Relaxed)), old_key }
}

/// A central refused for a key the watch has lost.
pub fn has_old_key() -> bool {
    OLD_KEY.lock(Cell::get).central().is_some()
}

/// `f` applied to [`OLD_KEY`]; the face and the page redrawn when the notice comes or goes.
fn old_key(f: impl FnOnce(&mut OldKey<bluenrg2::Address>)) {
    let changed = OLD_KEY.lock(|c| {
        let mut key = c.get();
        let before = key.central().is_some();
        f(&mut key);
        c.set(key);
        before != key.central().is_some()
    });
    if changed {
        crate::redraw(wrist_pages::Page::Face);
        redraw();
    }
}

/// The pairing whose digits the page shows.
pub fn pairing() -> Option<Pairing> {
    waiting().map(|(pairing, _)| pairing)
}

/// When the page's digits stop waiting, if they are.
pub fn pairing_ends() -> Option<Instant> {
    PROMPT.lock(Cell::get).ends_ms(Instant::now().as_millis()).map(Instant::from_millis)
}

fn waiting() -> Option<(Pairing, u32)> {
    PROMPT.lock(Cell::get).waiting(Instant::now().as_millis())
}

/// The radio off if it is on or starting, on otherwise.
pub fn toggle() {
    let off = matches!(Radio::from_u8(STATE.load(Ordering::Relaxed)), Radio::Off | Radio::Failed);
    let _ = REQUESTS.try_send(if off { Request::On } else { Request::Off });
}

/// The wearer's answer to the numeric comparison waiting, as `ble yes|no`, taken at once.
pub fn answer(yes: bool) {
    if answer_prompt(yes) {
        WAKE.signal(());
    }
}

fn answer_prompt(yes: bool) -> bool {
    PROMPT.lock(|p| {
        let mut prompt = p.get();
        let answered = prompt.answer(yes, Instant::now().as_millis());
        p.set(prompt);
        answered
    })
}

/// A command's reply: the status byte and the parameters after it.
struct Reply {
    status: u8,
    params: [u8; RETURN_PARAMS_MAX],
    len: usize,
}

#[embassy_executor::task]
pub async fn run(mut pins: Pins, flash: &'static FlashMutex) -> ! {
    let uart = Uart::best_effort();
    // Held in reset from here on, except while the radio is on.
    let mut rst = Output::new(pins.rst, Level::Low, Speed::Low);
    let mut changes = crate::power::changes();
    let mut on = crate::rtc::radio_wanted();
    if on {
        uart.write_line("[ble] on at boot: `ble on` was the last word (BKP16R)");
    }
    loop {
        while !on {
            match REQUESTS.receive().await {
                Request::On => on = true,
                Request::Version
                | Request::Bonds
                | Request::Forget
                | Request::ForgetOne(..)
                | Request::Disconnect
                | Request::IntervalNow(..)
                | Request::Scan(..) => {
                    uart.write_line("[ble] off; `ble on` starts it");
                }
                #[cfg(feature = "ble-probe")]
                Request::Probe | Request::ProbeGatt(_) | Request::Restart(_) | Request::ProbeAdvertising | Request::ProbeDrop(_) => {
                    uart.write_line("[ble] off; `ble on` starts it");
                }
                Request::Off => {
                    set_state(STATE_OFF);
                    crate::rtc::set_radio_wanted(false);
                    uart.write_line("[ble] already off");
                }
                Request::Yes | Request::No => uart.write_line("[ble] already off"),
            }
        }
        if !crate::modules::allowed(crate::modules::Slot::Link1) {
            uart.write_line("[ble] not started: link1 holds another module (`modules force link1` allows it)");
            on = false;
            continue;
        }
        crate::rtc::set_radio_wanted(true);
        let mut link = Link {
            chip: bus(
                (pins.spi.reborrow(), pins.sck.reborrow(), pins.miso.reborrow(), pins.mosi.reborrow()),
                (pins.cs.reborrow(), pins.irq.reborrow(), pins.irq_exti.reborrow()),
                (pins.tx_dma.reborrow(), pins.rx_dma.reborrow()),
            ),
            rst: &mut rst,
            uart,
            flash,
            last_rx: 0,
            waiting_for: None,
            reply: None,
            readvertise: false,
            passkey_for: None,
            rebond_for: None,
            recount_bonds: false,
            nus: None,
            line: [0; 192],
            line_len: 0,
            mtu: DEFAULT_MTU,
            asked: None,
            advertised: None,
            request_interval: None,
            subscribed: false,
            peer: None,
            transfer: None,
            last_data: Instant::from_ticks(0),
            finish_transfer: false,
            console_at: Instant::from_ticks(0),
            outbox: Outbox::default(),
            pair_ask: PairAsk::default(),
            interval_ask: IntervalAsk::default(),
            scanner: scan::Scanner::default(),
            seen: Seen::default(),
            booting: false,
            boot: None,
            restart: None,
            restarts: Restarts::default(),
        };
        // A start, or a restart's recovery, that failed.
        if !(link.start().await && link.serve(&mut changes).await) {
            set_state(STATE_FAILED);
            old_key(OldKey::clear);
            link.rst.set_low();
            uart.write_line("[ble] start failed: chip held in reset; `ble on` retries");
            on = false;
            continue;
        }
        // `ble off`: the chip in reset first, then SPI1, CS and IRQ dropped back to analog.
        link.rst.set_low();
        drop(link);
        MIRROR.store(false, Ordering::Relaxed);
        set_encrypted(false);
        set_state(STATE_OFF);
        clear_prompt();
        old_key(OldKey::clear);
        crate::rtc::set_radio_wanted(false);
        uart.write_line("[ble] off: chip held in reset");
        on = false;
    }
}

type Bus<'d> = Transport<'static, Spi<'d, Async, spi::mode::Master>, Output<'d>, ExtiInput<'d, Async>, Delay>;

/// SPI1, CS and IRQ while the radio is on.
fn bus<'d>(
    (spi, sck, miso, mosi): (Peri<'d, peripherals::SPI1>, Peri<'d, peripherals::PG2>, Peri<'d, peripherals::PG3>, Peri<'d, peripherals::PB5>),
    (cs, irq, irq_exti): (Peri<'d, peripherals::PA15>, Peri<'d, peripherals::PA9>, Peri<'d, peripherals::EXTI9>),
    (tx_dma, rx_dma): (Peri<'d, peripherals::GPDMA1_CH2>, Peri<'d, peripherals::GPDMA1_CH3>),
) -> Bus<'d> {
    let mut config = spi::Config::default();
    config.frequency = Hertz(1_000_000);
    // ST's bus setup for the BlueNRG-2 in X-CUBE-BLE2.
    config.mode = spi::MODE_1;
    Transport::new(
        Spi::new(spi, sck, mosi, miso, tx_dma, rx_dma, Irqs, config),
        Output::new(cs, Level::High, Speed::VeryHigh),
        ExtiInput::new(irq, irq_exti, Pull::Down, Irqs),
        Delay,
        &TRANSPORT,
    )
}

// Each flag is one piece of work an event left for the loop.
#[allow(clippy::struct_excessive_bools)]
struct Link<'d> {
    chip: Bus<'d>,
    rst: &'d mut Output<'static>,
    uart: Uart,
    flash: &'static FlashMutex,
    /// CRC of the last console write, to spot one delivered twice.
    last_rx: u32,
    /// The opcode whose reply [`Link::command`] is waiting for, and the reply.
    waiting_for: Option<u16>,
    reply: Option<Reply>,
    /// Deferred work raised by events, done from the main loop.
    readvertise: bool,
    passkey_for: Option<u16>,
    rebond_for: Option<u16>,
    recount_bonds: bool,
    /// The console service: service, RX and TX characteristic handles.
    nus: Option<(u16, u16, u16)>,
    line: [u8; 192],
    line_len: usize,
    /// The negotiated ATT MTU; notifications carry `mtu - 3`.
    mtu: usize,
    /// The timing last asked of this connection, and the one advertising runs at.
    asked: Option<Timing>,
    advertised: Option<Timing>,
    /// `ble interval now`: ask for this range (1.25 ms units) once, whatever the timing.
    request_interval: Option<(u16, u16)>,
    /// The central wrote TX's CCCD on: the mirror runs while the link is encrypted too.
    subscribed: bool,
    /// The connected central's address, as the link gave it.
    peer: Option<bluenrg2::Address>,
    /// An image arriving through RX, and when its last bytes came.
    transfer: Option<crate::update::Session>,
    last_data: Instant,
    finish_transfer: bool,
    /// The next console notification may go at this instant ([`CONSOLE_PACE`]).
    console_at: Instant,
    outbox: Outbox<NUS_VALUE_LEN>,
    pair_ask: PairAsk,
    interval_ask: IntervalAsk,
    scanner: scan::Scanner,
    seen: Seen,
    /// A reset through the pin is waiting for the chip's boot, and how that boot came up.
    booting: bool,
    boot: Option<Boot>,
    /// The chip restarted by itself and has not been brought up again.
    restart: Option<Boot>,
    restarts: Restarts,
}

/// Codes of unknown events already named on the console: each is named once.
#[derive(Default)]
struct Seen {
    codes: [u16; 16],
    len: usize,
}

impl Seen {
    /// True the first time `code` comes; once full, never.
    fn first(&mut self, code: u16) -> bool {
        if self.codes[..self.len].contains(&code) || self.len == self.codes.len() {
            return false;
        }
        self.codes[self.len] = code;
        self.len += 1;
        true
    }
}

impl Link<'_> {
    /// Until `ble off` (true), or a restart the chip was not brought back from (false). Each pass
    /// does what is due, then sleeps until the chip's IRQ, a request, [`WAKE`], a power change or
    /// the earliest deadline.
    async fn serve(&mut self, changes: &mut crate::power::Changes) -> bool {
        loop {
            if self.restart.is_some() && !self.recover().await {
                return false;
            }
            WAKE.reset();
            self.deferred().await;
            self.scan_tick().await;
            self.retime().await;
            self.console_tick().await;
            let due = self.due();
            let timer = async {
                match due {
                    Some(at) => Timer::at(at).await,
                    None => core::future::pending::<()>().await,
                }
            };
            let power_changed = async {
                match changes {
                    Some(rx) => {
                        rx.changed().await;
                    }
                    None => core::future::pending::<()>().await,
                }
            };
            match select4(REQUESTS.receive(), self.chip.wait_pending(), timer, select(WAKE.wait(), power_changed)).await {
                Either4::First(Request::Off) => {
                    if let Some(session) = self.transfer.take() {
                        crate::update::abort(session);
                    }
                    let _ = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await;
                    return true;
                }
                Either4::First(req) => self.request(req).await,
                Either4::Second(()) => {
                    self.drain().await;
                    // IRQ stays high through unreadable headers, so the next wait returns at once.
                    if self.chip.backing_off() {
                        Timer::after_millis(500).await;
                    }
                }
                Either4::Third(()) | Either4::Fourth(_) => {}
            }
        }
    }

    /// When the loop must next run by time (`wrist_wake::ble::due`); `None` when only an
    /// event can give it work.
    fn due(&self) -> Option<Instant> {
        use wrist_wake::ble::{Console, State, Transfer};
        let tick = |i: Instant| i.as_ticks();
        let one = Duration::from_ticks(1);
        let state = State {
            now: tick(Instant::now()),
            work: self.request_interval.is_some()
                || self.finish_transfer
                || self.rebond_for.is_some()
                || self.recount_bonds
                || self.passkey_for.is_some()
                || self.readvertise,
            confirm_by: PROMPT.lock(Cell::get).owed().map(|(at, _)| tick(Instant::from_millis(at))),
            transfer: match &self.transfer {
                None => Transfer::None,
                Some(s) if s.started() => Transfer::Started,
                Some(_) => Transfer::Claimed,
            },
            console: (self.nus.is_some() && MIRROR.load(Ordering::Relaxed))
                .then(|| Console { pending: !OUT.is_empty() || self.outbox.holding(), next_at: tick(self.console_at) }),
            abandon_at: self.transfer.as_ref().map(|_| tick(self.last_data + TRANSFER_IDLE + one)),
            ask_at: self.pair_ask.due(),
            scan_end: self.scanner.until().map(tick),
        };
        wrist_wake::ble::due(&state).map(Instant::from_ticks)
    }

    async fn request(&mut self, req: Request) {
        match req {
            Request::On => self.uart.write_line("[ble] already on"),
            Request::Off => {}
            Request::Version => self.version().await,
            Request::Yes | Request::No => {
                if !answer_prompt(req == Request::Yes) {
                    self.uart.write_line("[ble] no comparison is waiting");
                }
            }
            Request::Bonds => self.list_bonds(false).await,
            Request::Forget => {
                if self.command(ACI_GAP_CLEAR_SECURITY_DB, &[]).await.is_ok() {
                    BONDED.store(0, Ordering::Relaxed);
                    redraw();
                    self.uart.write_line("[ble] bond database cleared; forget the watch on the central too");
                    self.end_forgotten_link().await;
                }
            }
            Request::ForgetOne(addr) => self.forget_one(addr).await,
            Request::Disconnect => self.terminate().await,
            Request::IntervalNow(min, max) => {
                if STATE.load(Ordering::Relaxed) == STATE_CONNECTED {
                    self.request_interval = Some((min, max));
                } else {
                    self.uart.write_line("[ble] interval now: not connected");
                }
            }
            Request::Scan(seconds, duty) => self.scan_request(seconds, duty).await,
            #[cfg(feature = "ble-probe")]
            Request::Probe => self.probe().await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeGatt(what) => self.probe_gatt(what).await,
            #[cfg(feature = "ble-probe")]
            Request::Restart(updater) => self.restart_chip(updater).await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeAdvertising => self.probe_advertising().await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeDrop(reason) => self.probe_drop(reason).await,
        }
    }

    async fn version(&mut self) {
        if let Ok(r) = self.command(HCI_READ_LOCAL_VERSION, &[]).await {
            if r.len >= 8 {
                self.uart.write_str("[ble] HCI version ");
                self.uart.write_u32(u32::from(r.params[0]));
                self.uart.write_str(", LMP version ");
                self.uart.write_u32(u32::from(r.params[3]));
                self.uart.write_str(", manufacturer 0x");
                self.uart.write_hex8(r.params[5]);
                self.uart.write_hex8(r.params[4]);
                self.uart.write_line(" (0030 = ST)");
            }
        }
        if let Ok(r) = self.command(ACI_HAL_GET_FW_BUILD_NUMBER, &[]).await {
            if r.len >= 2 {
                self.uart.write_str("[ble] firmware build ");
                self.uart.write_u32(u32::from(u16::from_le_bytes([r.params[0], r.params[1]])));
                self.uart.write_line("");
            }
        }
    }

    /// The timing the power source asks for, applied where it changed: advertising again at
    /// the new interval, or the connection asked for new parameters.
    async fn retime(&mut self) {
        let want = timing(power(), self.transfer.is_some());
        match STATE.load(Ordering::Relaxed) {
            STATE_ADVERTISING if self.advertised != Some(want) => {
                if self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await.is_ok() {
                    self.advertise().await;
                }
            }
            STATE_CONNECTED if self.asked != Some(want) && self.interval_ask.open() => {
                self.asked = Some(want);
                self.ask_connection(want.interval_min, want.interval_max, want.latency, want.timeout).await;
            }
            _ => {}
        }
    }

    async fn ask_connection(&mut self, min: u16, max: u16, latency: u16, timeout: u16) {
        let mut p = [0u8; 10];
        p[0..2].copy_from_slice(&CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes());
        p[2..4].copy_from_slice(&min.to_le_bytes());
        p[4..6].copy_from_slice(&max.to_le_bytes());
        p[6..8].copy_from_slice(&latency.to_le_bytes());
        p[8..10].copy_from_slice(&timeout.to_le_bytes());
        match self.command(ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ, &p).await {
            Ok(r) if r.status == 0 => {}
            Ok(r) => {
                self.uart.write_str("[ble] interval request refused, status ");
                self.uart.write_hex8(r.status);
                self.uart.write_line("");
            }
            Err(why) => {
                self.uart.write_str("[ble] interval request: ");
                self.uart.write_line(why);
            }
        }
    }

    /// Work raised by events: a finished transfer, a rebond, a passkey, a comparison's answer
    /// (or its rejection after the timeout), a quiet transfer abandoned, an unencrypted central
    /// asked to pair, re-advertising after a disconnect.
    async fn deferred(&mut self) {
        if self.pair_ask.take(Instant::now().as_ticks()) {
            self.ask_to_pair().await;
        }
        if let Some((min, max)) = self.request_interval.take() {
            self.ask_connection(min, max, wrist_wake::ble::NOW_LATENCY, wrist_wake::ble::NOW_TIMEOUT).await;
        }
        if self.finish_transfer {
            self.finish_transfer = false;
            if let Some(session) = self.transfer.take() {
                // What queued while the mirror waited is stale; the result line must find room.
                clear_out(self.outbox.discard());
                if let Err(why) = crate::update::finish_app(self.flash, session).await {
                    self.uart.write_str("[update] error: ");
                    self.uart.write_line(why);
                }
            }
        }
        if self.transfer.is_some() && self.last_data.elapsed() > TRANSFER_IDLE {
            if let Some(session) = self.transfer.take() {
                crate::update::abort(session);
            }
            self.uart.write_line("[update] error: no data for 5 s; transfer abandoned");
        }
        if let Some(h) = self.rebond_for.take() {
            let _ = self.command(ACI_GAP_ALLOW_REBOND, &h.to_le_bytes()).await;
            self.uart.write_line("[ble] the central lost its bond; allowing it to pair again");
        }
        if core::mem::take(&mut self.recount_bonds) {
            self.list_bonds(true).await;
        }
        if let Some(h) = self.passkey_for.take() {
            self.answer_passkey(h).await;
        }
        let now = Instant::now().as_millis();
        let (prompt, owed) = PROMPT.lock(|p| {
            let prompt = p.get();
            let owed = prompt.owed().filter(|&(at, _)| at <= now).map(|(_, yes)| yes);
            if owed.is_some() {
                p.set(Prompt::None);
            }
            (prompt, owed)
        });
        if let Some(yes) = owed {
            let hb = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
            let _ = self.command(ACI_GAP_NUMERIC_COMPARISON_CONFIRM, &[hb[0], hb[1], u8::from(yes)]).await;
            self.uart.write_line(if yes {
                "[ble] comparison confirmed"
            } else if prompt == Prompt::Answered(false) {
                "[ble] comparison rejected"
            } else {
                "[ble] comparison not answered in 60 s: rejected"
            });
        }
        if self.readvertise {
            self.readvertise = false;
            self.advertise().await;
        }
    }

    async fn ask_to_pair(&mut self) {
        if STATE.load(Ordering::Relaxed) != STATE_CONNECTED || ENCRYPTED.load(Ordering::Relaxed) {
            return;
        }
        let h = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
        match self.command(ACI_GAP_SLAVE_SECURITY_REQ, &h).await {
            Ok(r) if r.status == 0 => self.uart.write_line("[ble] link still unencrypted: asked the central to pair"),
            Ok(r) => {
                self.uart.write_str("[ble] security request refused, status ");
                self.uart.write_hex8(r.status);
                self.uart.write_line("");
            }
            Err(why) => {
                self.uart.write_str("[ble] security request: ");
                self.uart.write_line(why);
            }
        }
    }

    /// Bring the chip up, and again if it restarts meanwhile; whether it is up.
    async fn start(&mut self) -> bool {
        self.bring_up().await;
        self.recover().await
    }

    /// Bring up again a chip that restarted by itself, within the budget; whether it is up.
    async fn recover(&mut self) -> bool {
        while let Some(boot) = self.restart.take() {
            if boot == Boot::KeptRestarting {
                self.uart.write_line("[ble] the chip restarted more than 3 times in an hour");
                return false;
            }
            self.bring_up().await;
        }
        !Radio::from_u8(STATE.load(Ordering::Relaxed)).start_failed()
    }

    /// Reset, address, TX power, event mask, GATT, GAP, name, IO capability, security
    /// requirements, advertise: ST's sample order.
    async fn bring_up(&mut self) {
        set_state(STATE_STARTING);
        clear_prompt();
        self.reset().await;
        if self.boot == Some(Boot::InUpdater) {
            return self.fail("reset: the chip came up in its updater");
        }
        let addr = address_from_uid();
        for (slot, b) in ADDRESS.iter().zip(addr.iter()) {
            slot.store(*b, Ordering::Relaxed);
        }
        let mut random = [0u8; 8];
        random[0] = config::STATIC_RANDOM_ADDRESS;
        random[1] = u8::try_from(addr.len()).unwrap_or(0);
        random[2..].copy_from_slice(&addr);
        let mut public = random;
        public[0] = config::PUBLIC_ADDRESS;
        let early: [(&str, u16, &[u8]); 5] = [
            ("static random address", ACI_HAL_WRITE_CONFIG_DATA, &random),
            // Pairing failed at the `DHKey` check until this slot had been written (HARDWARE.md).
            ("public address slot", ACI_HAL_WRITE_CONFIG_DATA, &public),
            ("TX power", ACI_HAL_SET_TX_POWER_LEVEL, &TX_POWER),
            ("event mask", HCI_SET_EVENT_MASK, &EVENT_MASK),
            ("GATT init", ACI_GATT_INIT, &[]),
        ];
        for (what, opcode, params) in early {
            if !self.step(what, opcode, params).await {
                return;
            }
        }
        // Peripheral alone if the chip refuses both roles, so updates never depend on the
        // central role.
        let mut handles = None;
        for roles in [gap_role::PERIPHERAL | gap_role::CENTRAL, gap_role::PERIPHERAL] {
            match self.command(ACI_GAP_INIT, &[roles, privacy::DISABLED, GAP_NAME_LEN]).await {
                Ok(r) if r.status == 0 && r.len >= 6 => {
                    let service = u16::from_le_bytes([r.params[0], r.params[1]]);
                    let name_char = u16::from_le_bytes([r.params[2], r.params[3]]);
                    handles = Some((service, name_char, roles & gap_role::CENTRAL != 0));
                    break;
                }
                Ok(r) => {
                    self.uart.write_str("[ble] GAP init refused roles ");
                    self.uart.write_hex8(roles);
                    self.uart.write_str(", status ");
                    self.uart.write_hex8(r.status);
                    self.uart.write_line("");
                }
                Err(why) => {
                    self.uart.write_str("[ble] GAP init: ");
                    self.uart.write_line(why);
                }
            }
        }
        let Some((service, name_char, central)) = handles else {
            return self.fail("GAP init");
        };
        self.scanner.started(central);
        let mut name = [0u8; 16];
        name[0..2].copy_from_slice(&service.to_le_bytes());
        name[2..4].copy_from_slice(&name_char.to_le_bytes());
        name[4] = 0;
        name[5] = u8::try_from(DEVICE_NAME.len()).unwrap_or(0);
        name[6..6 + DEVICE_NAME.len()].copy_from_slice(DEVICE_NAME);
        let late: [(&str, u16, &[u8]); 3] = [
            ("device name", ACI_GATT_UPDATE_CHAR_VALUE, &name[..6 + DEVICE_NAME.len()]),
            ("IO capability", ACI_GAP_SET_IO_CAPABILITY, &[IO_CAP_DISPLAY_YES_NO]),
            ("security requirements", ACI_GAP_SET_AUTHENTICATION_REQUIREMENT, &wrist_bluenrg2::bonds::PAIRING.params()),
        ];
        for (what, opcode, params) in late {
            if !self.step(what, opcode, params).await {
                return;
            }
        }
        if !self.add_services().await {
            return;
        }
        self.uart.write_str("[ble] up: ST stack, address ");
        write_address(self.uart, bluenrg2::Address { kind: AddrKind::RANDOM, bytes: addr });
        self.uart.write_line(if central { ", peripheral and central" } else { ", peripheral only" });
        self.list_bonds(false).await;
        self.advertise().await;
    }

    /// Device Information (manufacturer, model, the build) and the console (RX, TX).
    async fn add_services(&mut self) -> bool {
        let Some(dis) = self.add_service(UUID_DEVICE_INFORMATION, 7).await else {
            return false;
        };
        let strings: [(u16, &[u8]); 3] = [
            (UUID_MANUFACTURER_NAME, MANUFACTURER),
            (UUID_MODEL_NUMBER, MODEL),
            (UUID_FIRMWARE_REVISION, crate::version::BUILD_ID.as_bytes()),
        ];
        for (uuid, value) in strings {
            let n = value.len().min(32);
            let Some(ch) = self.add_char(dis, uuid, n).await else {
                return false;
            };
            if self.update_value(dis, ch, &value[..n]).await != 0 {
                return false;
            }
        }
        let Some(nus) = self.add_service_128(NUS_SERVICE, 6).await else {
            self.fail("adding the console service");
            return false;
        };
        // Write alone is not enough: with it, a bonded central's writes were neither answered nor
        // reported (installs 140 to 143). ST's stack checks `secured` only on writes with
        // response; `attribute_modified` drops the rest unless the link is encrypted.
        let secured = permission::AUTHEN_WRITE | permission::ENCRY_WRITE;
        let (Some(rx), Some(tx)) = (
            self.add_char_128(nus, NUS_RX, props::WRITE | props::WRITE_WITHOUT_RESPONSE, secured, evt_mask::ATTRIBUTE_WRITE).await,
            self.add_char_128(nus, NUS_TX, props::NOTIFY, permission::NONE, evt_mask::NONE).await,
        ) else {
            self.fail("adding a console characteristic");
            return false;
        };
        self.nus = Some((nus, rx, tx));
        true
    }

    /// A primary service with the console's 128-bit UUID, byte 12 `last`.
    async fn add_service_128(&mut self, last: u8, max_records: u8) -> Option<u16> {
        let mut p = [0u8; 19];
        p[0] = uuid_type::U128;
        p[1..17].copy_from_slice(&NUS_BASE);
        p[13] = last;
        p[17] = service_type::PRIMARY;
        p[18] = max_records;
        match self.command(ACI_GATT_ADD_SERVICE, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => None,
        }
    }

    /// A variable-length characteristic with the console's 128-bit UUID, byte 12 `last`.
    async fn add_char_128(&mut self, service: u16, last: u8, props: u8, security: u8, evt_mask: u8) -> Option<u16> {
        let mut p = [0u8; 26];
        p[0..2].copy_from_slice(&service.to_le_bytes());
        p[2] = uuid_type::U128;
        p[3..19].copy_from_slice(&NUS_BASE);
        p[15] = last;
        p[19..21].copy_from_slice(&u16::try_from(NUS_VALUE_LEN).unwrap_or(0).to_le_bytes());
        p[21] = props;
        p[22] = security;
        p[23] = evt_mask;
        p[24] = enc_key_size::MIN;
        p[25] = value_length::VARIABLE;
        match self.command(ACI_GATT_ADD_CHAR, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => None,
        }
    }

    async fn add_service(&mut self, uuid: u16, max_records: u8) -> Option<u16> {
        let u = uuid.to_le_bytes();
        match self.command(ACI_GATT_ADD_SERVICE, &[uuid_type::U16, u[0], u[1], service_type::PRIMARY, max_records]).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => {
                self.fail("adding a service");
                None
            }
        }
    }

    /// A readable, variable-length characteristic with a 16-bit UUID.
    async fn add_char(&mut self, service: u16, uuid: u16, len: usize) -> Option<u16> {
        let sv = service.to_le_bytes();
        let u = uuid.to_le_bytes();
        let l = u16::try_from(len).unwrap_or(0).to_le_bytes();
        let p = [
            sv[0], sv[1], uuid_type::U16, u[0], u[1], l[0], l[1], props::READ, permission::NONE, evt_mask::NONE, enc_key_size::MIN,
            value_length::VARIABLE,
        ];
        match self.command(ACI_GATT_ADD_CHAR, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => {
                self.fail("adding a characteristic");
                None
            }
        }
    }

    /// The command's status; 0xFF if the chip did not answer.
    async fn update_value(&mut self, service: u16, ch: u16, value: &[u8]) -> u8 {
        let mut p = [0u8; 6 + NUS_VALUE_LEN];
        p[0..2].copy_from_slice(&service.to_le_bytes());
        p[2..4].copy_from_slice(&ch.to_le_bytes());
        p[4] = 0;
        let n = value.len().min(NUS_VALUE_LEN);
        p[5] = u8::try_from(n).unwrap_or(0);
        p[6..6 + n].copy_from_slice(&value[..n]);
        self.command(ACI_GATT_UPDATE_CHAR_VALUE, &p[..6 + n]).await.map_or(0xFF, |r| r.status)
    }

    /// Console output to a subscribed central, one notification per pass.
    async fn console_tick(&mut self) {
        let Some((nus, _rx, tx)) = self.nus else {
            return;
        };
        // While image bytes arrive the mirror waits: notifications refused for want of TX
        // buffers were followed by the chip reporting the central's writes twice.
        if !MIRROR.load(Ordering::Relaxed)
            || self.transfer.as_ref().is_some_and(crate::update::Session::started)
            || Instant::now() < self.console_at
        {
            return;
        }
        let most = (self.mtu.saturating_sub(ATT_HEADER_LEN)).clamp(DEFAULT_VALUE_LEN, NUS_VALUE_LEN);
        let (chunk, n) = self.outbox.next(most, |buf| OUT.try_read(buf).unwrap_or(0));
        if n == 0 {
            return;
        }
        let status = self.update_value(nus, tx, &chunk[..n]).await;
        self.console_at = Instant::now() + CONSOLE_PACE;
        if self.outbox.sent(chunk, n, status).is_err() {
            self.uart.write_line("[ble] a console notification was refused");
        }
    }

    /// The mirror runs while a central is subscribed over an encrypted link.
    fn set_mirror(&mut self) {
        let on = self.subscribed && ENCRYPTED.load(Ordering::Relaxed);
        if on != MIRROR.swap(on, Ordering::Relaxed) {
            clear_out(self.outbox.discard());
            self.uart.write_line(if on {
                "[ble] console attached over the air; 'help' lists commands"
            } else {
                "[ble] console detached"
            });
        }
    }

    /// Bytes the central wrote to RX: one command per line, run by the USB console's parser
    /// and answered through the mirror, or the next piece of an image.
    fn console_bytes(&mut self, data: &[u8]) {
        if let Some(session) = self.transfer.as_mut() {
            self.last_data = Instant::now();
            if crate::update::feed(session, data) {
                self.finish_transfer = true;
            }
            return;
        }
        for &b in data {
            match b {
                b'\r' | b'\n' => self.run_line(),
                _ if self.line_len < self.line.len() => {
                    self.line[self.line_len] = b;
                    self.line_len += 1;
                }
                _ => {}
            }
        }
        // A write with no newline is a whole command too: nRF Connect's write dialog sends
        // none, and a terminal sends one line per write.
        self.run_line();
    }

    fn run_line(&mut self) {
        if self.line_len == 0 {
            return;
        }
        let mut line = [0u8; 192];
        let n = self.line_len;
        line[..n].copy_from_slice(&self.line[..n]);
        self.line_len = 0;
        match crate::console::parse_transfer(&line[..n]) {
            Some((Target::Application, len, crc)) => self.start_transfer(len, crc),
            Some((Target::Loader, ..)) => self.uart.write_line("[loader] error: not over the air; USB only"),
            None => crate::console::command(&line[..n], self.flash),
        }
    }

    /// `update <len> <crc>` from the central: claim the staging buffer; the bytes that follow
    /// on RX feed it until complete.
    fn start_transfer(&mut self, len: usize, crc: u32) {
        let begun = crate::update::app_update_allowed(self.flash)
            .and_then(|()| crate::update::begin("[update]", wrist_flash::vectors::Target::Application, len, crc));
        match begun {
            Ok(session) => {
                self.transfer = Some(session);
                self.last_data = Instant::now();
            }
            Err(why) => {
                self.uart.write_str("[update] error: ");
                self.uart.write_line(why);
            }
        }
    }

    /// One start-up command; false (and the failure logged) if refused.
    async fn step(&mut self, what: &str, opcode: u16, params: &[u8]) -> bool {
        match self.command(opcode, params).await {
            Ok(r) if r.status == 0 => true,
            _ => {
                self.fail(what);
                false
            }
        }
    }

    fn fail(&mut self, step: &str) {
        set_state(STATE_FAILED);
        self.uart.write_str("[ble] start failed at ");
        self.uart.write_line(step);
    }

    /// `ACI_GAP_SET_DISCOVERABLE`: connectable undirected at the power source's interval,
    /// static random address, no filter, name `UNA`, no UUIDs, no interval hint.
    async fn advertise(&mut self) {
        let want = timing(power(), false);
        let mut p = [0u8; 20];
        p[0] = adv_type::ADV_IND;
        p[1..3].copy_from_slice(&want.advertise.to_le_bytes());
        p[3..5].copy_from_slice(&want.advertise.to_le_bytes());
        p[5] = AddrKind::RANDOM.as_raw();
        p[6] = adv_filter_policy::NO_FILTER;
        p[7] = u8::try_from(1 + DEVICE_NAME.len()).unwrap_or(0);
        p[8] = bluenrg2::ad::NAME_COMPLETE;
        p[9..9 + DEVICE_NAME.len()].copy_from_slice(DEVICE_NAME);
        let n = 9 + DEVICE_NAME.len();
        // Service UUID length 0, then slave connection interval min and max 0.
        p[n..n + 5].copy_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00]);
        match self.command(ACI_GAP_SET_DISCOVERABLE, &p[..n + 5]).await {
            Ok(r) if r.status == 0 => {
                set_state(STATE_ADVERTISING);
                self.advertised = Some(want);
                self.uart.write_str("[ble] advertising as UNA every ");
                self.uart.write_u32(u32::from(want.advertise) * 625 / 1000);
                self.uart.write_line(" ms");
            }
            Ok(r) => {
                // Refused while a scan runs: it resumes when the scan ends.
                if self.scanner.advertising_refused() {
                    set_state(STATE_IDLE);
                }
                self.uart.write_str("[ble] advertising refused, status ");
                self.uart.write_hex8(r.status);
                self.uart.write_line("");
            }
            Err(why) => {
                self.uart.write_str("[ble] advertising: ");
                self.uart.write_line(why);
            }
        }
    }

    async fn list_bonds(&mut self, after_pairing: bool) {
        if let Ok(r) = self.command(ACI_GAP_GET_BONDED_DEVICES, &[]).await {
            if r.len >= 1 {
                let params = &r.params[..r.len.min(r.params.len())];
                let before = BONDED_READ.load(Ordering::Relaxed).then(|| BONDED.load(Ordering::Relaxed));
                BONDED.store(params[0], Ordering::Relaxed);
                BONDED_READ.store(true, Ordering::Relaxed);
                redraw();
                self.uart.write_str("[ble] bonded devices: ");
                self.uart.write_u32(u32::from(params[0]));
                for bond in bluenrg2::bonded_devices(params) {
                    self.uart.write_str(" ");
                    write_address(self.uart, bond);
                }
                self.uart.write_line("");
                if after_pairing && self.peer.is_some_and(|peer| wrist_bluenrg2::bonds::bond_kept(params, before, peer)) {
                    self.uart.write_line("[ble] bonded");
                }
            }
        }
    }

    /// `ble forget <address>`, then the bonds left.
    async fn forget_one(&mut self, addr: bluenrg2::Address) {
        let Ok(r) = self.command(ACI_GAP_GET_BONDED_DEVICES, &[]).await else {
            return self.uart.write_line("[ble] forget: the bonded devices could not be read");
        };
        let params = &r.params[..r.len.min(r.params.len())];
        let found = wrist_bluenrg2::bonds::forget(params, addr);
        if found != wrist_bluenrg2::bonds::Forget::Bond {
            self.uart.write_str("[ble] forget: not bonded: ");
            write_address(self.uart, addr);
            if let wrist_bluenrg2::bonds::Forget::OtherType(bond) = found {
                self.uart.write_str("; bonded as ");
                write_address(self.uart, bond);
            }
            return self.uart.write_line("");
        }
        let keep = self.peer.is_some_and(|peer| wrist_bluenrg2::bonds::another_bond(params, addr, peer));
        let mut p = [0u8; 7];
        p[0] = addr.kind.as_raw();
        p[1..].copy_from_slice(&addr.bytes);
        match self.command(ACI_GAP_REMOVE_BONDED_DEVICE, &p).await {
            Ok(r) if r.status == 0 => {
                self.uart.write_str("[ble] forgot ");
                write_address(self.uart, addr);
                self.uart.write_line("; forget the watch on that central too");
                if !keep {
                    self.end_forgotten_link().await;
                }
                self.list_bonds(false).await;
            }
            Ok(r) => {
                self.uart.write_str("[ble] forget: the chip refused, status ");
                self.uart.write_hex8(r.status);
                self.uart.write_line(if r.status == 0x01 { " (its stack has no single-bond removal)" } else { "" });
            }
            Err(e) => {
                self.uart.write_str("[ble] forget: ");
                self.uart.write_line(e);
            }
        }
    }

    /// The link ended, if one is up: its encryption would otherwise keep the console and
    /// updates open to a central no longer bonded.
    async fn end_forgotten_link(&mut self) {
        if self.peer.is_some() {
            self.terminate().await;
            self.uart.write_line("[ble] link ended: not shown to be another bond's");
        }
    }

    async fn terminate(&mut self) {
        let h = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
        let _ = self.command(ACI_GAP_TERMINATE, &[h[0], h[1], bluenrg2::status::REMOTE_USER_TERMINATED]).await;
    }

    /// A random six-digit passkey from the controller's own generator, shown on the console
    /// for the central to type, and given to the stack.
    async fn answer_passkey(&mut self, handle: u16) {
        let key = match self.command(HCI_LE_RAND, &[]).await {
            Ok(r) if r.len >= 4 => u32::from_le_bytes([r.params[0], r.params[1], r.params[2], r.params[3]]) % 1_000_000,
            _ => {
                self.uart.write_line("[ble] passkey request but no random number; pairing will fail");
                return;
            }
        };
        let shown = PROMPT.lock(|p| {
            let mut prompt = p.get();
            let shown = prompt.passkey(key, Instant::now().as_millis());
            p.set(prompt);
            shown
        });
        if !shown {
            return self.uart.write_line("[ble] passkey ready after its pairing ended; not given");
        }
        crate::wake_display();
        self.uart.write_str("[ble] pairing: type this passkey on the central: ");
        write_six_digits(self.uart, key);
        self.uart.write_line("");
        let mut p = [0u8; 6];
        p[0..2].copy_from_slice(&handle.to_le_bytes());
        p[2..6].copy_from_slice(&key.to_le_bytes());
        let _ = self.command(ACI_GAP_PASS_KEY_RESP, &p).await;
    }

    async fn reset(&mut self) {
        #[cfg(feature = "ble-trace")]
        trace::packet(trace::Dir::Reset, &[]);
        self.chip.deselect();
        self.rst.set_low();
        Timer::after_millis(5).await;
        self.rst.set_high();
        self.waiting_for = None;
        self.reply = None;
        self.restart = None;
        self.boot = None;
        self.booting = true;
        CONN_HANDLE.store(0, Ordering::Relaxed);
        // The chip takes no command until its stack announces itself.
        if self.chip.wait_pending_within(micros(BOOT_TIMEOUT)).await {
            self.drain().await;
        } else {
            self.uart.write_line("[ble] no sign of life after reset (no module, or not ST's firmware)");
        }
        self.booting = false;
    }

    /// Send a command and wait for its Command Complete (or Command Status), handling every
    /// other event that arrives meanwhile.
    async fn command(&mut self, opcode: u16, params: &[u8]) -> Result<Reply, &'static str> {
        // A chip that restarted is reset before it hears anything: its updater, say, takes
        // commands of its own.
        if self.restart.is_some() {
            return Err("not sent: the chip restarted");
        }
        let mut packet = [0u8; MAX_COMMAND];
        let n = bluenrg2::command(opcode, &[params], &mut packet).ok_or("not sent: parameters past 255 bytes")?;
        #[cfg(feature = "ble-trace")]
        trace::packet(trace::Dir::ToChip, &packet[..n]);
        self.waiting_for = Some(opcode);
        self.reply = None;
        if !self.chip.write(&packet[..n]).await {
            self.waiting_for = None;
            return Err("the chip did not take the command");
        }
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while self.reply.is_none() && self.restart.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left == Duration::from_ticks(0) {
                break;
            }
            if !self.chip.wait_pending_within(micros(left)).await {
                break;
            }
            self.drain().await;
        }
        self.waiting_for = None;
        self.reply.take().ok_or("no reply within 1 s")
    }

    /// Every event waiting, one `0B` read each, while IRQ stays high.
    async fn drain(&mut self) {
        let mut rounds = 0;
        while self.chip.pending() && rounds < 8 {
            rounds += 1;
            let mut buf = [0u8; transport::MAX_READ];
            match self.chip.read(&mut buf).await {
                Read::Bytes(count) => {
                    for packet in transport::packets(&buf[..count], &TRANSPORT) {
                        self.handle(packet);
                    }
                }
                Read::Empty => continue,
                Read::Unreadable { header, first } => {
                    if first {
                        self.uart.write_str("[ble] headers unreadable with IRQ high, last ");
                        write_hex(self.uart, &header);
                        self.uart.write_line("; backing off to one read per 500 ms");
                    }
                    return;
                }
            }
            embassy_futures::yield_now().await;
        }
    }

    /// One packet from the chip: capture a reply, act on an event.
    fn handle(&mut self, p: &[u8]) {
        #[cfg(feature = "ble-trace")]
        trace::packet(trace::Dir::FromChip, p);
        match bluenrg2::parse(p) {
            Event::CommandComplete { cmd_opcode, status: Some(status), return_param_bytes: params } => {
                if self.waiting_for == Some(cmd_opcode) {
                    let mut reply = Reply { status: status.into_inner(), params: [0; RETURN_PARAMS_MAX], len: params.len().min(RETURN_PARAMS_MAX) };
                    reply.params[..reply.len].copy_from_slice(&params[..reply.len]);
                    self.reply = Some(reply);
                }
            }
            Event::CommandComplete { status: None, .. } => {}
            Event::CommandStatus { cmd_opcode, status } => {
                if self.waiting_for == Some(cmd_opcode) {
                    self.reply = Some(Reply { status: status.into_inner(), params: [0; RETURN_PARAMS_MAX], len: 0 });
                }
            }
            Event::LeConnectionComplete { handle, peer, timing, .. } => {
                CONN_HANDLE.store(handle.raw(), Ordering::Relaxed);
                self.peer = Some(peer);
                self.pair_ask.connected(Instant::now().as_ticks(), embassy_time::TICK_HZ);
                self.interval_ask.connected();
                if let Some(t) = timing {
                    INTERVAL.store(t.conn_interval, Ordering::Relaxed);
                    LATENCY.store(t.peripheral_latency, Ordering::Relaxed);
                }
                set_state(STATE_CONNECTED);
                set_encrypted(false);
                self.subscribed = false;
                self.set_mirror();
                self.outbox = Outbox::default();
                // No request here: one crossing the central's own encryption start ended a
                // bonded macOS central's link (HARDWARE.md).
                self.mtu = DEFAULT_MTU;
                self.asked = None;
                self.advertised = None;
                self.uart.write_line("[ble] connected");
            }
            Event::LeConnectionFailed { .. } => {
                CONNECT_FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            Event::LeConnectionUpdateComplete { conn_interval: iv, peripheral_latency: lat, .. } => {
                INTERVAL.store(iv, Ordering::Relaxed);
                LATENCY.store(lat, Ordering::Relaxed);
                self.uart.write_str("[ble] connection interval ");
                write_interval(self.uart, iv);
                self.uart.write_str(", latency ");
                self.uart.write_u32(u32::from(lat));
                self.uart.write_line("");
            }
            Event::LeConnectionUpdateFailed { status, .. } => {
                UPDATE_FAILURES.fetch_add(1, Ordering::Relaxed);
                LAST_UPDATE_FAILURE.store(status.into_inner(), Ordering::Relaxed);
            }
            Event::DisconnectionComplete { status, reason, .. } => {
                if status != BleStatus::SUCCESS {
                    DISCONNECT_FAILURES.fetch_add(1, Ordering::Relaxed);
                }
                set_state(STATE_IDLE);
                self.link_gone();
                self.uart.write_str("[ble] disconnected, reason ");
                self.uart.write_hex8(reason.into_inner());
                self.uart.write_line(" (13 = the central hung up)");
                self.readvertise = true;
            }
            e @ Event::EncryptionChange { status, .. } => {
                if status != BleStatus::SUCCESS {
                    ENCRYPTION_FAILURES.fetch_add(1, Ordering::Relaxed);
                }
                let on = matches!(e.encryption(), Some((_, true)));
                self.set_link_encrypted(on, if on { "[ble] link encrypted" } else { "[ble] link encryption off" });
            }
            e @ Event::EncryptionKeyRefresh { status, .. } => {
                if status != BleStatus::SUCCESS {
                    ENCRYPTION_FAILURES.fetch_add(1, Ordering::Relaxed);
                }
                let on = matches!(e.encryption(), Some((_, true)));
                self.set_link_encrypted(on, if status == BleStatus::SUCCESS { "[ble] link keys refreshed" } else { "[ble] link key refresh failed" });
            }
            Event::LeDataLengthChange { max_tx_octets, max_tx_time, max_rx_octets, max_rx_time, .. } => {
                self.data_length((max_tx_octets, max_tx_time), (max_rx_octets, max_rx_time));
            }
            Event::LeAdvertisingReport { data } => self.scanner.advertising(data, self.uart),
            Event::Vendor { code, params } => self.vendor(code, params),
            Event::LeUnknown { subevent_code } => self.unknown("[ble] LE event ", event_code::LE_META, subevent_code),
            Event::Unknown { code } => self.unknown("[ble] event ", code, 0),
            _ => {
                UNKNOWN.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The link's encryption, from either event that reports it, so a refresh after a lost
    /// change settles the same state.
    fn set_link_encrypted(&mut self, on: bool, line: &str) {
        set_encrypted(on);
        if on {
            self.pair_ask.settled();
            self.interval_ask.encrypted();
            if !PROVEN.swap(true, Ordering::Relaxed) {
                PROVEN_NOW.signal(());
            }
        }
        self.uart.write_line(line);
        self.set_mirror();
    }

    /// The link is gone: its encryption, the subscription and mirror, a pairing prompt, the
    /// pending asks and any transfer with it.
    fn link_gone(&mut self) {
        CONN_HANDLE.store(0, Ordering::Relaxed);
        set_encrypted(false);
        self.subscribed = false;
        self.peer = None;
        self.set_mirror();
        self.request_interval = None;
        self.pair_ask.settled();
        self.interval_ask.disconnected();
        clear_prompt();
        if let Some(session) = self.transfer.take() {
            crate::update::abort(session);
        }
    }

    /// `ACI_BLUE_INITIALIZED`: the boot a reset waits for, or a restart nobody asked for, which
    /// leaves nothing of the link or the configuration; [`Link::serve`] brings the chip up again.
    fn initialized(&mut self, reason: u8) {
        let boot = self.restarts.initialized(self.booting, reason, Instant::now().as_ticks(), embassy_time::TICK_HZ);
        let restarted = matches!(boot, Boot::Restarted | Boot::KeptRestarting);
        self.uart.write_str(if restarted { "[ble] the chip restarted by itself, reason " } else { "[ble] chip initialised, reason " });
        self.uart.write_hex8(reason);
        self.uart.write_str(" (");
        self.uart.write_str(boot_reason::describe(reason));
        if !restarted {
            self.booting = false;
            self.boot = Some(boot);
            return self.uart.write_line(")");
        }
        self.uart.write_line(if boot_reason::updater(reason) { "); resetting it out of the updater" } else { "); resetting it" });
        RESTARTS.fetch_add(1, Ordering::Relaxed);
        LAST_RESTART.store(reason, Ordering::Relaxed);
        set_state(STATE_STARTING);
        self.link_gone();
        clear_out(self.outbox.discard());
        self.outbox = Outbox::default();
        self.nus = None;
        self.mtu = DEFAULT_MTU;
        self.asked = None;
        self.advertised = None;
        self.readvertise = false;
        self.finish_transfer = false;
        self.passkey_for = None;
        self.rebond_for = None;
        self.recount_bonds = false;
        self.restart = Some(boot);
    }

    /// `[ble] data length TX 251 octets in 2120 us, RX 27 octets in 328 us`.
    fn data_length(&self, tx: (u16, u16), rx: (u16, u16)) {
        for (label, (octets, time)) in [("[ble] data length TX ", tx), (" us, RX ", rx)] {
            self.uart.write_str(label);
            self.uart.write_u32(u32::from(octets));
            self.uart.write_str(" octets in ");
            self.uart.write_u32(u32::from(time));
        }
        self.uart.write_line(" us");
    }

    /// Named on the console the first time its code comes.
    fn unknown(&mut self, label: &str, code: u8, subevent: u8) {
        UNKNOWN.fetch_add(1, Ordering::Relaxed);
        if self.seen.first(u16::from_be_bytes([code, subevent])) {
            self.uart.write_str(label);
            self.uart.write_hex8(if code == event_code::LE_META { subevent } else { code });
            self.uart.write_line("");
        }
    }

    /// A central wrote one of our attributes: the console's RX (bytes for the parser or a
    /// transfer) or TX's CCCD (the mirror on or off).
    fn attribute_modified(&mut self, write: AttributeModified) {
        let AttributeModified { attr, offset, data } = write;
        let Some((_, rx, tx)) = self.nus else {
            return;
        };
        if attr == rx + 1 {
            if !ENCRYPTED.load(Ordering::Relaxed) {
                UNSECURED.fetch_add(1, Ordering::Relaxed);
                return self.uart.write_line("[ble] a console write over an unencrypted link: dropped");
            }
            if offset != 0 {
                FRAGMENTS.fetch_add(1, Ordering::Relaxed);
            }
            let crc = wrist_flash::plan::crc32(data);
            if data.len() > DEFAULT_VALUE_LEN && crc == self.last_rx {
                REPEATS.fetch_add(1, Ordering::Relaxed);
                self.uart.write_str("[ble] console write repeated, events ");
                self.uart.write_u32(TRANSPORT.reads.load(Ordering::Relaxed));
                self.uart.write_line("");
            }
            self.last_rx = crc;
            self.console_bytes(data);
        } else if attr == tx + 2 {
            self.subscribed = data.first().is_some_and(|b| b & 0x01 != 0);
            self.set_mirror();
        }
    }

    fn vendor(&mut self, ecode: u16, rest: &[u8]) {
        let handle = bluenrg2::connection_handle;
        match ecode {
            vendor::BLUE_INITIALIZED => self.initialized(bluenrg2::blue_initialized(rest)),
            vendor::HAL_EVENTS_LOST => self.uart.write_line("[ble] the chip dropped events: the host read too slowly"),
            vendor::GAP_PASS_KEY_REQ if handle(rest).is_some() => {
                self.pair_ask.settled();
                self.passkey_for = handle(rest).map(|h| h.raw());
                PROMPT.lock(|p| p.set(Prompt::Asked));
            }
            vendor::GAP_NUMERIC_COMPARISON_VALUE if bluenrg2::numeric_comparison(rest).is_some() => {
                self.pair_ask.settled();
                let (_, value) = bluenrg2::numeric_comparison(rest).unwrap_or_default();
                PROMPT.lock(|p| p.set(Prompt::Waiting(Pairing::Compare, value, Instant::now().as_millis())));
                crate::wake_display();
                self.uart.write_str("[ble] pairing: the central shows a number; if it is ");
                write_six_digits(self.uart, value);
                self.uart.write_line(", `ble yes` on the USB console (`ble no` rejects)");
            }
            vendor::GAP_PAIRING_COMPLETE if rest.len() >= 4 => {
                let (status, reason) = bluenrg2::pairing_complete(rest).unwrap_or_default();
                LAST_PAIRING.store(status.saturating_add(1), Ordering::Relaxed);
                LAST_PAIRING_REASON.store(reason, Ordering::Relaxed);
                self.pair_ask.settled();
                clear_prompt();
                if status == pairing_status::SUCCESS {
                    old_key(OldKey::clear);
                    self.uart.write_line("[ble] paired");
                    self.recount_bonds = true;
                } else {
                    if let (pairing_status::LTK_MISSING, Some(peer)) = (status, self.peer) {
                        old_key(|k| k.refused(peer));
                    }
                    self.interval_ask.pairing_failed();
                    self.uart.write_str("[ble] pairing failed: status ");
                    self.uart.write_hex8(status);
                    self.uart.write_str(" (1 timeout, 2 failed, 3/4 LTK missing), reason ");
                    self.uart.write_hex8(reason);
                    self.uart.write_line(" (4 confirm failed, B DHKey check, C comparison refused)");
                }
            }
            vendor::GAP_SLAVE_SECURITY_INITIATED => self.uart.write_line("[ble] security request sent"),
            vendor::GAP_BOND_LOST => self.rebond_for = Some(CONN_HANDLE.load(Ordering::Relaxed)),
            vendor::ATT_EXCHANGE_MTU_RESP if rest.len() >= 4 => {
                self.mtu = usize::from(bluenrg2::att_mtu(rest).unwrap_or(0)).clamp(DEFAULT_MTU, MAX_MTU);
                self.uart.write_str("[ble] ATT MTU ");
                self.uart.write_u32(u32::try_from(self.mtu).unwrap_or(0));
                self.uart.write_line("");
            }
            vendor::GATT_TX_POOL_AVAILABLE => {}
            vendor::GATT_ATTRIBUTE_MODIFIED if rest.len() >= 8 => {
                if let Some(write) = AttributeModified::decode(rest) {
                    self.attribute_modified(write);
                }
            }
            vendor::L2CAP_CONNECTION_UPDATE_RESP if rest.len() >= 4 => {
                if L2capUpdateResponse::decode(rest).is_some_and(|r| !r.accepted()) {
                    UPDATES_REFUSED.fetch_add(1, Ordering::Relaxed);
                    self.uart.write_line("[ble] the central refused the interval request");
                }
            }
            vendor::GAP_PROC_COMPLETE => {
                if let Some((code, status)) = bluenrg2::gap_proc_complete(rest) {
                    self.scanner.gap_proc_complete(code, status, self.uart);
                }
            }
            _ => {
                UNKNOWN.fetch_add(1, Ordering::Relaxed);
                self.uart.write_str("[ble] vendor event ");
                self.uart.write_hex8((ecode >> 8) as u8);
                self.uart.write_hex8((ecode & 0xFF) as u8);
                self.uart.write_line("");
            }
        }
    }
}

fn set_state(state: u8) {
    if STATE.swap(state, Ordering::Relaxed) != state {
        redraw();
    }
}

fn set_encrypted(on: bool) {
    if ENCRYPTED.swap(on, Ordering::Relaxed) != on {
        redraw();
    }
}

/// The page redrawn, if up, for a change it shows.
fn redraw() {
    crate::redraw(wrist_pages::Page::Bluetooth);
}

fn micros(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

fn write_address(uart: Uart, a: bluenrg2::Address) {
    use core::fmt::Write;
    struct Out(Uart);
    impl Write for Out {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0.write_str(s);
            Ok(())
        }
    }
    let _ = write!(Out(uart), "{a}");
}

fn clear_prompt() {
    PROMPT.lock(|p| p.set(Prompt::None));
    crate::wake_display();
}

/// A connection interval in 1.25 ms units as milliseconds, `48.75 ms`.
fn write_interval(uart: Uart, units: u16) {
    let hundredths = bluenrg2::interval_hundredths_ms(units);
    uart.write_u32(hundredths / 100);
    uart.write_str(if hundredths % 100 < 10 { ".0" } else { "." });
    uart.write_u32(hundredths % 100);
    uart.write_str(" ms");
}

fn address_from_uid() -> [u8; 6] {
    let uid = embassy_stm32::uid::uid();
    let w = |i: usize| u32::from_le_bytes([uid[4 * i], uid[4 * i + 1], uid[4 * i + 2], uid[4 * i + 3]]);
    wrist_bluenrg2::bonds::static_random_address([w(0), w(1), w(2)])
}

fn write_six_digits(uart: Uart, v: u32) {
    let v = v % 1_000_000;
    let mut div = 100_000;
    while div > 0 {
        uart.write_u32(v / div % 10);
        div /= 10;
    }
}

fn write_hex(uart: Uart, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            uart.write_str(" ");
        }
        uart.write_hex8(*b);
    }
}
