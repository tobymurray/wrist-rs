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
//! exists, so an encrypted link is a bonded central's. The console's RX characteristic takes
//! writes with response only, and only over an authenticated, encrypted link; ST's stack
//! does not apply those permissions to a write without response, so it is not offered. The
//! watch also checks in software: nothing written over a link that is not encrypted is acted
//! on, and the console's output is mirrored only to an encrypted link. The loader is never
//! accepted over the air.
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
//! The transport (ST's X-CUBE-BLE2 `hci_tl_interface.c`, for the handshake only; SPI mode 1):
//! to **send**, pull CS low, wait for IRQ high, clock `0A 00 00 00 00` and read the slave's
//! header back, bytes 1..2 little-endian being how much it can take; if enough, clock the
//! packet; CS high; wait for IRQ low. To **receive**, CS low, clock `0B 00 00 00 00`, bytes
//! 3..4 of the reply say how many bytes wait, clock that many zeros, wait for IRQ low, CS high.
//! IRQ high with CS high means an event is waiting. Reset is CS high, reset low 5 ms, high;
//! the stack announces `ACI_BLUE_INITIALIZED` about 2 s later, and until then MISO only
//! echoes MOSI. Pairing fails at the `DHKey` check until a public address has been written
//! (`ACI_HAL_WRITE_CONFIG_DATA` offset 0), which ST's samples do first.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

use embassy_futures::select::{select, select4, Either4};
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{bind_interrupts, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::pipe::Pipe;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use wrist_aci::opcode::{
    ACI_GAP_ALLOW_REBOND, ACI_GAP_CLEAR_SECURITY_DB, ACI_GAP_GET_BONDED_DEVICES, ACI_GAP_INIT,
    ACI_GAP_NUMERIC_COMPARISON_CONFIRM, ACI_GAP_PASS_KEY_RESP, ACI_GAP_SET_AUTHENTICATION_REQUIREMENT,
    ACI_GAP_SET_DISCOVERABLE, ACI_GAP_SET_IO_CAPABILITY, ACI_GAP_SET_NON_DISCOVERABLE, ACI_GAP_SLAVE_SECURITY_REQ,
    ACI_GAP_TERMINATE, ACI_GATT_ADD_CHAR, ACI_GATT_ADD_SERVICE, ACI_GATT_EXCHANGE_CONFIG, ACI_GATT_INIT,
    ACI_GATT_UPDATE_CHAR_VALUE, ACI_HAL_GET_FW_BUILD_NUMBER, ACI_HAL_SET_TX_POWER_LEVEL, ACI_HAL_WRITE_CONFIG_DATA,
    ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ, HCI_LE_RAND, HCI_READ_LOCAL_VERSION, HCI_SET_EVENT_MASK,
};
use wrist_aci::{permission, vendor, AttributeModified, Event};
use wrist_wake::ble::{timing, Power, Timing};

use crate::console::Target;
use crate::uart::Uart;
use crate::update::FlashMutex;

bind_interrupts!(struct Irqs {
    EXTI9 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI9>;
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
}

const SLAVE_READY_TIMEOUT: Duration = Duration::from_millis(100);
const IRQ_LOW_TIMEOUT: Duration = Duration::from_millis(1000);
const BOOT_TIMEOUT: Duration = Duration::from_millis(3000);
const REPLY_TIMEOUT: Duration = Duration::from_millis(1000);
/// How long a numeric comparison waits for `ble yes` before it is rejected.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// A transfer with no bytes for this long is abandoned, as over USB.
const TRANSFER_IDLE: Duration = Duration::from_secs(5);
/// One SPI read. The chip queues events back to back and announces them all in one count,
/// which passes 260 with two 217-byte console writes waiting.
const MAX_PACKET: usize = 1024;

const DEVICE_NAME: &[u8] = b"UNA";
// Bluetooth SIG assigned numbers: the Device Information service.
const UUID_DEVICE_INFORMATION: u16 = 0x180A;
const UUID_MANUFACTURER_NAME: u16 = 0x2A29;
const UUID_MODEL_NUMBER: u16 = 0x2A24;
const UUID_FIRMWARE_REVISION: u16 = 0x2A26;
const CHAR_PROP_READ: u8 = 0x02;
const CHAR_PROP_WRITE: u8 = 0x08;
const CHAR_PROP_NOTIFY: u8 = 0x10;
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
/// The characteristics' value size: the most an ATT MTU of 247 carries.
const NUS_VALUE_LEN: usize = 244;
/// The default ATT MTU, until an exchange raises it.
const DEFAULT_MTU: usize = 23;
/// `IO_CAP_DISPLAY_YES_NO`: the console shows the number, `ble yes` answers.
const IO_CAP_DISPLAY_YES_NO: u8 = 0x01;
/// The STM32U5's 96-bit unique id, for the address (RM0456 "Unique device ID").
const UID_BASE: u32 = 0x0BFA_0700;
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
    Disconnect,
    /// Ask for this interval range (1.25 ms units) on the current connection.
    IntervalNow(u16, u16),
}

static REQUESTS: Channel<CriticalSectionRawMutex, Request, 4> = Channel::new();
/// Raised by other tasks that leave the loop work it cannot see coming: console output for a
/// subscribed central. The loop clears it before it reads that state.
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Console output for the central, filled while [`MIRROR`] is set.
static OUT: Pipe<CriticalSectionRawMutex, 2048> = Pipe::new();
/// A central is subscribed to TX over an encrypted link.
static MIRROR: AtomicBool = AtomicBool::new(false);

const STATE_OFF: u8 = 0;
const STATE_STARTING: u8 = 1;
const STATE_IDLE: u8 = 2;
const STATE_ADVERTISING: u8 = 3;
const STATE_CONNECTED: u8 = 4;
const STATE_FAILED: u8 = 5;
static STATE: AtomicU8 = AtomicU8::new(STATE_OFF);
static ENCRYPTED: AtomicBool = AtomicBool::new(false);
/// An encrypted link was up at some point this boot: a way to send the next image.
static PROVEN: AtomicBool = AtomicBool::new(false);
static CONN_HANDLE: AtomicU16 = AtomicU16::new(0);
static EVENTS: AtomicU32 = AtomicU32::new(0);
/// Packets cut short by the end of a read, dropped rather than handled.
static CUT_SHORT: AtomicU32 = AtomicU32::new(0);
/// Console writes that arrived with a nonzero offset (fragments).
static FRAGMENTS: AtomicU32 = AtomicU32::new(0);
/// Writes dropped because the link was not encrypted.
static UNSECURED: AtomicU32 = AtomicU32::new(0);
/// The link's connection interval (1.25 ms units) and slave latency.
static INTERVAL: AtomicU16 = AtomicU16::new(0);
static LATENCY: AtomicU16 = AtomicU16::new(0);
/// Console writes identical to the one before (a write delivered twice).
static REPEATS: AtomicU32 = AtomicU32::new(0);
/// Reads and writes after which IRQ did not fall within `IRQ_LOW_TIMEOUT`.
static IRQ_STUCK_READ: AtomicU32 = AtomicU32::new(0);
static IRQ_STUCK_WRITE: AtomicU32 = AtomicU32::new(0);
static BONDED: AtomicU8 = AtomicU8::new(0);
/// Last pairing outcome: 0 none, else 1 + status (so 1 is success).
static LAST_PAIRING: AtomicU8 = AtomicU8::new(0);
static LAST_PAIRING_REASON: AtomicU8 = AtomicU8::new(0);
/// The pairing number waiting: kind 0 none, 1 passkey to type, 2 comparison to confirm.
static PROMPT_VALUE: AtomicU32 = AtomicU32::new(0);
static PROMPT_KIND: AtomicU8 = AtomicU8::new(0);
/// The answer to a comparison: 0 none, 1 yes, 2 no.
static ANSWER: AtomicU8 = AtomicU8::new(0);
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

/// Console output, mirrored to a subscribed central over an encrypted link. Never blocks:
/// what does not fit is dropped.
pub fn tee(bytes: &[u8]) {
    if MIRROR.load(Ordering::Relaxed) && OUT.try_write(bytes).is_ok() {
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

/// `ble`, `ble on|off|version|yes|no|bonds|forget|disconnect|profile auto|cable|battery|interval now <min> <max>`.
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
        b"disconnect" => Request::Disconnect,
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
                (Some(Some(min)), Some(Some(max))) if (6..=max).contains(&min) && max <= 3200 => {
                    Request::IntervalNow(min, max)
                }
                _ => return uart.write_line("[ble] usage: ble interval now <min> <max> (1.25 ms units, 6 to 3200)"),
            }
        }
        _ => {
            return uart.write_line(
                "[ble] usage: ble [on|off|version|yes|no|bonds|forget|disconnect|profile auto|cable|battery|interval now <min> <max>]",
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
    for i in (0..6).rev() {
        uart.write_hex8(ADDRESS[i].load(Ordering::Relaxed));
        if i > 0 {
            uart.write_str(":");
        }
    }
    uart.write_str(", bonded ");
    uart.write_u32(u32::from(BONDED.load(Ordering::Relaxed)));
    let n = |label: &str, v: &AtomicU32| {
        uart.write_str(label);
        uart.write_u32(v.load(Ordering::Relaxed));
    };
    n(", events ", &EVENTS);
    n(", cut short ", &CUT_SHORT);
    n(", fragments ", &FRAGMENTS);
    n(", repeats ", &REPEATS);
    n(", unsecured writes dropped ", &UNSECURED);
    n(", irq stuck after read ", &IRQ_STUCK_READ);
    n(" write ", &IRQ_STUCK_WRITE);
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
    match PROMPT_KIND.load(Ordering::Relaxed) {
        1 => uart.write_str("; TYPE this on the central: "),
        2 => uart.write_str("; COMPARE with the central's number, then `ble yes` or `ble no`: "),
        _ => return uart.write_line(""),
    }
    write_six_digits(uart, PROMPT_VALUE.load(Ordering::Relaxed));
    uart.write_line("");
}

/// A command's reply: the status byte and the parameters after it.
struct Reply {
    status: u8,
    params: [u8; 64],
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
                Request::Version | Request::Bonds | Request::Forget | Request::Disconnect | Request::IntervalNow(..) => {
                    uart.write_line("[ble] off; `ble on` starts it");
                }
                Request::Off | Request::Yes | Request::No => uart.write_line("[ble] already off"),
            }
        }
        if !crate::modules::allowed(crate::modules::Slot::Link1) {
            uart.write_line("[ble] not started: link1 holds another module (`modules force link1` allows it)");
            on = false;
            continue;
        }
        crate::rtc::set_radio_wanted(true);
        let mut config = spi::Config::default();
        config.frequency = Hertz(1_000_000);
        // ST's own bus setup for the BlueNRG-2: clock idle low, data sampled on the second
        // edge (X-CUBE-BLE2). Mode 0 gave bit-slipped headers.
        config.mode = spi::MODE_1;
        let mut link = Link {
            spi: Spi::new_blocking(pins.spi.reborrow(), pins.sck.reborrow(), pins.mosi.reborrow(), pins.miso.reborrow(), config),
            cs: Output::new(pins.cs.reborrow(), Level::High, Speed::VeryHigh),
            irq: ExtiInput::new(pins.irq.reborrow(), pins.irq_exti.reborrow(), Pull::Down, Irqs),
            rst: &mut rst,
            uart,
            flash,
            junk: 0,
            last_rx: 0,
            waiting_for: None,
            reply: None,
            readvertise: false,
            passkey_for: None,
            confirm_for: None,
            rebond_for: None,
            ask_security: false,
            nus: None,
            line: [0; 192],
            line_len: 0,
            mtu: DEFAULT_MTU,
            exchange_mtu: false,
            asked: None,
            advertised: None,
            request_interval: None,
            subscribed: false,
            transfer: None,
            last_data: Instant::from_ticks(0),
            finish_transfer: false,
            console_at: Instant::from_ticks(0),
        };
        link.bring_up().await;
        link.serve(&mut changes).await;
        // `ble off`: the chip in reset first, then SPI1, CS and IRQ dropped back to analog.
        link.rst.set_low();
        drop(link);
        MIRROR.store(false, Ordering::Relaxed);
        ENCRYPTED.store(false, Ordering::Relaxed);
        STATE.store(STATE_OFF, Ordering::Relaxed);
        crate::rtc::set_radio_wanted(false);
        uart.write_line("[ble] off: chip held in reset");
        on = false;
    }
}

// Each flag is one piece of work an event left for the loop.
#[allow(clippy::struct_excessive_bools)]
struct Link<'d> {
    spi: Spi<'d, Blocking, spi::mode::Master>,
    cs: Output<'d>,
    irq: ExtiInput<'d, Async>,
    rst: &'d mut Output<'static>,
    uart: Uart,
    flash: &'static FlashMutex,
    /// Consecutive unreadable headers: a stuck IRQ must not flood the log.
    junk: u32,
    /// CRC of the last console write, to spot one delivered twice.
    last_rx: u32,
    /// The opcode whose reply [`Link::command`] is waiting for, and the reply.
    waiting_for: Option<u16>,
    reply: Option<Reply>,
    /// Deferred work raised by events, done from the main loop.
    readvertise: bool,
    passkey_for: Option<u16>,
    confirm_for: Option<(u16, Instant)>,
    rebond_for: Option<u16>,
    ask_security: bool,
    /// The console service: service, RX and TX characteristic handles.
    nus: Option<(u16, u16, u16)>,
    line: [u8; 192],
    line_len: usize,
    /// The negotiated ATT MTU; notifications carry `mtu - 3`.
    mtu: usize,
    exchange_mtu: bool,
    /// The timing last asked of this connection, and the one advertising runs at.
    asked: Option<Timing>,
    advertised: Option<Timing>,
    /// `ble interval now`: ask for this range (1.25 ms units) once, whatever the timing.
    request_interval: Option<(u16, u16)>,
    /// The central wrote TX's CCCD on: the mirror runs while the link is encrypted too.
    subscribed: bool,
    /// An image arriving through RX, and when its last bytes came.
    transfer: Option<crate::update::Session>,
    last_data: Instant,
    finish_transfer: bool,
    /// The next console notification may go at this instant ([`CONSOLE_PACE`]).
    console_at: Instant,
}

impl Link<'_> {
    /// Until `ble off`. Each pass does what is due, then sleeps until the chip's IRQ, a
    /// request, [`WAKE`], a power change or the earliest deadline.
    async fn serve(&mut self, changes: &mut crate::power::Changes) {
        loop {
            WAKE.reset();
            self.deferred().await;
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
            match select4(REQUESTS.receive(), self.irq.wait_for_high(), timer, select(WAKE.wait(), power_changed)).await {
                Either4::First(Request::Off) => {
                    if let Some(session) = self.transfer.take() {
                        crate::update::abort(session);
                    }
                    let _ = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await;
                    return;
                }
                Either4::First(req) => self.request(req).await,
                Either4::Second(()) => {
                    self.drain().await;
                    // Unreadable headers with IRQ high: one read per 500 ms (the IRQ is still
                    // high, so the next wait returns at once).
                    if self.junk >= 3 {
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
            work: self.exchange_mtu
                || self.ask_security
                || self.request_interval.is_some()
                || self.finish_transfer
                || self.rebond_for.is_some()
                || self.passkey_for.is_some()
                || self.readvertise,
            confirm_by: self.confirm_for.map(|(_, since)| tick(since + CONFIRM_TIMEOUT + one)),
            transfer: match &self.transfer {
                None => Transfer::None,
                Some(s) if s.started() => Transfer::Started,
                Some(_) => Transfer::Claimed,
            },
            console: (self.nus.is_some() && MIRROR.load(Ordering::Relaxed))
                .then(|| Console { pending: !OUT.is_empty(), next_at: tick(self.console_at) }),
            abandon_at: self.transfer.as_ref().map(|_| tick(self.last_data + TRANSFER_IDLE + one)),
        };
        wrist_wake::ble::due(&state).map(Instant::from_ticks)
    }

    async fn request(&mut self, req: Request) {
        match req {
            Request::On => self.uart.write_line("[ble] already on"),
            Request::Off => {}
            Request::Version => self.version().await,
            Request::Yes | Request::No => {
                if self.confirm_for.is_none() {
                    return self.uart.write_line("[ble] no comparison is waiting");
                }
                ANSWER.store(if req == Request::Yes { 1 } else { 2 }, Ordering::Relaxed);
            }
            Request::Bonds => self.list_bonds().await,
            Request::Forget => {
                if self.command(ACI_GAP_CLEAR_SECURITY_DB, &[]).await.is_ok() {
                    BONDED.store(0, Ordering::Relaxed);
                    self.uart.write_line("[ble] bond database cleared; forget the watch on the central too");
                }
            }
            Request::Disconnect => {
                let h = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
                // Reason 0x13: remote user terminated connection.
                let _ = self.command(ACI_GAP_TERMINATE, &[h[0], h[1], 0x13]).await;
            }
            Request::IntervalNow(min, max) => {
                if STATE.load(Ordering::Relaxed) == STATE_CONNECTED {
                    self.request_interval = Some((min, max));
                } else {
                    self.uart.write_line("[ble] interval now: not connected");
                }
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
            // After the MTU exchange and the security request, which come first.
            STATE_CONNECTED if !self.exchange_mtu && !self.ask_security && self.asked != Some(want) => {
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

    /// Work raised by events: the MTU exchange and security request after a connection, a
    /// finished transfer, a rebond, a passkey, a comparison's answer (or its rejection after
    /// the timeout), a quiet transfer abandoned, re-advertising after a disconnect.
    async fn deferred(&mut self) {
        let h = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
        if self.exchange_mtu {
            self.exchange_mtu = false;
            let _ = self.command(ACI_GATT_EXCHANGE_CONFIG, &h).await;
        } else if self.ask_security {
            self.ask_security = false;
            // A bonded central encrypts at once; another is asked to pair.
            let _ = self.command(ACI_GAP_SLAVE_SECURITY_REQ, &h).await;
        } else if let Some((min, max)) = self.request_interval.take() {
            // Latency 0, 5 s supervision timeout (10 ms units).
            self.ask_connection(min, max, 0, 500).await;
        }
        if self.finish_transfer {
            self.finish_transfer = false;
            if let Some(session) = self.transfer.take() {
                // What queued while the mirror waited is stale; the result line must find room.
                OUT.clear();
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
        if let Some(h) = self.passkey_for.take() {
            self.answer_passkey(h).await;
        }
        if let Some((h, since)) = self.confirm_for {
            let answer = ANSWER.load(Ordering::Relaxed);
            let timed_out = since.elapsed() > CONFIRM_TIMEOUT;
            if answer != 0 || timed_out {
                self.confirm_for = None;
                let yes = answer == 1;
                let hb = h.to_le_bytes();
                let _ = self.command(ACI_GAP_NUMERIC_COMPARISON_CONFIRM, &[hb[0], hb[1], u8::from(yes)]).await;
                self.uart.write_line(if yes {
                    "[ble] comparison confirmed"
                } else if timed_out {
                    "[ble] comparison not answered in 60 s: rejected"
                } else {
                    "[ble] comparison rejected"
                });
                clear_prompt();
            }
        }
        if self.readvertise {
            self.readvertise = false;
            self.advertise().await;
        }
    }

    /// Reset, address, TX power, event mask, GATT, GAP, name, IO capability, security
    /// requirements, advertise: ST's sample order.
    async fn bring_up(&mut self) {
        STATE.store(STATE_STARTING, Ordering::Relaxed);
        clear_prompt();
        self.reset().await;
        let addr = address_from_uid();
        for (slot, b) in ADDRESS.iter().zip(addr.iter()) {
            slot.store(*b, Ordering::Relaxed);
        }
        let mut cfg = [0u8; 8];
        cfg[0] = 0x00;
        cfg[1] = 6;
        cfg[2..].copy_from_slice(&addr);
        let early: [(&str, u16, &[u8]); 4] = [
            ("public address", ACI_HAL_WRITE_CONFIG_DATA, &cfg),
            ("TX power", ACI_HAL_SET_TX_POWER_LEVEL, &[0x01, 0x04]),
            ("event mask", HCI_SET_EVENT_MASK, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xBF, 0x3D]),
            ("GATT init", ACI_GATT_INIT, &[]),
        ];
        for (what, opcode, params) in early {
            if !self.step(what, opcode, params).await {
                return;
            }
        }
        // GAP init: peripheral, no privacy, a name of up to 7 bytes. The reply carries the GAP
        // service and device-name handles.
        let (service, name_char) = match self.command(ACI_GAP_INIT, &[0x01, 0x00, 0x07]).await {
            Ok(r) if r.status == 0 && r.len >= 6 => {
                (u16::from_le_bytes([r.params[0], r.params[1]]), u16::from_le_bytes([r.params[2], r.params[3]]))
            }
            _ => return self.fail("GAP init"),
        };
        let mut name = [0u8; 16];
        name[0..2].copy_from_slice(&service.to_le_bytes());
        name[2..4].copy_from_slice(&name_char.to_le_bytes());
        name[4] = 0;
        name[5] = u8::try_from(DEVICE_NAME.len()).unwrap_or(0);
        name[6..6 + DEVICE_NAME.len()].copy_from_slice(DEVICE_NAME);
        let late: [(&str, u16, &[u8]); 3] = [
            ("device name", ACI_GATT_UPDATE_CHAR_VALUE, &name[..6 + DEVICE_NAME.len()]),
            ("IO capability", ACI_GAP_SET_IO_CAPABILITY, &[IO_CAP_DISPLAY_YES_NO]),
            // Bonding, MITM required, Secure Connections supported (optional), no keypress
            // notifications, key size 7 to 16, no fixed pin (the host is asked), public
            // identity address.
            (
                "security requirements",
                ACI_GAP_SET_AUTHENTICATION_REQUIREMENT,
                &[0x01, 0x01, 0x01, 0x00, 0x07, 0x10, 0x01, 0, 0, 0, 0, 0x00],
            ),
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
        for i in (0..6).rev() {
            self.uart.write_hex8(addr[i]);
            if i > 0 {
                self.uart.write_str(":");
            }
        }
        self.uart.write_line("");
        self.list_bonds().await;
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
            if !self.update_value(dis, ch, &value[..n]).await {
                return false;
            }
        }
        let Some(nus) = self.add_service_128(NUS_SERVICE, 6).await else {
            self.fail("adding the console service");
            return false;
        };
        let secured = permission::AUTHEN_WRITE | permission::ENCRY_WRITE;
        let (Some(rx), Some(tx)) = (
            self.add_char_128(nus, NUS_RX, CHAR_PROP_WRITE, secured, 0x01).await,
            self.add_char_128(nus, NUS_TX, CHAR_PROP_NOTIFY, permission::NONE, 0x00).await,
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
        p[0] = 0x02;
        p[1..17].copy_from_slice(&NUS_BASE);
        p[13] = last;
        p[17] = 0x01;
        p[18] = max_records;
        match self.command(ACI_GATT_ADD_SERVICE, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => None,
        }
    }

    /// A variable-length characteristic with the console's 128-bit UUID, byte 12 `last`;
    /// `evt_mask` 1 reports the central's writes to us.
    async fn add_char_128(&mut self, service: u16, last: u8, props: u8, security: u8, evt_mask: u8) -> Option<u16> {
        let mut p = [0u8; 26];
        p[0..2].copy_from_slice(&service.to_le_bytes());
        p[2] = 0x02;
        p[3..19].copy_from_slice(&NUS_BASE);
        p[15] = last;
        p[19..21].copy_from_slice(&u16::try_from(NUS_VALUE_LEN).unwrap_or(0).to_le_bytes());
        p[21] = props;
        p[22] = security;
        p[23] = evt_mask;
        p[24] = 0x07;
        p[25] = 0x01;
        match self.command(ACI_GATT_ADD_CHAR, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => None,
        }
    }

    async fn add_service(&mut self, uuid: u16, max_records: u8) -> Option<u16> {
        let u = uuid.to_le_bytes();
        // 16-bit UUID, primary service.
        match self.command(ACI_GATT_ADD_SERVICE, &[0x01, u[0], u[1], 0x01, max_records]).await {
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
        // No security permissions, no write events, key size 7, variable length.
        let p = [sv[0], sv[1], 0x01, u[0], u[1], l[0], l[1], CHAR_PROP_READ, permission::NONE, 0x00, 0x07, 0x01];
        match self.command(ACI_GATT_ADD_CHAR, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => {
                self.fail("adding a characteristic");
                None
            }
        }
    }

    async fn update_value(&mut self, service: u16, ch: u16, value: &[u8]) -> bool {
        let mut p = [0u8; 6 + NUS_VALUE_LEN];
        p[0..2].copy_from_slice(&service.to_le_bytes());
        p[2..4].copy_from_slice(&ch.to_le_bytes());
        p[4] = 0;
        let n = value.len().min(NUS_VALUE_LEN);
        p[5] = u8::try_from(n).unwrap_or(0);
        p[6..6 + n].copy_from_slice(&value[..n]);
        matches!(self.command(ACI_GATT_UPDATE_CHAR_VALUE, &p[..6 + n]).await, Ok(r) if r.status == 0)
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
        let mut chunk = [0u8; NUS_VALUE_LEN];
        let most = (self.mtu.saturating_sub(3)).clamp(20, NUS_VALUE_LEN);
        let n = OUT.try_read(&mut chunk[..most]).unwrap_or(0);
        if n == 0 {
            return;
        }
        let sent = self.update_value(nus, tx, &chunk[..n]).await;
        self.console_at = Instant::now() + CONSOLE_PACE;
        if !sent {
            // One notification refused (the chip busy right after a flash stall, say): that
            // chunk is lost and the mirror stays on.
            self.uart.write_line("[ble] a console notification was refused");
        }
    }

    /// The mirror runs while a central is subscribed over an encrypted link.
    fn set_mirror(&mut self) {
        let on = self.subscribed && ENCRYPTED.load(Ordering::Relaxed);
        if on != MIRROR.swap(on, Ordering::Relaxed) {
            OUT.clear();
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
        STATE.store(STATE_FAILED, Ordering::Relaxed);
        self.uart.write_str("[ble] start failed at ");
        self.uart.write_line(step);
    }

    /// `ACI_GAP_SET_DISCOVERABLE`: connectable undirected at the power source's interval,
    /// public address, no filter, name `UNA`, no UUIDs, no interval hint.
    async fn advertise(&mut self) {
        let want = timing(power(), false);
        let mut p = [0u8; 20];
        p[0] = 0x00;
        p[1..3].copy_from_slice(&want.advertise.to_le_bytes());
        p[3..5].copy_from_slice(&want.advertise.to_le_bytes());
        p[5] = 0x00;
        p[6] = 0x00;
        p[7] = u8::try_from(1 + DEVICE_NAME.len()).unwrap_or(0);
        p[8] = 0x09;
        p[9..9 + DEVICE_NAME.len()].copy_from_slice(DEVICE_NAME);
        let n = 9 + DEVICE_NAME.len();
        // Service UUID length 0, then slave connection interval min and max 0.
        p[n..n + 5].copy_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00]);
        match self.command(ACI_GAP_SET_DISCOVERABLE, &p[..n + 5]).await {
            Ok(r) if r.status == 0 => {
                STATE.store(STATE_ADVERTISING, Ordering::Relaxed);
                self.advertised = Some(want);
                self.uart.write_str("[ble] advertising as UNA every ");
                self.uart.write_u32(u32::from(want.advertise) * 625 / 1000);
                self.uart.write_line(" ms");
            }
            Ok(r) => {
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

    async fn list_bonds(&mut self) {
        if let Ok(r) = self.command(ACI_GAP_GET_BONDED_DEVICES, &[]).await {
            if r.len >= 1 {
                BONDED.store(r.params[0], Ordering::Relaxed);
                self.uart.write_str("[ble] bonded devices: ");
                self.uart.write_u32(u32::from(r.params[0]));
                for i in 0..usize::from(r.params[0]).min(4) {
                    let e = &r.params[1 + i * 7..];
                    if e.len() < 7 {
                        break;
                    }
                    self.uart.write_str(" ");
                    for j in (1..7).rev() {
                        self.uart.write_hex8(e[j]);
                        if j > 1 {
                            self.uart.write_str(":");
                        }
                    }
                }
                self.uart.write_line("");
            }
        }
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
        PROMPT_VALUE.store(key, Ordering::Relaxed);
        PROMPT_KIND.store(1, Ordering::Relaxed);
        self.uart.write_str("[ble] pairing: type this passkey on the central: ");
        write_six_digits(self.uart, key);
        self.uart.write_line("");
        let mut p = [0u8; 6];
        p[0..2].copy_from_slice(&handle.to_le_bytes());
        p[2..6].copy_from_slice(&key.to_le_bytes());
        let _ = self.command(ACI_GAP_PASS_KEY_RESP, &p).await;
    }

    async fn reset(&mut self) {
        self.cs.set_high();
        self.rst.set_low();
        Timer::after_millis(5).await;
        self.rst.set_high();
        self.waiting_for = None;
        self.reply = None;
        CONN_HANDLE.store(0, Ordering::Relaxed);
        // The stack announces itself about 2 s later; until then MISO only echoes MOSI, so
        // wait for the event before any command.
        if with_timeout(BOOT_TIMEOUT, self.irq.wait_for_high()).await.is_ok() {
            self.drain().await;
        } else {
            self.uart.write_line("[ble] no sign of life after reset (no module, or not ST's firmware)");
        }
    }

    /// Send a command and wait for its Command Complete (or Command Status), handling every
    /// other event that arrives meanwhile.
    async fn command(&mut self, opcode: u16, params: &[u8]) -> Result<Reply, &'static str> {
        let mut packet = [0u8; 4 + 255];
        let n = wrist_aci::command(opcode, params, &mut packet);
        self.waiting_for = Some(opcode);
        self.reply = None;
        if !self.send(&packet[..n]).await {
            self.waiting_for = None;
            return Err("the chip did not take the command");
        }
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while self.reply.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left == Duration::from_ticks(0) {
                break;
            }
            if with_timeout(left, self.irq.wait_for_high()).await.is_err() {
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
        while self.irq.is_high() && rounds < 8 {
            rounds += 1;
            let mut buf = [0u8; MAX_PACKET];
            let mut header = wrist_aci::HEADER_READ;
            self.cs.set_low();
            let _ = self.spi.blocking_transfer_in_place(&mut header);
            let (wbuf, announced) = wrist_aci::header_sizes(&header);
            let count = announced.min(MAX_PACKET);
            if count > 0 {
                let _ = self.spi.blocking_transfer_in_place(&mut buf[..count]);
            }
            if with_timeout(IRQ_LOW_TIMEOUT, self.irq.wait_for_low()).await.is_err() {
                IRQ_STUCK_READ.fetch_add(1, Ordering::Relaxed);
            }
            self.cs.set_high();
            if (1..=1024).contains(&wbuf) && count <= MAX_PACKET {
                self.junk = 0;
            } else {
                self.junk += 1;
                if self.junk == 3 {
                    self.uart.write_str("[ble] headers unreadable with IRQ high, last ");
                    write_hex(self.uart, &header);
                    self.uart.write_line("; backing off to one read per 500 ms");
                }
                if self.junk >= 3 {
                    return;
                }
            }
            if count == 0 {
                Timer::after_millis(10).await;
                continue;
            }
            EVENTS.fetch_add(1, Ordering::Relaxed);
            // The read may hold several packets back to back.
            let mut at = 0;
            while at < count {
                let len = wrist_aci::packet_len(&buf[at..count]);
                if len == 0 {
                    break;
                }
                if !wrist_aci::packet_whole(&buf[at..count]) {
                    // Half an event handled would feed half a console write into a transfer,
                    // and the rest would never come.
                    CUT_SHORT.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                self.handle(&buf[at..at + len]);
                at += len;
            }
            embassy_futures::yield_now().await;
        }
    }

    /// One packet from the chip: capture a reply, act on an event.
    fn handle(&mut self, p: &[u8]) {
        let Some(event) = wrist_aci::parse(p) else {
            return;
        };
        match event {
            Event::CommandComplete { opcode, status, params } => {
                if self.waiting_for == Some(opcode) {
                    let mut reply = Reply { status, params: [0; 64], len: params.len().min(64) };
                    reply.params[..reply.len].copy_from_slice(&params[..reply.len]);
                    self.reply = Some(reply);
                }
            }
            Event::CommandStatus { opcode, status } => {
                if self.waiting_for == Some(opcode) {
                    self.reply = Some(Reply { status, params: [0; 64], len: 0 });
                }
            }
            Event::Connected { handle, interval } => {
                CONN_HANDLE.store(handle, Ordering::Relaxed);
                if let Some((iv, lat)) = interval {
                    INTERVAL.store(iv, Ordering::Relaxed);
                    LATENCY.store(lat, Ordering::Relaxed);
                }
                STATE.store(STATE_CONNECTED, Ordering::Relaxed);
                ENCRYPTED.store(false, Ordering::Relaxed);
                self.subscribed = false;
                self.set_mirror();
                self.mtu = DEFAULT_MTU;
                self.exchange_mtu = true;
                self.ask_security = true;
                self.asked = None;
                self.advertised = None;
                self.uart.write_line("[ble] connected");
            }
            Event::ConnectionUpdate { interval: iv, latency: lat } => {
                INTERVAL.store(iv, Ordering::Relaxed);
                LATENCY.store(lat, Ordering::Relaxed);
                self.uart.write_str("[ble] connection interval ");
                write_interval(self.uart, iv);
                self.uart.write_str(", latency ");
                self.uart.write_u32(u32::from(lat));
                self.uart.write_line("");
            }
            Event::Disconnected { reason } => {
                STATE.store(STATE_IDLE, Ordering::Relaxed);
                CONN_HANDLE.store(0, Ordering::Relaxed);
                ENCRYPTED.store(false, Ordering::Relaxed);
                self.subscribed = false;
                self.set_mirror();
                self.confirm_for = None;
                self.exchange_mtu = false;
                self.ask_security = false;
                self.request_interval = None;
                clear_prompt();
                if let Some(session) = self.transfer.take() {
                    crate::update::abort(session);
                }
                self.uart.write_str("[ble] disconnected, reason ");
                self.uart.write_hex8(reason);
                self.uart.write_line(" (13 = the central hung up)");
                self.readvertise = true;
            }
            Event::Encryption { on } => {
                let on = on != 0;
                ENCRYPTED.store(on, Ordering::Relaxed);
                if on && !PROVEN.swap(true, Ordering::Relaxed) {
                    PROVEN_NOW.signal(());
                }
                self.uart.write_line(if on { "[ble] link encrypted" } else { "[ble] link encryption off" });
                self.set_mirror();
            }
            Event::Vendor { code, params } => self.vendor(code, params),
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
            if data.len() > 20 && crc == self.last_rx {
                REPEATS.fetch_add(1, Ordering::Relaxed);
                self.uart.write_str("[ble] console write repeated, events ");
                self.uart.write_u32(EVENTS.load(Ordering::Relaxed));
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
        let handle = wrist_aci::connection_handle;
        match ecode {
            vendor::BLUE_INITIALIZED => self.uart.write_line("[ble] chip initialised"),
            vendor::HAL_EVENTS_LOST => self.uart.write_line("[ble] the chip dropped events: the host read too slowly"),
            vendor::GAP_PASS_KEY_REQ => self.passkey_for = Some(handle(rest)),
            vendor::GAP_NUMERIC_COMPARISON_VALUE if rest.len() >= 6 => {
                let (conn, value) = wrist_aci::numeric_comparison(rest).unwrap_or_default();
                PROMPT_VALUE.store(value, Ordering::Relaxed);
                PROMPT_KIND.store(2, Ordering::Relaxed);
                ANSWER.store(0, Ordering::Relaxed);
                self.confirm_for = Some((conn, Instant::now()));
                self.uart.write_str("[ble] pairing: the central shows a number; if it is ");
                write_six_digits(self.uart, value);
                self.uart.write_line(", `ble yes` on the USB console (`ble no` rejects)");
            }
            vendor::GAP_PAIRING_COMPLETE if rest.len() >= 4 => {
                let (status, reason) = wrist_aci::pairing_complete(rest).unwrap_or_default();
                LAST_PAIRING.store(status.saturating_add(1), Ordering::Relaxed);
                LAST_PAIRING_REASON.store(reason, Ordering::Relaxed);
                self.confirm_for = None;
                clear_prompt();
                if status == 0 {
                    self.uart.write_line("[ble] paired and bonded");
                } else {
                    self.uart.write_str("[ble] pairing failed: status ");
                    self.uart.write_hex8(status);
                    self.uart.write_str(" (1 timeout, 2 failed, 3/4 LTK missing), reason ");
                    self.uart.write_hex8(reason);
                    self.uart.write_line(" (4 confirm failed, B DHKey check, C comparison refused)");
                }
            }
            vendor::GAP_BOND_LOST => self.rebond_for = Some(handle(rest)),
            vendor::ATT_EXCHANGE_MTU_RESP if rest.len() >= 4 => {
                self.mtu = usize::from(wrist_aci::att_mtu(rest).unwrap_or(0)).clamp(DEFAULT_MTU, 247);
                self.uart.write_str("[ble] ATT MTU ");
                self.uart.write_u32(u32::try_from(self.mtu).unwrap_or(0));
                self.uart.write_line("");
            }
            vendor::GAP_SLAVE_SECURITY_INITIATED => self.uart.write_line("[ble] security requested"),
            // The MTU exchange ends with this; nothing else of ours is a client procedure.
            vendor::GATT_PROC_COMPLETE => {}
            vendor::GATT_ATTRIBUTE_MODIFIED if rest.len() >= 8 => {
                if let Some(write) = AttributeModified::decode(rest) {
                    self.attribute_modified(write);
                }
            }
            _ => {
                self.uart.write_str("[ble] vendor event ");
                self.uart.write_hex8((ecode >> 8) as u8);
                self.uart.write_hex8((ecode & 0xFF) as u8);
                self.uart.write_line("");
            }
        }
    }

    /// The `0A` handshake; true when the chip took the packet.
    async fn send(&mut self, packet: &[u8]) -> bool {
        for _attempt in 0..5 {
            self.cs.set_low();
            if with_timeout(SLAVE_READY_TIMEOUT, self.irq.wait_for_high()).await.is_err() {
                self.cs.set_high();
                Timer::after_millis(10).await;
                continue;
            }
            let mut header = wrist_aci::HEADER_WRITE;
            let _ = self.spi.blocking_transfer_in_place(&mut header);
            let (wbuf, _) = wrist_aci::header_sizes(&header);
            // The BlueNRG-2 sends no ready byte, only buffer sizes; ST checks the size alone,
            // so do we.
            let accepted = (1..=1024).contains(&wbuf) && wbuf >= packet.len();
            if accepted {
                let mut out = [0u8; 4 + 255];
                out[..packet.len()].copy_from_slice(packet);
                let _ = self.spi.blocking_transfer_in_place(&mut out[..packet.len()]);
            }
            self.cs.set_high();
            if accepted {
                if with_timeout(IRQ_LOW_TIMEOUT, self.irq.wait_for_low()).await.is_err() {
                    IRQ_STUCK_WRITE.fetch_add(1, Ordering::Relaxed);
                }
                return true;
            }
            Timer::after_millis(10).await;
        }
        false
    }
}

fn clear_prompt() {
    PROMPT_KIND.store(0, Ordering::Relaxed);
    PROMPT_VALUE.store(0, Ordering::Relaxed);
    ANSWER.store(0, Ordering::Relaxed);
}

/// A connection interval in 1.25 ms units as milliseconds, `48.75 ms`.
fn write_interval(uart: Uart, units: u16) {
    let hundredths = wrist_aci::interval_hundredths_ms(units);
    uart.write_u32(hundredths / 100);
    uart.write_str(if hundredths % 100 < 10 { ".0" } else { "." });
    uart.write_u32(hundredths % 100);
    uart.write_str(" ms");
}

/// A locally administered public address from the MCU's unique id: `02:80:E1` (ST's prefix
/// with the local bit) and three bytes folded from the 96-bit id. Little-endian, as the chip
/// and the air carry it.
fn address_from_uid() -> [u8; 6] {
    let w0 = unsafe { core::ptr::read_volatile(UID_BASE as *const u32) };
    let w1 = unsafe { core::ptr::read_volatile((UID_BASE + 4) as *const u32) };
    let w2 = unsafe { core::ptr::read_volatile((UID_BASE + 8) as *const u32) };
    let fold = w0 ^ w1.rotate_left(11) ^ w2.rotate_left(22);
    let b = fold.to_le_bytes();
    [b[0], b[1], b[2], 0xE1, 0x80, 0x02]
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
