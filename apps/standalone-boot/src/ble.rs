//! The BlueNRG-2 on the link1 module (Sensor 1), driven by ST's ACI: the console and
//! application updates over the air, for a bonded central only (`ble` feature).
//!
//! The radio is off until `ble on`, and `ble off` holds it in reset again; the choice is kept
//! in a backup register (`rtc::radio_wanted`), so an image that arrived over the air comes
//! back with its radio on. When on, the task resets the chip, gives it an address,
//! brings up ST's GATT and GAP, and advertises as `UNA`.
//!
//! Security. Pairing is LE Secure Connections with numeric comparison, MITM required: the
//! central shows a number, `ble` on the USB console shows the same one, and `ble yes`
//! answers it (`ble no`, or 60 s, rejects). With MITM required no unauthenticated key
//! exists, so an encrypted link is a bonded central's. The console's RX characteristic asks
//! the stack for an authenticated, encrypted link, and the watch checks too: nothing written
//! over a link that is not encrypted is acted on, and the console's output is mirrored only to
//! an encrypted link. The loader is never accepted over the air.
//!
//! The loop has no tick: it waits for the chip's IRQ, a request, a signal, a power change or its
//! earliest deadline (`wrist_bluenrg2::wake::ble::due`).
//!
//! Pins: SPI1 `SCK` PG2, `MISO` PG3, `MOSI` PB5; `CS` PA15 as a GPIO; `BLE_RST_L` PA8
//! (active low); `BLE_BOOT_SPI_IRQ` PA9, the chip's `DIO7`, which is both the transport's
//! "data ready / slave ready" line and the bootloader-entry pin if high at reset, so it is an
//! input with a pull-down and never driven. SPI1, CS and IRQ exist only while the radio is on.
//!
//! The SPI handshake is `wrist_bluenrg2::transport`.
//!
//! Restarts. A boot the driver's start did not ask for is the chip restarting by itself
//! (`wrist_bluenrg2::wake::ble::restart`): the link and everything on it is dropped, no command
//! goes to it, and it is reset through its pin and brought up again, until the core's budget for
//! resets is spent.
//!
//! Address. A static random address from the MCU's unique id
//! (`wrist_bluenrg2::bonds::static_random_address`), so bonds survive updates.
//!
//! Decisions. The Bluetooth core (`wrist_bluenrg2::shadow`) makes the policy decisions and this
//! driver carries them out; a policy command sent otherwise is a `[ble] shadow:` line, counted on
//! the `ble` line.

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
use static_cell::StaticCell;
use bluenrg2::opcode::{
    ACI_GAP_CLEAR_SECURITY_DB, ACI_GAP_GET_BONDED_DEVICES, ACI_GAP_REMOVE_BONDED_DEVICE, ACI_HAL_GET_FW_BUILD_NUMBER, HCI_LE_RAND,
    HCI_READ_LOCAL_VERSION,
};
use bluenrg2::{
    event_code, io_capability, AddrKind, BdAddr, BootReason, Event, BleStatus,
    PairingReason, VendorEvent, ATT_HEADER_LEN,
};
use wrist_ble::policy::chip::Action;
use wrist_ble::{
    Cccd, CharDecl, ConnHandle, DisconnectReason, Failure, FailureKind, LeConnRole, Micros, Op, PairingEvent, PairingStep, Payload, RadioEvent, ServerTable,
    ServiceDecl, ATT_MAX_MTU,
};
use wrist_ble::policy::goals::{self, Goals, PhoneGoal};
use wrist_ble::policy::status::{next_change, status, Status as CoreStatus, StatusLog};
use wrist_ble::policy::tally::Tally;
use wrist_bluenrg2::shadow::verdict::Disagreement;
use wrist_bluenrg2::shadow::{Mode, Observer, Shadow};
use wrist_bluenrg2::commands;
use wrist_bluenrg2::driver::{self, Driver, Got, Reply, Unanswered};
use wrist_bluenrg2::snapshot::{Snap, Snapping};
use wrist_bluenrg2::start::{Setup, Start, Starting, Step};
use wrist_bluenrg2::transport::{self, Counts};
use wrist_logic::pages::{OldKey, Pairing, Prompt};
use wrist_bluenrg2::wake::ble::restart::{self, Boot};
use wrist_bluenrg2::wake::ble::{server, Lines, Outbox, Power, Radio, Scan, Sink, Write, Writes};

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

/// A transfer with no bytes for this long is abandoned, as over USB.
const TRANSFER_IDLE: Duration = Duration::from_secs(5);

const DEVICE_NAME: &[u8] = b"UNA";
// Advertising carries the name only up to `commands::MAX_NAME` (PM0257 Table 43), and the GATT
// device name only up to what `ACI_GAP_INIT` gave it room for.
const _: () = assert!(DEVICE_NAME.len() <= commands::MAX_NAME && DEVICE_NAME.len() <= GAP_NAME_LEN as usize);
/// `ACI_GAP_INIT`'s `device_name_char_len`.
const GAP_NAME_LEN: u8 = 7;
/// `ACI_HAL_SET_TX_POWER_LEVEL`'s `En_High_Power` and `PA_Level`.
const TX_POWER: [u8; 2] = [0x01, 0x04];
/// `HCI_SET_EVENT_MASK` (Core Vol 4 Part E 7.3.1).
const EVENT_MASK: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xBF, 0x3D];
static DEVICE_INFORMATION: [CharDecl; 3] = server::device_information(crate::version::BUILD_ID.as_bytes());
static SERVICES: [ServiceDecl; 2] = server::services(&DEVICE_INFORMATION);
static SERVER: ServerTable = ServerTable { services: &SERVICES };
/// Until an exchange raises it.
const DEFAULT_MTU: usize = bluenrg2::ATT_DEFAULT_MTU;
/// The value a default-MTU write or notification carries.
const DEFAULT_VALUE_LEN: usize = DEFAULT_MTU - ATT_HEADER_LEN;
/// Console notifications at most this often.
const CONSOLE_PACE: Duration = Duration::from_millis(10);
/// The shortest connection interval (Core 6.B 4.5.1); a write repeated sooner may be one the chip
/// reported twice.
const DOUBLED_WITHIN: Duration = Duration::from_micros(7500);
/// The longest a chunk refused for want of TX buffers waits for the chip to say it has some.
const POOL_WAIT: Duration = Duration::from_secs(1);
/// The core's operations carried out in one pass of the loop, at most.
const ISSUED_PER_PASS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    On,
    Off,
    Version,
    Yes,
    No,
    Bonds,
    /// `ble status`: the core's status line, now.
    Status,
    Forget,
    /// `ble forget <address>`: one bond, by its address and type.
    ForgetOne(bluenrg2::Address),
    Disconnect,
    /// Ask for this interval range (1.25 ms units) on the current connection.
    IntervalNow(u16, u16),
    /// `ble scan`: seconds, and a duty in place of the power source's.
    Scan(u8, Option<Scan>),
    /// `ble probe refuse <hex>`: the core's next command answered with this status, unsent.
    #[cfg(feature = "ble-probe")]
    ProbeRefuse(u8),
    /// `ble probe refuse scan <hex>`: the next scan's start answered with this status, unsent.
    #[cfg(feature = "ble-probe")]
    ProbeRefuseScan(u8),
    /// `ble probe lose link`: the next connection withheld from the core, a lost read reported.
    #[cfg(feature = "ble-probe")]
    ProbeLoseLink,
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
    /// `ble probe stop`: `ACI_GAP_SET_NON_DISCOVERABLE` twice, nothing between.
    #[cfg(feature = "ble-probe")]
    ProbeStop,
    /// `ble probe drop <hex>`: `ACI_GAP_TERMINATE` with this reason.
    #[cfg(feature = "ble-probe")]
    ProbeDrop(u8),
    /// `ble probe snapshot`: the driver's snapshot of the chip.
    #[cfg(feature = "ble-probe")]
    ProbeSnapshot,
    /// `ble probe security`: `ACI_GAP_SLAVE_SECURITY_REQ` again on an unencrypted link.
    #[cfg(feature = "ble-probe")]
    ProbeSecurity,
}

static REQUESTS: Channel<CriticalSectionRawMutex, Request, 4> = Channel::new();
/// A line from the central's RX and its length.
type Line = ([u8; 192], usize);
/// Raised by other tasks that leave the loop work it cannot see coming: console output for a
/// subscribed central. The loop clears it before it reads that state.
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Console output for the central, filled while [`MIRROR`] is set.
static OUT: Pipe<CriticalSectionRawMutex, 4096> = Pipe::new();
/// Raised as room opens in [`OUT`] or the mirror stops, for [`room`].
static ROOM: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// The longest [`room`] waits before lines go regardless, until room next opens.
const ROOM_WAIT: Duration = Duration::from_secs(3);
/// [`room`] waited [`ROOM_WAIT`] in vain, and room has not opened since.
static STALLED: AtomicBool = AtomicBool::new(false);
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
/// The phone link's connection interval (1.25 ms units), 0 if unknown, and its latency.
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
/// Where the shadow and this driver acted differently, this boot ([`Said`]).
static SHADOW: AtomicU32 = AtomicU32::new(0);
/// The core's counters, as the `ble` line shows them ([`Link::publish`]).
static CORE_ASKED: AtomicU32 = AtomicU32::new(0);
static CORE_REFUSED: AtomicU32 = AtomicU32::new(0);
static CORE_FAILED: AtomicU32 = AtomicU32::new(0);
static CORE_TIMED_OUT: AtomicU32 = AtomicU32::new(0);
static CORE_REASKS: AtomicU32 = AtomicU32::new(0);
static CORE_DIVERGENCES: AtomicU32 = AtomicU32::new(0);
static CORE_BUSY_SPENT: AtomicU32 = AtomicU32::new(0);
static CORE_DROPPED: AtomicU32 = AtomicU32::new(0);
/// Chip restarts the core asked for while the chip was up, this boot.
static CORE_RESTARTS: AtomicU32 = AtomicU32::new(0);

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

/// Empties [`OUT`] and forgets the line in hand.
fn clear_out() {
    LINES.lock(|lines| {
        OUT.clear();
        if let Ok(mut l) = lines.try_borrow_mut() {
            l.clear();
        }
    });
    room_opened();
}

fn room_opened() {
    STALLED.store(false, Ordering::Relaxed);
    ROOM.signal(());
}

/// Waits, boundedly, until a line of `len` bytes fits in the mirror with room to spare for
/// output that cannot wait, or the mirror stops: a long command goes at the link's pace.
pub async fn room(len: usize) {
    let until = Instant::now() + ROOM_WAIT;
    while MIRROR.load(Ordering::Relaxed)
        && !STALLED.load(Ordering::Relaxed)
        && !wrist_logic::console::room(OUT.free_capacity(), OUT.capacity(), len)
    {
        if embassy_time::with_deadline(until, ROOM.wait()).await.is_err() {
            STALLED.store(true, Ordering::Relaxed);
            return;
        }
    }
}

/// Lines the mirror has dropped for want of room, this boot.
pub fn lines_dropped() -> u32 {
    LINES_DROPPED.load(Ordering::Relaxed)
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

/// `ble on|off|version|yes|no|bonds|status|forget [address]|disconnect|profile auto|cable|battery|interval now <min> <max>|scan [s [interval window]]`.
pub fn command(args: &[u8], uart: Uart) {
    let request = match args {
        b"on" => Request::On,
        b"off" => Request::Off,
        b"version" => Request::Version,
        b"yes" => Request::Yes,
        b"no" => Request::No,
        b"bonds" => Request::Bonds,
        b"status" => Request::Status,
        b"forget" => Request::Forget,
        rest if rest.starts_with(b"forget ") => {
            match core::str::from_utf8(&rest[7..]).ok().and_then(wrist_bluenrg2::console::parse_address) {
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
        b"probe stop" => Request::ProbeStop,
        #[cfg(feature = "ble-probe")]
        b"probe lose link" => Request::ProbeLoseLink,
        #[cfg(feature = "ble-probe")]
        b"probe snapshot" => Request::ProbeSnapshot,
        #[cfg(feature = "ble-probe")]
        b"probe security" => Request::ProbeSecurity,
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
        #[cfg(feature = "ble-probe")]
        rest if rest.starts_with(b"probe refuse scan ") => {
            match core::str::from_utf8(&rest[18..]).ok().and_then(|t| u8::from_str_radix(t.trim(), 16).ok()) {
                Some(status) => Request::ProbeRefuseScan(status),
                None => return uart.write_line("[ble] usage: ble probe refuse scan <status, hex>"),
            }
        }
        #[cfg(feature = "ble-probe")]
        rest if rest.starts_with(b"probe refuse ") => {
            match core::str::from_utf8(&rest[13..]).ok().and_then(|t| u8::from_str_radix(t.trim(), 16).ok()) {
                Some(status) => Request::ProbeRefuse(status),
                None => return uart.write_line("[ble] usage: ble probe refuse <status, hex>"),
            }
        }
        rest if rest.starts_with(b"interval now") => {
            let text = core::str::from_utf8(&rest[12..]).unwrap_or("");
            let mut units = text.split_whitespace().map(|w| w.parse::<u16>().ok());
            match (units.next(), units.next()) {
                (Some(Some(min)), Some(Some(max))) if wrist_bluenrg2::wake::ble::interval_now(min, max) => {
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
                "[ble] usage: ble [on|off|version|yes|no|bonds|status|forget [address]|disconnect|profile auto|cable|battery|interval now <min> <max>|scan [s [interval window]]]",
            )
        }
    };
    if REQUESTS.try_send(request).is_err() {
        uart.write_line("[ble] not queued: four already waiting");
    }
}

/// `ble`: the radio and its counters, then what went wrong and how often.
pub async fn write_status(uart: Uart) {
    write_radio(uart).await;
    uart.pace().await;
    let _ = TALLY.lock(Cell::get).write_line(&mut Text(uart));
    uart.write_line("");
}

#[allow(clippy::too_many_lines)]
async fn write_radio(uart: Uart) {
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
        match INTERVAL.load(Ordering::Relaxed) {
            0 => uart.write_str(", interval unknown"),
            units => {
                uart.write_str(", interval ");
                write_interval(uart, units);
                uart.write_str(" latency ");
                uart.write_u32(u32::from(LATENCY.load(Ordering::Relaxed)));
            }
        }
    }
    uart.write_str(", timing ");
    uart.write_str(match (PROFILE.load(Ordering::Relaxed), power()) {
        (0, Power::Cable) => "cable",
        (0, Power::Battery) => "battery",
        (_, Power::Cable) => "cable (forced)",
        (_, Power::Battery) => "battery (forced)",
    });
    uart.write_str(", address ");
    write_address(uart, bluenrg2::Address { kind: AddrKind::RANDOM, addr: BdAddr(ADDRESS.each_ref().map(|b| b.load(Ordering::Relaxed))) });
    uart.write_str(", bonded ");
    uart.write_u32(u32::from(BONDED.load(Ordering::Relaxed)));
    uart.pace().await;
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
    uart.pace().await;
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
    uart.pace().await;
    n(", core asked ", &CORE_ASKED);
    n(" refused ", &CORE_REFUSED);
    n(" failed ", &CORE_FAILED);
    n(" timed out ", &CORE_TIMED_OUT);
    n(" reasks ", &CORE_REASKS);
    n(" divergences ", &CORE_DIVERGENCES);
    n(" busy restarts ", &CORE_BUSY_SPENT);
    n(" dropped ", &CORE_DROPPED);
    n(" restarts ", &CORE_RESTARTS);
    uart.pace().await;
    n(", shadow disagreements ", &SHADOW);
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
pub fn page() -> wrist_display::glass::pages::Bluetooth {
    use wrist_display::glass::pages::{Bluetooth, Prompt as Digits, Radio as Shown};
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
        crate::redraw(wrist_logic::pages::Page::Face);
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

#[embassy_executor::task]
pub async fn run(mut pins: Pins, flash: &'static FlashMutex) -> ! {
    let uart = Uart::best_effort();
    // Held in reset from here on, except while the radio is on.
    let mut rst = Output::new(pins.rst, Level::Low, Speed::Low);
    let mut changes = crate::power::changes();
    let mut on = crate::rtc::radio_wanted();
    if on {
        uart.write_line("[ble] on at boot: `ble on` was the last word (BKP29R)");
    }
    let shadow = shadow(uart);
    loop {
        while !on {
            match REQUESTS.receive().await {
                Request::On => on = true,
                Request::Status => write_status_line(uart, &status(&wrist_ble::policy::State::OFF, now_ms())),
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
                Request::Probe
                | Request::ProbeGatt(_)
                | Request::Restart(_)
                | Request::ProbeAdvertising
                | Request::ProbeStop
                | Request::ProbeDrop(_)
                | Request::ProbeSnapshot
                | Request::ProbeSecurity
                | Request::ProbeRefuse(_)
                | Request::ProbeRefuseScan(_)
                | Request::ProbeLoseLink => {
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
        let mut link = Link::new(
            chip(
                (pins.spi.reborrow(), pins.sck.reborrow(), pins.miso.reborrow(), pins.mosi.reborrow()),
                (pins.cs.reborrow(), pins.irq.reborrow(), pins.irq_exti.reborrow()),
                (pins.tx_dma.reborrow(), pins.rx_dma.reborrow()),
                &mut rst,
            ),
            uart,
            flash,
            &mut *shadow,
        );
        // A start, or a restart's recovery, that failed.
        if !(link.start().await && link.serve(&mut changes).await) {
            // The core's own fault, before the radio goes off; a step the driver failed is not the
            // core's to show, so the radio as it goes off.
            let core_gave_up = Radio::of(link.shadow.state()) == Radio::Failed;
            if core_gave_up {
                link.last_status();
            }
            link.shadow_off();
            if !core_gave_up {
                link.last_status();
            }
            set_state(STATE_FAILED);
            old_key(OldKey::clear);
            link.chip.hold();
            uart.write_line("[ble] start failed: chip held in reset; `ble on` retries");
            on = false;
            continue;
        }
        // `ble off`: the chip in reset first, then SPI1, CS and IRQ dropped back to analog.
        link.shadow_off();
        link.last_status();
        link.chip.hold();
        drop(link);
        MIRROR.store(false, Ordering::Relaxed);
        ROOM.signal(());
        set_encrypted(false);
        set_state(STATE_OFF);
        clear_prompt();
        old_key(OldKey::clear);
        crate::rtc::set_radio_wanted(false);
        uart.write_line("[ble] off: chip held in reset");
        on = false;
    }
}

type Chip<'d> = Driver<'static, Spi<'d, Async, spi::mode::Master>, Output<'d>, ExtiInput<'d, Async>, Delay, &'d mut Output<'static>, Now>;

/// The driver's clock: embassy's.
struct Now;

impl driver::Clock for Now {
    const HZ: u64 = embassy_time::TICK_HZ;

    fn now(&self) -> u64 {
        Instant::now().as_ticks()
    }
}

/// SPI1, CS and IRQ while the radio is on, and the reset pin.
fn chip<'d>(
    (spi, sck, miso, mosi): (Peri<'d, peripherals::SPI1>, Peri<'d, peripherals::PG2>, Peri<'d, peripherals::PG3>, Peri<'d, peripherals::PB5>),
    (cs, irq, irq_exti): (Peri<'d, peripherals::PA15>, Peri<'d, peripherals::PA9>, Peri<'d, peripherals::EXTI9>),
    (tx_dma, rx_dma): (Peri<'d, peripherals::GPDMA1_CH2>, Peri<'d, peripherals::GPDMA1_CH3>),
    rst: &'d mut Output<'static>,
) -> Chip<'d> {
    let mut config = spi::Config::default();
    config.frequency = Hertz(1_000_000);
    // ST's bus setup for the BlueNRG-2 in X-CUBE-BLE2.
    config.mode = spi::MODE_1;
    Driver::new(
        Spi::new(spi, sck, mosi, miso, tx_dma, rx_dma, Irqs, config),
        Output::new(cs, Level::High, Speed::VeryHigh),
        ExtiInput::new(irq, irq_exti, Pull::Down, Irqs),
        Delay,
        &TRANSPORT,
        rst,
        Now,
    )
}

// Each flag is one piece of work an event left for the loop.
#[allow(clippy::struct_excessive_bools)]
struct Link<'d> {
    chip: Chip<'d>,
    uart: Uart,
    flash: &'static FlashMutex,
    /// The last console write, to spot one delivered twice.
    writes: Writes,
    /// Deferred work raised by events, done from the main loop.
    passkey_for: Option<u16>,
    recount_bonds: bool,
    line: Line,
    /// The negotiated ATT MTU; notifications carry `mtu - 3`.
    mtu: usize,
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
    outbox: Outbox<{ server::CONSOLE_VALUE_LEN }>,
    scanner: scan::Scanner,
    seen: Seen,
    /// How the boot a reset asked for came up.
    boot: Option<Boot>,
    /// A start under way, or one that stopped at a step: shown whatever the core holds.
    shown: Option<Radio>,
    /// Which of the core's statuses have gone out as lines.
    status_log: StatusLog,
    shadow: &'d mut Shadow<Said>,
    /// `ble probe refuse`: the core's next command answered with this status, unsent.
    #[cfg(feature = "ble-probe")]
    refuse_next: Option<u8>,
    /// `ble probe refuse scan`: the next scan's start answered with this status, unsent.
    #[cfg(feature = "ble-probe")]
    refuse_scan: Option<u8>,
    /// `ble probe lose link`: the next connection withheld from the core.
    #[cfg(feature = "ble-probe")]
    withhold_connection: bool,
}

impl<'d> Link<'d> {
    fn new(chip: Chip<'d>, uart: Uart, flash: &'static FlashMutex, shadow: &'d mut Shadow<Said>) -> Self {
        Self {
            chip,
            uart,
            flash,
            writes: Writes::default(),
            passkey_for: None,
            recount_bonds: false,
            line: ([0; 192], 0),
            mtu: DEFAULT_MTU,
            subscribed: false,
            peer: None,
            transfer: None,
            last_data: Instant::from_ticks(0),
            finish_transfer: false,
            console_at: Instant::from_ticks(0),
            outbox: Outbox::default(),
            scanner: scan::Scanner::default(),
            seen: Seen::default(),
            boot: None,
            shown: None,
            status_log: StatusLog::NEW,
            shadow,
            #[cfg(feature = "ble-probe")]
            refuse_next: None,
            #[cfg(feature = "ble-probe")]
            refuse_scan: None,
            #[cfg(feature = "ble-probe")]
            withhold_connection: false,
        }
    }
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
            if self.chip.restarted() && !self.recover().await {
                return false;
            }
            if !self.core_chip().await {
                return false;
            }
            WAKE.reset();
            self.shadow.advance(now_ms());
            // What the driver ended or let expire since the last drain.
            self.packets().await;
            self.deferred().await;
            self.scan_tick().await;
            self.shadow.timing(now_ms(), shadow_power(), self.transfer.is_some());
            self.console_tick().await;
            self.resync().await;
            self.drive().await;
            self.publish();
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
                    let _ = self.submit(Op::Advertise(None)).await;
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

    /// When the loop must next run by time (`wrist_bluenrg2::wake::ble::due`); `None` when only an
    /// event can give it work.
    fn due(&self) -> Option<Instant> {
        use wrist_bluenrg2::wake::ble::{Console, State, Transfer};
        let tick = |i: Instant| i.as_ticks();
        let one = Duration::from_ticks(1);
        let state = State {
            now: tick(Instant::now()),
            work: self.shadow.issuing() || self.shadow.snapshot_wanted() || self.finish_transfer || self.recount_bonds || self.passkey_for.is_some(),
            confirm_by: PROMPT.lock(Cell::get).owed().map(|(at, _)| tick(Instant::from_millis(at))),
            transfer: match &self.transfer {
                None => Transfer::None,
                Some(s) if s.started() => Transfer::Started,
                Some(_) => Transfer::Claimed,
            },
            console: (self.chip.server().is_some() && MIRROR.load(Ordering::Relaxed))
                .then(|| Console { pending: !OUT.is_empty() || self.outbox.pending(), next_at: tick(self.console_at) }),
            abandon_at: self.transfer.as_ref().map(|_| tick(self.last_data + TRANSFER_IDLE + one)),
            scan_end: self.scanner.until().map(tick),
        };
        // The first tick at which `now_ms` reaches it.
        let ms = |at: wrist_ble::Instant| (at.0 * embassy_time::TICK_HZ).div_ceil(1000);
        let shadow = self.shadow.next().map(ms);
        let status = [next_change(self.shadow.state(), self.shadow.now()), self.status_log.next()].into_iter().flatten().min().map(ms);
        wrist_bluenrg2::wake::earliest([wrist_bluenrg2::wake::ble::due(&state), shadow, self.chip.due(), status]).map(Instant::from_ticks)
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
            Request::Status => write_status_line(self.uart, &status(self.shadow.state(), self.shadow.now())),
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
                    self.shadow.interval_now(now_ms(), min, max);
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
            Request::ProbeStop => self.probe_stop().await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeDrop(reason) => self.probe_drop(reason).await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeSnapshot => self.probe_snapshot().await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeSecurity => self.ask_to_pair().await,
            #[cfg(feature = "ble-probe")]
            Request::ProbeRefuse(status) => {
                self.refuse_next = Some(status);
                self.uart.write_str("[ble] the core's next command goes unsent, answered with status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
            }
            #[cfg(feature = "ble-probe")]
            Request::ProbeLoseLink => {
                self.withhold_connection = true;
                self.uart.write_line("[ble] the next connection goes to the core as a lost read");
            }
            #[cfg(feature = "ble-probe")]
            Request::ProbeRefuseScan(status) => {
                self.refuse_scan = Some(status);
                self.uart.write_str("[ble] the next scan's start goes unsent, answered with status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
            }
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

    /// Work raised by events.
    async fn deferred(&mut self) {
        if self.finish_transfer {
            self.finish_transfer = false;
            if let Some(session) = self.transfer.take() {
                // What queued while the mirror waited is stale; the result line must find room.
                self.outbox.clear();
                clear_out();
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
        // Unanswered, the digits only stop: the core answers at its own deadline.
        if let Some(yes) = owed.filter(|&yes| prompt == Prompt::Answered(yes)) {
            self.shadow.answer(now_ms(), yes);
        }
    }

    #[cfg(feature = "ble-probe")]
    async fn ask_to_pair(&mut self) {
        if STATE.load(Ordering::Relaxed) != STATE_CONNECTED || ENCRYPTED.load(Ordering::Relaxed) {
            return;
        }
        match self.submit(Op::RequestSecurity(link())).await {
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
        self.shadow.set_goals(now_ms(), shadow_goals());
        // The start this bring-up answers.
        let _ = self.shadow.chip_action();
        self.bring_up().await;
        self.recover().await
    }

    /// The radio off as the shadow's core takes it, everything it still holds judged, and a
    /// snapshot it asked for of this chip dropped.
    fn shadow_off(&mut self) {
        self.shadow.radio(now_ms(), false);
        self.shadow.finish();
        let _ = self.shadow.take_snapshot();
    }

    /// Bring up again a chip that restarted by itself since the driver last reset it, unless the
    /// core gave up on it; whether it is up.
    async fn recover(&mut self) -> bool {
        while self.chip.restarted() {
            if Radio::of(self.shadow.state()) == Radio::Failed {
                let _ = self.shadow.chip_action();
                self.uart.write_line("[ble] the core gave up on the chip: too many resets in an hour");
                return false;
            }
            self.bring_up().await;
        }
        self.shown.is_none()
    }

    /// The driver's start.
    async fn bring_up(&mut self) {
        self.shown = Some(Radio::Starting);
        self.show();
        clear_prompt();
        #[cfg(feature = "ble-trace")]
        {
            trace::packet(trace::Dir::Reset, &[]);
            trace::packet(trace::Dir::Mode, &[1]);
        }
        self.boot = None;
        CONN_HANDLE.store(0, Ordering::Relaxed);
        let addr = address_from_uid();
        let setup = Setup {
            address: addr,
            tx_power: TX_POWER,
            event_mask: EVENT_MASK,
            name: DEVICE_NAME,
            name_room: GAP_NAME_LEN,
            // The console shows the number; `ble yes` answers.
            io_capability: io_capability::DISPLAY_YES_NO,
            pairing: wrist_bluenrg2::bonds::PAIRING,
        };
        let mut start = Start::new(&SERVER, setup);
        let mut packet = [0u8; transport::MAX_READ];
        let mut central = false;
        let handles = loop {
            let Some(note) = self.chip.starting(&mut start, &mut packet).await else { return };
            match note {
                Starting::Got(got) => self.got(&packet, got),
                Starting::Sending(c) => {
                    trace_command(c.opcode, c.params());
                    self.shadow.command(now_ms(), c.opcode, c.params());
                }
                Starting::Silent => self.uart.write_line("[ble] no sign of life after reset (no module, or not ST's firmware)"),
                Starting::Booted => {
                    if self.boot == Some(Boot::InUpdater) {
                        return self.fail("reset: the chip came up in its updater");
                    }
                    for (slot, b) in ADDRESS.iter().zip(addr.iter()) {
                        slot.store(*b, Ordering::Relaxed);
                    }
                }
                Starting::GapRefused { roles, status } => {
                    self.uart.write_str("[ble] GAP init refused roles ");
                    self.uart.write_hex8(roles);
                    self.uart.write_str(", status ");
                    self.uart.write_hex8(status);
                    self.uart.write_line("");
                }
                Starting::GapUnanswered { why } => {
                    self.uart.write_str("[ble] GAP init: ");
                    self.uart.write_line(unanswered(why));
                }
                Starting::Roles { central: c } => {
                    central = c;
                    self.scanner.started(c);
                }
                Starting::Failed(step) => return self.failed(step),
                Starting::Up(handles) => break handles,
            }
        };
        if handles.get(server::RX).is_none() || handles.get(server::TX).is_none() {
            return self.fail("finding the console in the server");
        }
        self.chip.serve(handles);
        self.uart.write_str("[ble] up: ST stack, address ");
        write_address(self.uart, bluenrg2::Address { kind: AddrKind::RANDOM, addr: BdAddr(addr) });
        self.uart.write_line(if central { ", peripheral and central" } else { ", peripheral only" });
        self.list_bonds(false).await;
        self.shown = None;
        self.show();
    }

    /// Console output to a subscribed central, one notification per pass.
    async fn console_tick(&mut self) {
        if self.chip.server().is_none() {
            return;
        }
        // While image bytes arrive the mirror waits: notifications refused for want of TX
        // buffers were followed by the chip reporting the central's writes twice.
        if !MIRROR.load(Ordering::Relaxed)
            || self.transfer.as_ref().is_some_and(crate::update::Session::started)
            || Instant::now() < self.console_at
        {
            return;
        }
        let most = (self.mtu.saturating_sub(ATT_HEADER_LEN)).clamp(DEFAULT_VALUE_LEN, server::CONSOLE_VALUE_LEN);
        let chunk = self.outbox.next(most, |buf| {
            let n = OUT.try_read(buf).unwrap_or(0);
            if n > 0 {
                room_opened();
            }
            n
        });
        if chunk.is_empty() {
            return;
        }
        let link = ConnHandle::new(CONN_HANDLE.load(Ordering::Relaxed));
        let notify = Payload::new(chunk)
            .map(|value| Op::Notify(link, server::TX, value))
            .and_then(|op| commands::command(&op, DEVICE_NAME, |_| None, self.chip.server()));
        let answer = match notify {
            Some(c) => self.exchange(c.opcode, c.params()).await.map(|r| r.status),
            None => Err(Unanswered::TooLong),
        };
        let lost = self.outbox.sent(answer);
        self.console_at = Instant::now() + if self.outbox.waits_for_pool() { POOL_WAIT } else { CONSOLE_PACE };
        if lost {
            self.uart.write_line("[ble] a console notification was lost");
        }
    }

    /// The mirror runs while a central is subscribed over an encrypted link.
    fn set_mirror(&mut self) {
        let on = self.subscribed && ENCRYPTED.load(Ordering::Relaxed);
        if on != MIRROR.swap(on, Ordering::Relaxed) {
            self.outbox.clear();
            clear_out();
            self.uart.write_line(if on {
                "[ble] console attached over the air; 'help' lists commands"
            } else {
                "[ble] console detached"
            });
        }
    }

    /// Bytes the central wrote to RX: one command per line, answered through the mirror, or
    /// the next piece of an image.
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
                _ if self.line.1 < self.line.0.len() => {
                    self.line.0[self.line.1] = b;
                    self.line.1 += 1;
                }
                _ => {}
            }
        }
        // A write with no newline is a whole command too: nRF Connect's write dialog sends
        // none, and a terminal sends one line per write.
        self.run_line();
    }

    /// A transfer starts here; any other command goes to the console's task.
    fn run_line(&mut self) {
        let (line, n) = core::mem::replace(&mut self.line, ([0; 192], 0));
        if n == 0 {
            return;
        }
        match crate::console::parse_transfer(&line[..n]) {
            Some((Target::Application, len, crc)) => self.start_transfer(len, crc),
            Some((Target::Loader, ..)) => self.uart.write_line("[loader] error: not over the air; USB only"),
            None => {
                if !crate::console::submit(&line[..n]) {
                    self.uart.write_line("[ble] command dropped: the queue is full");
                }
            }
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

    /// The line for the step a start stopped at.
    fn failed(&mut self, step: Step) {
        match step {
            Step::StaticRandomAddress => self.fail("static random address"),
            Step::PublicAddressSlot => self.fail("public address slot"),
            Step::TxPower => self.fail("TX power"),
            Step::EventMask => self.fail("event mask"),
            Step::DataLength => self.fail("data length"),
            Step::GattInit => self.fail("GATT init"),
            Step::GapInit => self.fail("GAP init"),
            Step::DeviceName => self.fail("device name"),
            Step::IoCapability => self.fail("IO capability"),
            Step::SecurityRequirements => self.fail("security requirements"),
            Step::Service(name) => self.fail_at(&["adding the ", name, " service"]),
            Step::Characteristic(name) => self.fail_at(&["adding a ", name, " characteristic"]),
            Step::Value(name) => self.fail_at(&["setting a ", name, " value"]),
        }
    }

    fn fail(&mut self, step: &str) {
        self.fail_at(&[step]);
    }

    fn fail_at(&mut self, step: &[&str]) {
        self.shown = Some(Radio::Failed);
        self.show();
        self.uart.write_str("[ble] start failed at ");
        for part in step {
            self.uart.write_str(part);
        }
        self.uart.write_line("");
    }

    async fn list_bonds(&mut self, after_pairing: bool) {
        if let Ok(r) = self.command(ACI_GAP_GET_BONDED_DEVICES, &[]).await {
            if r.len >= 1 {
                let params = &r.params[..r.len.min(r.params.len())];
                let before = BONDED_READ.load(Ordering::Relaxed).then(|| BONDED.load(Ordering::Relaxed));
                BONDED.store(params[0], Ordering::Relaxed);
                BONDED_READ.store(true, Ordering::Relaxed);
                redraw();
                let mut line = self.uart.lines();
                line.write_str("[ble] bonded devices: ");
                line.write_u32(u32::from(params[0]));
                for bond in bluenrg2::bonded_devices(params) {
                    line.write_str(" ");
                    line.write_str(wrist_bluenrg2::console::address(bond).as_str());
                }
                line.write_line("");
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
        p[1..].copy_from_slice(addr.addr.raw());
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
        self.shadow.operator_disconnect(now_ms());
        let _ = self.submit(Op::Disconnect(link(), DisconnectReason::RemoteUserTerminatedConn)).await;
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
        let _ = self.submit(Op::Passkey(ConnHandle::new(handle), key)).await;
    }

    /// Send a command and wait for its Command Complete (or Command Status), handling every
    /// other event that arrives meanwhile.
    async fn command(&mut self, opcode: u16, params: &[u8]) -> Result<Reply, &'static str> {
        self.exchange(opcode, params).await.map_err(unanswered)
    }

    async fn exchange(&mut self, opcode: u16, params: &[u8]) -> Result<Reply, Unanswered> {
        if self.chip.restarted() {
            return Err(Unanswered::Restarted);
        }
        #[cfg(feature = "ble-probe")]
        if opcode == bluenrg2::opcode::ACI_GAP_START_GENERAL_DISCOVERY_PROC {
            if let Some(status) = self.refuse_scan.take() {
                self.shadow.command(now_ms(), opcode, params);
                return Ok(Reply { status, params: [0; bluenrg2::RETURN_PARAMS_MAX], len: 0 });
            }
        }
        trace_command(opcode, params);
        self.shadow.command(now_ms(), opcode, params);
        self.chip.send(opcode, params).await?;
        while self.chip.awaiting().await {
            self.packets().await;
        }
        self.chip.reply()
    }

    /// `op` sent as its command, the driver told how it was answered.
    async fn submit(&mut self, op: Op) -> Result<Reply, &'static str> {
        self.settled(op).await.0
    }

    /// [`Link::submit`], with what the driver made of the answer.
    async fn settled(&mut self, op: Op) -> (Result<Reply, &'static str>, Result<wrist_ble::OpId, Failure>) {
        let Some(c) = commands::command(&op, DEVICE_NAME, |_| Some(LeConnRole::Peripheral), self.chip.server()) else {
            return (Err("not sent: no command carries it out"), Err(Failure { kind: FailureKind::Unknown, native: 0 }));
        };
        let reply = self.exchange(c.opcode, c.params()).await;
        let taken = self.chip.settle(&op, &reply);
        (reply.map_err(unanswered), taken)
    }

    /// The driver's snapshot for the core, which asks for one after lost events.
    async fn resync(&mut self) {
        if !self.shadow.take_snapshot() {
            return;
        }
        let mut snap = Snap::default();
        let mut packet = [0u8; transport::MAX_READ];
        while let Some(note) = self.chip.snapshotting(&mut snap, &mut packet).await {
            match note {
                Snapping::Got(got) => self.got(&packet, got),
                Snapping::Sending(c) => {
                    trace_command(c.opcode, c.params());
                    self.shadow.command(now_ms(), c.opcode, c.params());
                }
                Snapping::Taken(r) => {
                    match &r {
                        Ok(s) => {
                            self.uart.write_str("[ble] resynced: links ");
                            self.uart.write_u32(u32::try_from(s.links.iter().flatten().count()).unwrap_or(u32::MAX));
                            self.uart.write_line(if s.advertising { ", advertising" } else { ", not advertising" });
                        }
                        Err(_) => self.uart.write_line("[ble] resync: the chip gave no snapshot"),
                    }
                    #[cfg(feature = "ble-trace")]
                    trace::snapshot(r.as_ref().ok());
                    self.shadow.snapshotted(now_ms(), r);
                }
            }
        }
    }

    /// The core's operations carried out, each answered at its reply's place among the chip's
    /// packets; the rest wait for the next pass.
    async fn drive(&mut self) {
        for _ in 0..ISSUED_PER_PASS {
            let Some(op) = self.shadow.issue() else { return };
            let Some(c) = commands::command(&op, DEVICE_NAME, |_| Some(LeConnRole::Peripheral), self.chip.server()) else {
                self.shadow.submitted(now_ms(), Err(Failure { kind: FailureKind::Unknown, native: 0 }));
                continue;
            };
            let reply = self.answered(c.opcode, c.params()).await;
            let taken = self.chip.settle(&op, &reply);
            self.shadow.submitted(now_ms(), taken);
            self.show();
            self.issued(&op, reply.as_ref().map(|r| r.status).map_err(|why| unanswered(*why)), taken);
            // The rest of the drain the reply came in, and what the driver ended meanwhile.
            self.packets().await;
        }
    }

    /// [`Link::exchange`], handing out the chip's packets only up to the reply.
    async fn answered(&mut self, opcode: u16, params: &[u8]) -> Result<Reply, Unanswered> {
        if self.chip.restarted() {
            return Err(Unanswered::Restarted);
        }
        #[cfg(feature = "ble-probe")]
        if let Some(status) = self.refuse_next.take() {
            self.shadow.command(now_ms(), opcode, params);
            return Ok(Reply { status, params: [0; bluenrg2::RETURN_PARAMS_MAX], len: 0 });
        }
        trace_command(opcode, params);
        self.shadow.command(now_ms(), opcode, params);
        self.chip.send(opcode, params).await?;
        let mut packet = [0u8; transport::MAX_READ];
        while self.chip.awaiting().await {
            while !self.chip.replied() {
                let Some(got) = self.chip.next(&mut packet).await else { break };
                self.got(&packet, got);
            }
        }
        self.chip.reply()
    }

    /// The console line for the core's operation.
    fn issued(&mut self, op: &Op, status: Result<u8, &'static str>, taken: Result<wrist_ble::OpId, Failure>) {
        match (*op, status) {
            (Op::Advertise(Some(a)), Ok(0)) => {
                self.uart.write_str("[ble] advertising as UNA every ");
                self.uart.write_u32(u32::from(a.interval_min.as_u16()) * 625 / 1000);
                self.uart.write_line(" ms");
            }
            (Op::Advertise(Some(_)), Ok(s)) => {
                self.uart.write_str("[ble] advertising refused, status ");
                self.uart.write_hex8(s);
                self.uart.write_line("");
            }
            (Op::Advertise(Some(_)), Err(why)) => {
                self.uart.write_str("[ble] advertising: ");
                self.uart.write_line(why);
            }
            (Op::UpdateParams(..), Ok(0)) => {}
            (Op::UpdateParams(..), Ok(_)) if matches!(taken, Err(Failure { kind: FailureKind::Busy, .. })) => {
                self.uart.write_line("[ble] interval request held: the central has yet to answer the one before");
            }
            (Op::UpdateParams(..), Ok(s)) => {
                self.uart.write_str("[ble] interval request refused, status ");
                self.uart.write_hex8(s);
                self.uart.write_line("");
            }
            (Op::UpdateParams(..), Err(why)) => {
                self.uart.write_str("[ble] interval request: ");
                self.uart.write_line(why);
            }
            (Op::RequestSecurity(_), Ok(0)) => self.uart.write_line("[ble] link still unencrypted: asked the central to pair"),
            (Op::RequestSecurity(_), Ok(s)) => {
                self.uart.write_str("[ble] security request refused, status ");
                self.uart.write_hex8(s);
                self.uart.write_line("");
            }
            (Op::RequestSecurity(_), Err(why)) => {
                self.uart.write_str("[ble] security request: ");
                self.uart.write_line(why);
            }
            (Op::AllowRebond(_), _) => self.uart.write_line("[ble] the central lost its bond; allowing it to pair again"),
            (Op::ConfirmNumeric(_, yes), Ok(0)) => self.uart.write_line(if yes { "[ble] comparison confirmed" } else { "[ble] comparison rejected" }),
            (Op::ConfirmNumeric(..), Ok(s)) => {
                self.uart.write_str("[ble] comparison answer refused, status ");
                self.uart.write_hex8(s);
                self.uart.write_line("");
            }
            (Op::ConfirmNumeric(..), Err(why)) => {
                self.uart.write_str("[ble] comparison answer: ");
                self.uart.write_line(why);
            }
            _ => {}
        }
    }

    /// The core's word on a chip that is up: a start is a restart, a hold the radio's end (false).
    async fn core_chip(&mut self) -> bool {
        match self.shadow.chip_action() {
            Some(Action::Start(id)) if self.shadow.state().chip.starting() == Some(id) => {
                CORE_RESTARTS.fetch_add(1, Ordering::Relaxed);
                self.uart.write_line("[ble] the core restarts the chip");
                self.chip_gone();
                self.bring_up().await;
                self.recover().await
            }
            Some(Action::Hold) => {
                self.uart.write_line("[ble] the core gave up on the chip");
                false
            }
            _ => true,
        }
    }

    /// Advertising on or off for a scan, as the core's goal.
    fn want_discoverable(&mut self, on: bool) {
        let g = self.shadow.goals();
        self.shadow.set_goals(now_ms(), Goals { phone: PhoneGoal { discoverable: on, ..g.phone }, ..g });
    }

    /// The radio and the phone's link as the core's record has them, for the `ble` line and the page.
    fn show(&self) {
        set_state(self.shown.unwrap_or_else(|| Radio::of(self.shadow.state())) as u8);
        let params = self.shadow.phone_params();
        INTERVAL.store(params.map_or(0, |p| p.interval.as_u16()), Ordering::Relaxed);
        LATENCY.store(params.map_or(0, |p| p.latency), Ordering::Relaxed);
    }

    /// A status line for the core's status as the task lets the radio go, if it has changed.
    fn last_status(&mut self) {
        let now = self.shadow.now();
        let s = status(self.shadow.state(), now);
        if self.status_log.last(now, &s) {
            write_status_line(self.uart, &s);
        }
    }

    /// The core's counters and the radio onto the `ble` line, and a status line when the status
    /// has changed.
    fn publish(&mut self) {
        self.show();
        let now = self.shadow.now();
        let s = status(self.shadow.state(), now);
        TALLY.lock(|t| t.set(self.shadow.core_tally()));
        if self.status_log.due(now, &s) {
            write_status_line(self.uart, &s);
        }
        let c = self.shadow.counters();
        for (slot, v) in [
            (&CORE_ASKED, c.submitted),
            (&CORE_REFUSED, c.refused),
            (&CORE_FAILED, c.failed),
            (&CORE_TIMED_OUT, c.timed_out),
            (&CORE_REASKS, c.reasks),
            (&CORE_DIVERGENCES, c.divergences),
            (&CORE_BUSY_SPENT, c.busy_spent),
            (&CORE_DROPPED, c.dropped),
        ] {
            slot.store(v, Ordering::Relaxed);
        }
    }

    /// Every event waiting.
    async fn drain(&mut self) {
        self.chip.drain();
        self.packets().await;
    }

    /// What the drain under way hands out, each in turn.
    async fn packets(&mut self) {
        let mut packet = [0u8; transport::MAX_READ];
        while let Some(got) = self.chip.next(&mut packet).await {
            self.got(&packet, got);
        }
    }

    /// One thing a drain handed out, its bytes in `packet`, and the radio as it leaves the core.
    fn got(&mut self, packet: &[u8; transport::MAX_READ], got: Got) {
        self.take(packet, got);
        self.show();
    }

    fn take(&mut self, packet: &[u8; transport::MAX_READ], got: Got) {
        #[cfg(feature = "ble-probe")]
        if self.withhold_connection && matches!(got, Got::Packet { event: Some(RadioEvent::LinkUp { .. }), .. }) {
            self.withhold_connection = false;
            self.uart.write_line("[ble] probe: a connection withheld from the core, a lost read reported");
            if let Got::Packet { len, event } = got {
                self.lost_read();
                return self.handle(&packet[..len], event);
            }
        }
        if let Got::Packet { len, .. } | Got::Boot { len, .. } = got {
            #[cfg(feature = "ble-trace")]
            trace::packet(trace::Dir::FromChip, &packet[..len]);
            self.shadow.event(now_ms(), &packet[..len]);
        }
        match got {
            Got::Packet { len, event } => self.handle(&packet[..len], event),
            Got::Boot { reason, .. } => self.initialized(true, reason.unwrap_or(BootReason::new(0))),
            Got::Unreadable { header, first } => {
                if first {
                    self.uart.write_str("[ble] headers unreadable with IRQ high, last ");
                    write_hex(self.uart, &header);
                    self.uart.write_line("; backing off to one read per 500 ms");
                }
            }
            Got::Event(RadioEvent::EventsLost) => {
                self.uart.write_line("[ble] the driver lost events");
                self.lost_read();
            }
            Got::Event(e) => self.shadow.reported(now_ms(), e),
        }
    }

    /// The driver's lost read, for the core and the trace.
    fn lost_read(&mut self) {
        #[cfg(feature = "ble-trace")]
        trace::packet(trace::Dir::Lost, &[]);
        self.shadow.reported(now_ms(), RadioEvent::EventsLost);
    }

    /// One packet from the chip: capture a reply, act on an event.
    fn handle(&mut self, p: &[u8], event: Option<RadioEvent>) {
        match event {
            Some(RadioEvent::DataLength { tx, rx, tx_time, rx_time, .. }) => return self.data_length((tx, tx_time), (rx, rx_time)),
            Some(RadioEvent::ServerWrite { attr: server::RX, offset, more, data, .. }) => return self.console_write(offset != 0 || more, data.as_slice()),
            Some(RadioEvent::ServerSubscribe { attr: server::TX, cccd, .. }) => {
                self.subscribed = cccd == Cccd::Notify;
                return self.set_mirror();
            }
            Some(RadioEvent::LinkUp { link, peer, .. }) => return self.connected(link, peer),
            Some(RadioEvent::Pairing(PairingEvent { step: PairingStep::LocalKeyMissing, .. })) => {
                if let Some(peer) = self.peer {
                    old_key(|k| k.refused(peer));
                }
            }
            Some(RadioEvent::Pairing(PairingEvent { step: PairingStep::PeerKeyMissing, .. })) => {
                self.uart.write_line("[ble] the central has lost its bond; pair again from the central");
            }
            _ => {}
        }
        match bluenrg2::parse(p) {
            // Replies are the driver's.
            Event::CommandComplete { .. } | Event::CommandStatus { .. } => {}
            Event::LeConnectionFailed { .. } => {
                CONNECT_FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            Event::LeConnectionUpdateComplete { handle, conn_interval: iv, peripheral_latency: lat, .. } => {
                if self.shadow.state().phone.link() == Some(handle) {
                    self.uart.write_str("[ble] connection interval ");
                    write_interval(self.uart, iv.as_u16());
                    self.uart.write_str(", latency ");
                    self.uart.write_u32(u32::from(lat));
                    self.uart.write_line("");
                }
            }
            Event::LeConnectionUpdateFailed { status, .. } => {
                UPDATE_FAILURES.fetch_add(1, Ordering::Relaxed);
                LAST_UPDATE_FAILURE.store(status.into_inner(), Ordering::Relaxed);
            }
            Event::DisconnectionComplete { status, reason, .. } => {
                if status != BleStatus::SUCCESS {
                    DISCONNECT_FAILURES.fetch_add(1, Ordering::Relaxed);
                }
                self.link_gone();
                self.uart.write_str("[ble] disconnected, reason ");
                self.uart.write_hex8(reason.into_inner());
                self.uart.write_line(" (13 = the central hung up)");
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
            Event::LeAdvertisingReport { data, .. } => self.scanner.advertising(data, self.uart),
            Event::Vendor(e) => self.vendor(e),
            Event::LeUnknown { subevent_code, .. } => self.unknown("[ble] LE event ", event_code::LE_META, subevent_code),
            Event::Unknown { code, .. } => self.unknown("[ble] event ", code, 0),
            _ => {
                UNKNOWN.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn connected(&mut self, link: ConnHandle, peer: wrist_ble::PeerAddr) {
        CONN_HANDLE.store(link.raw(), Ordering::Relaxed);
        self.peer = Some(bluenrg2::Address { kind: peer.kind, addr: peer.addr });
        set_encrypted(false);
        self.subscribed = false;
        self.set_mirror();
        self.outbox = Outbox::default();
        // No request here: one crossing the central's own encryption start ended a
        // bonded macOS central's link (HARDWARE.md).
        self.mtu = DEFAULT_MTU;
        self.uart.write_line("[ble] connected");
    }

    /// The link's encryption, from either event that reports it, so a refresh after a lost
    /// change settles the same state.
    fn set_link_encrypted(&mut self, on: bool, line: &str) {
        set_encrypted(on);
        if on && !PROVEN.swap(true, Ordering::Relaxed) {
            PROVEN_NOW.signal(());
        }
        self.uart.write_line(line);
        self.set_mirror();
    }

    /// The link is gone: its encryption, the subscription and mirror, a pairing prompt and any
    /// transfer with it.
    fn link_gone(&mut self) {
        CONN_HANDLE.store(0, Ordering::Relaxed);
        set_encrypted(false);
        self.subscribed = false;
        self.peer = None;
        self.set_mirror();
        clear_prompt();
        if let Some(session) = self.transfer.take() {
            crate::update::abort(session);
        }
    }

    /// `ACI_BLUE_INITIALIZED`: the boot a reset waits for, or a restart nobody asked for, which
    /// leaves nothing of the link or the configuration; [`Link::serve`] brings the chip up again.
    fn initialized(&mut self, booting: bool, reason: BootReason) {
        let boot = restart::boot(booting, reason);
        let restarted = boot == Boot::Restarted;
        self.uart.write_str(if restarted { "[ble] the chip restarted by itself, reason " } else { "[ble] chip initialised, reason " });
        self.uart.write_hex8(reason.into_inner());
        self.uart.write_str(" (");
        self.uart.write_str(wrist_bluenrg2::console::boot_reason(reason));
        if !restarted {
            self.boot = Some(boot);
            return self.uart.write_line(")");
        }
        self.uart.write_line(if reason.is_updater() { "); resetting it out of the updater" } else { "); resetting it" });
        RESTARTS.fetch_add(1, Ordering::Relaxed);
        LAST_RESTART.store(reason.into_inner(), Ordering::Relaxed);
        self.chip_gone();
    }

    /// The chip is starting again: nothing of its link or the work it left survives.
    fn chip_gone(&mut self) {
        self.link_gone();
        clear_out();
        self.outbox = Outbox::default();
        self.mtu = DEFAULT_MTU;
        self.finish_transfer = false;
        self.passkey_for = None;
        self.recount_bonds = false;
    }

    /// `[ble] data length TX 251 octets in 2120 us, RX 27 octets in 328 us`.
    fn data_length(&self, tx: (u16, Micros), rx: (u16, Micros)) {
        for (label, (octets, time)) in [("[ble] data length TX ", tx), (" us, RX ", rx)] {
            self.uart.write_str(label);
            self.uart.write_u32(u32::from(octets));
            self.uart.write_str(" octets in ");
            self.uart.write_u32(time.0);
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

    /// A central wrote the console's RX: bytes for the parser or a transfer.
    fn console_write(&mut self, piece: bool, data: &[u8]) {
        if !ENCRYPTED.load(Ordering::Relaxed) {
            UNSECURED.fetch_add(1, Ordering::Relaxed);
            return self.uart.write_line("[ble] a console write over an unencrypted link: dropped");
        }
        if piece {
            FRAGMENTS.fetch_add(1, Ordering::Relaxed);
        }
        let crc = wrist_flash::plan::crc32(data);
        let write = self.writes.arrived(crc, data.len() > DEFAULT_VALUE_LEN, Instant::now().as_ticks(), DOUBLED_WITHIN.as_ticks());
        if write != Write::New {
            REPEATS.fetch_add(1, Ordering::Relaxed);
            self.uart.write_str(if write == Write::Doubled { "[ble] console write repeated within 7.5 ms, events " } else { "[ble] console write repeated, events " });
            self.uart.write_u32(TRANSPORT.reads.load(Ordering::Relaxed));
            self.uart.write_line("");
        }
        self.console_bytes(data);
    }

    fn vendor(&mut self, e: VendorEvent) {
        match e {
            VendorEvent::BlueInitialized { reason, .. } => self.initialized(false, reason.unwrap_or(BootReason::new(0))),
            VendorEvent::EventsLost { .. } => self.uart.write_line("[ble] the chip dropped events: the host read too slowly"),
            VendorEvent::PassKeyRequest { handle, .. } => {
                self.passkey_for = Some(handle.raw());
                PROMPT.lock(|p| p.set(Prompt::Asked));
            }
            VendorEvent::NumericComparison { value, .. } => {
                PROMPT.lock(|p| p.set(Prompt::Waiting(Pairing::Compare, value, Instant::now().as_millis())));
                crate::wake_display();
                self.uart.write_str("[ble] pairing: the central shows a number; if it is ");
                write_six_digits(self.uart, value);
                self.uart.write_line(", `ble yes` on the USB console (`ble no` rejects)");
            }
            VendorEvent::PairingComplete { status, reason, .. } => {
                let reason = reason.map_or(0, PairingReason::into_inner);
                LAST_PAIRING.store(status.into_inner().saturating_add(1), Ordering::Relaxed);
                LAST_PAIRING_REASON.store(reason, Ordering::Relaxed);
                clear_prompt();
                if status.is_success() {
                    old_key(OldKey::clear);
                    self.uart.write_line("[ble] paired");
                    self.recount_bonds = true;
                } else {
                    self.uart.write_str("[ble] pairing failed: status ");
                    self.uart.write_hex8(status.into_inner());
                    self.uart.write_str(" (1 timeout, 2 failed, 3/4 LTK missing), reason ");
                    self.uart.write_hex8(reason);
                    self.uart.write_line(" (4 confirm failed, B DHKey check, C comparison refused)");
                }
            }
            VendorEvent::SlaveSecurityInitiated => self.uart.write_line("[ble] security request sent"),
            VendorEvent::ExchangeMtuResponse { server_mtu, .. } => {
                self.mtu = usize::from(server_mtu).clamp(DEFAULT_MTU, ATT_MAX_MTU);
                self.uart.write_str("[ble] ATT MTU ");
                self.uart.write_u32(u32::try_from(self.mtu).unwrap_or(0));
                self.uart.write_line("");
            }
            VendorEvent::TxPoolAvailable { .. } if self.outbox.waits_for_pool() => {
                self.outbox.pool_available();
                self.console_at = Instant::now();
            }
            VendorEvent::TxPoolAvailable { .. } | VendorEvent::AttributeModified { .. } | VendorEvent::BondLost => {}
            VendorEvent::L2capUpdateResponse { accepted, .. } => {
                if !accepted {
                    UPDATES_REFUSED.fetch_add(1, Ordering::Relaxed);
                    self.uart.write_line("[ble] the central refused the interval request");
                }
            }
            VendorEvent::GapProcComplete { procedure, status, .. } => self.scanner.gap_proc_complete(procedure, status, self.uart),
            e => {
                UNKNOWN.fetch_add(1, Ordering::Relaxed);
                let [hi, lo] = e.code().to_be_bytes();
                self.uart.write_str("[ble] vendor event ");
                self.uart.write_hex8(hi);
                self.uart.write_hex8(lo);
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
    crate::redraw(wrist_logic::pages::Page::Bluetooth);
}

fn write_address(uart: Uart, a: bluenrg2::Address) {
    uart.write_str(wrist_bluenrg2::console::address(a).as_str());
}

fn clear_prompt() {
    PROMPT.lock(|p| p.set(Prompt::None));
    crate::wake_display();
}

/// A connection interval in 1.25 ms units as milliseconds, `48.75 ms`.
fn write_interval(uart: Uart, units: u16) {
    let hundredths = u32::from(units) * 125;
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

fn unanswered(why: Unanswered) -> &'static str {
    match why {
        Unanswered::TooLong => "not sent: parameters past 255 bytes",
        Unanswered::NotTaken => "the chip did not take the command",
        Unanswered::NoReply => "no reply within 1 s",
        Unanswered::Restarted => "not sent: the chip restarted",
    }
}

/// `ble-trace`: a command as it goes to the chip.
#[cfg_attr(not(feature = "ble-trace"), allow(unused_variables))]
fn trace_command(opcode: u16, params: &[u8]) {
    #[cfg(feature = "ble-trace")]
    {
        let mut packet = [0u8; bluenrg2::MAX_COMMAND];
        if let Some(n) = bluenrg2::command(opcode, &[params], &mut packet) {
            trace::packet(trace::Dir::ToChip, &packet[..n]);
        }
    }
}

/// The core's tally, as the ble task last copied it.
static TALLY: Mutex<CriticalSectionRawMutex, Cell<Tally>> = Mutex::new(Cell::new(Tally::NEW));

struct Text(Uart);

impl core::fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_str(s);
        Ok(())
    }
}

fn write_status_line(uart: Uart, s: &CoreStatus) {
    let _ = s.write_line(&mut Text(uart));
    uart.write_line("");
}

/// The clock the shadow keeps, as the trace stamps packets.
fn now_ms() -> wrist_ble::Instant {
    wrist_ble::Instant(Instant::now().as_millis())
}

fn shadow_power() -> goals::Power {
    match power() {
        Power::Cable => goals::Power::Cable,
        Power::Battery => goals::Power::Battery,
    }
}

/// What this driver works toward, as the shadow's core takes it: the radio on, the phone always
/// welcome, the timing the power source asks for.
fn shadow_goals() -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: shadow_power(), ..Goals::OFF }
}

/// Out of line, as built in the task it would widen the task's stack frame, at every poll, by
/// its size.
#[inline(never)]
fn shadow(uart: Uart) -> &'static mut Shadow<Said> {
    static SHADOW_CORE: StaticCell<Shadow<Said>> = StaticCell::new();
    let shadow = SHADOW_CORE.init_with(|| Shadow::new(wrist_bluenrg2::INFO, Goals::OFF, Said(uart)));
    shadow.set_mode(Mode::Drive);
    shadow
}

/// The shadow's disagreements, each a console line and one more on the `ble` line.
struct Said(Uart);

impl Observer for Said {
    fn event(&mut self, _: wrist_ble::Instant, e: &RadioEvent) {
        if let RadioEvent::Unknown { raw_code } = *e {
            SHADOW.fetch_add(1, Ordering::Relaxed);
            self.0.write_str("[ble] shadow: an event the core could not read, code 0x");
            raw_code.to_be_bytes().into_iter().for_each(|b| self.0.write_hex8(b));
            self.0.write_line("");
        }
    }

    fn disagreed(&mut self, d: Disagreement) {
        use core::fmt::Write;
        struct Out(Uart);
        impl Write for Out {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.0.write_str(s);
                Ok(())
            }
        }
        SHADOW.fetch_add(1, Ordering::Relaxed);
        let _ = write!(Out(self.0), "[ble] shadow: {d}");
        self.0.write_line("");
    }

    fn lost(&mut self, _: wrist_ble::Instant) {
        SHADOW.fetch_add(1, Ordering::Relaxed);
        self.0.write_line("[ble] shadow: an answer owed to the core found no room");
    }
}

/// The link the central holds.
fn link() -> ConnHandle {
    ConnHandle::new(CONN_HANDLE.load(Ordering::Relaxed))
}
