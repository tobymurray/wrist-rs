//! The USB CDC-ACM console: the UART's output mirrored to a host, and commands
//! from it (`HARDWARE.md`, "USB device").

use core::sync::atomic::{AtomicBool, Ordering};
use embassy_futures::select::{select4, Either4};
use embassy_stm32::usb::{self, Driver};
use embassy_stm32::{bind_interrupts, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_time::{Instant, Timer};
use embassy_usb::driver::EndpointError;
use embassy_usb::class::cdc_acm::{CdcAcmClass, Receiver, Sender, State};
use embassy_usb::{Builder, Handler};
use static_cell::ConstStaticCell;

use crate::mmio::{modify32, read32, PWR_SVMSR, PWR_VOSR, RCC_AHB2ENR1};
use crate::uart::Uart;
use crate::update::{self, FlashMutex};
use crate::{usblog, watchdog};

const OTG_HS_BASE: u32 = 0x4204_0000;
const OTG_GOTGCTL: u32 = OTG_HS_BASE;
const OTG_GUSBCFG: u32 = OTG_HS_BASE + 0x00C;
const OTG_GINTSTS: u32 = OTG_HS_BASE + 0x014;
const OTG_GCCFG: u32 = OTG_HS_BASE + 0x038;
const OTG_DCFG: u32 = OTG_HS_BASE + 0x800;
const OTG_DCTL: u32 = OTG_HS_BASE + 0x804;
const OTG_DSTS: u32 = OTG_HS_BASE + 0x808;
const OTG_PCGCCTL: u32 = OTG_HS_BASE + 0xE00;
const GINTSTS_USBRST: u32 = 1 << 12;
const DCFG_DAD: u32 = 0x7F << 4;
const DCTL_SDIS: u32 = 1 << 1;
const RCC_AHB2ENR1_OTGEN: u32 = 1 << 14;
const SYSCFG_OTGHSPHYCR: u32 = 0x4600_0400 + 0x74;
const SYSCFG_OTGHSPHYTUNER2: u32 = 0x4600_0400 + 0x7C;

static BUILT: AtomicBool = AtomicBool::new(false);
static TERMINAL_OPEN: AtomicBool = AtomicBool::new(false);
static HOST_CONFIGURED: AtomicBool = AtomicBool::new(false);
static HOST_ACTIVE: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn host_configured() -> bool {
    HOST_CONFIGURED.load(Ordering::Relaxed)
}

/// Configured now and not suspended, as an unplugged cable leaves it.
pub fn host_active() -> bool {
    HOST_ACTIVE.load(Ordering::Relaxed)
}

pub fn quiet(on: bool) {
    QUIET.store(on, Ordering::Relaxed);
}

/// Whether the block's registers can be read without a bus fault.
fn clocked() -> bool {
    BUILT.load(Ordering::Relaxed) && unsafe { read32(RCC_AHB2ENR1) } & RCC_AHB2ENR1_OTGEN != 0
}

#[derive(Clone, Copy)]
enum UsbState {
    None,
    Waiting,
    Addressed,
    Terminal,
}

impl UsbState {
    const fn word(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Waiting => "wait",
            Self::Addressed => "addr",
            Self::Terminal => "term",
        }
    }
}

fn state() -> UsbState {
    if !BUILT.load(Ordering::Relaxed) {
        return UsbState::None;
    }
    if TERMINAL_OPEN.load(Ordering::Relaxed) {
        return UsbState::Terminal;
    }
    if clocked() && unsafe { read32(OTG_DCFG) } & DCFG_DAD != 0 {
        return UsbState::Addressed;
    }
    UsbState::Waiting
}

pub fn write_regs(uart: Uart) {
    uart.write_str("[usb] state ");
    uart.write_str(state().word());
    uart.write_line("");
    for (name, addr) in [
        ("PWR_VOSR   ", PWR_VOSR),
        ("PWR_SVMSR  ", PWR_SVMSR),
        ("OTGHSPHYCR ", SYSCFG_OTGHSPHYCR),
        ("OTGHSPHYTN2", SYSCFG_OTGHSPHYTUNER2),
    ] {
        uart.write_str("  ");
        uart.write_str(name);
        uart.write_str(" = ");
        uart.write_hex32(unsafe { read32(addr) });
        uart.write_line("");
    }
    if !clocked() {
        uart.write_line("  OTG_HS unclocked; no core registers");
        return;
    }
    for (name, addr) in [
        ("GOTGCTL", OTG_GOTGCTL),
        ("GUSBCFG", OTG_GUSBCFG),
        ("GINTSTS", OTG_GINTSTS),
        ("GCCFG  ", OTG_GCCFG),
        ("DCFG   ", OTG_DCFG),
        ("DCTL   ", OTG_DCTL),
        ("DSTS   ", OTG_DSTS),
        ("PCGCCTL", OTG_PCGCCTL),
    ] {
        uart.write_str("  ");
        uart.write_str(name);
        uart.write_str(" = ");
        uart.write_hex32(unsafe { read32(addr) });
        uart.write_line("");
    }
}

bind_interrupts!(struct Irqs {
    OTG_HS => usb::InterruptHandler<peripherals::USB_OTG_HS>;
});

pub type Usb<'d> = Driver<'d, peripherals::USB_OTG_HS>;

const CONTROL_PACKET: u16 = 64;
const BULK_PACKET: u16 = 64;

/// How long a host's bus reset may sit unserviced before the link counts as
/// wedged.
const WEDGE_MS: u32 = 500;
const WEDGE_POLL_MS: u32 = 100;

static REENUMERATED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct LinkWatcher {
    configured: bool,
}

impl Handler for LinkWatcher {
    fn reset(&mut self) {
        if self.configured {
            REENUMERATED.store(true, Ordering::Relaxed);
        }
        self.configured = false;
        HOST_ACTIVE.store(false, Ordering::Relaxed);
    }

    fn configured(&mut self, configured: bool) {
        self.configured = configured;
        HOST_ACTIVE.store(configured, Ordering::Relaxed);
        if configured {
            HOST_CONFIGURED.store(true, Ordering::Relaxed);
        }
    }

    fn suspended(&mut self, suspended: bool) {
        HOST_ACTIVE.store(self.configured && !suspended, Ordering::Relaxed);
    }
}

static OUT: Pipe<CriticalSectionRawMutex, 8192> = Pipe::new();

/// Queues what fits of `bytes` and drops the rest, which it returns.
pub fn tee(bytes: &[u8]) -> &[u8] {
    #[cfg(feature = "ble")]
    crate::ble::tee(bytes);
    let mut rest = bytes;
    while let Ok(n @ 1..) = OUT.try_write(rest) {
        rest = &rest[n..];
    }
    rest
}

/// Wait, boundedly, for the pipe to be half empty, so long output written in
/// pieces reaches the host whole.
pub async fn make_room() {
    embassy_futures::yield_now().await;
    for _ in 0..40 {
        if !TERMINAL_OPEN.load(Ordering::Relaxed) || OUT.len() <= OUT.capacity() / 2 {
            return;
        }
        Timer::after_millis(5).await;
    }
}

/// Queues `line` for the host whole, waiting boundedly for room; false if the console is
/// closed or stays full.
pub async fn write_whole(line: &[u8]) -> bool {
    /// Room left for other tasks' lines.
    const HEADROOM: usize = 1024;
    for _ in 0..100 {
        if !TERMINAL_OPEN.load(Ordering::Relaxed) || line.len() + HEADROOM > OUT.capacity() {
            return false;
        }
        if OUT.capacity() - OUT.len() >= line.len() + HEADROOM {
            return tee(line).is_empty();
        }
        Timer::after_millis(5).await;
    }
    false
}

static EP_OUT: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static CONFIG_DESC: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static BOS_DESC: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static CONTROL_BUF: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0; 64]);

pub struct Pins {
    pub otg: Peri<'static, peripherals::USB_OTG_HS>,
    pub dp: Peri<'static, peripherals::PA12>,
    pub dm: Peri<'static, peripherals::PA11>,
    pub rcc: Peri<'static, peripherals::RCC>,
}

/// The `OTG_HS` supplies off now, and the crystal and PLL1 from the next Stop
/// exit on.
fn usb_block_off(rcc: &mut Peri<'static, peripherals::RCC>) {
    unsafe { crate::usb_power_off() };
    embassy_stm32::rcc::reconfigure(crate::rcc_config(false), false, rcc);
}

/// Blocks while the crystal starts, since a Stop entry would stop it again.
fn usb_block_on(rcc: &mut Peri<'static, peripherals::RCC>) -> Result<(), &'static str> {
    if !unsafe { crate::start_hse(crate::HSE_POLLS_48MHZ) } {
        return Err("the 16 MHz crystal did not start");
    }
    embassy_stm32::rcc::reconfigure(crate::rcc_config(true), true, rcc);
    unsafe { crate::usb_power_ready() }
}

/// The pid.codes VID and the PID requested from it; `tools/wrist_port.py` and
/// `tools/60-wrist-rs.rules` carry the same pair.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0xD1A1;

fn device_config() -> embassy_usb::Config<'static> {
    let mut config = embassy_usb::Config::new(USB_VID, USB_PID);
    config.manufacturer = Some("wrist-rs");
    config.product = Some("standalone-boot console");
    // The 96-bit unique device ID at 0x0BFA_0700 (RM0456, device electronic
    // signature), so two watches on one host differ.
    config.serial_number = Some(embassy_stm32::uid::uid_hex());
    config.max_packet_size_0 = u8::try_from(CONTROL_PACKET).unwrap_or(64);
    // A miscellaneous device with IADs, so CDC-ACM binds on every host.
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;
    config
}

/// The link for the life of the image: parked on battery, and rebuilt
/// whenever [`watch_for_wedge`] finds it stuck.
#[embassy_executor::task]
pub async fn service(mut pins: Pins, flash: &'static FlashMutex) -> ! {
    let ep_out = EP_OUT.take();
    let config_desc = CONFIG_DESC.take();
    let bos_desc = BOS_DESC.take();
    let control_buf = CONTROL_BUF.take();
    let uart = Uart::best_effort();
    let mut generation: u32 = 0;
    let mut changes = crate::power::changes();
    let mut up = true;
    loop {
        if crate::power::on_battery() && up {
            usb_block_off(&mut pins.rcc);
            up = false;
        }
        crate::power::wait_while_on_battery(&mut changes).await;
        if !up {
            if let Err(why) = usb_block_on(&mut pins.rcc) {
                uart.write_str("[usb] not restarted: ");
                uart.write_line(why);
                Timer::after_secs(10).await;
                continue;
            }
            up = true;
            uart.write_line("[usb] external power: crystal, PLL1 and USB supplies on again");
        }
        let mut driver_config = usb::Config::default();
        driver_config.vbus_detection = false;
        let driver = Driver::new_hs(
            pins.otg.reborrow(),
            Irqs,
            pins.dp.reborrow(),
            pins.dm.reborrow(),
            &mut ep_out[..],
            driver_config,
        );
        let mut state = State::new();
        let mut watcher = LinkWatcher::default();
        REENUMERATED.store(false, Ordering::Relaxed);
        let mut builder = Builder::new(
            driver,
            device_config(),
            &mut config_desc[..],
            &mut bos_desc[..],
            &mut [],
            &mut control_buf[..],
        );
        builder.handler(&mut watcher);
        let class = CdcAcmClass::new(&mut builder, &mut state, BULK_PACKET);
        let (tx, rx) = class.split();
        let mut device = builder.build();
        BUILT.store(true, Ordering::Relaxed);

        let (why, detail) =
            match select4(device.run(), pump(tx), listen(rx, flash), watch_for_wedge()).await {
                Either4::First(never) | Either4::Second(never) | Either4::Third(never) => never,
                Either4::Fourth(found) => found,
            };

        // Soft-disconnect first, so the host sees the device leave.
        TERMINAL_OPEN.store(false, Ordering::Relaxed);
        QUIET.store(false, Ordering::Relaxed);
        unsafe { modify32(OTG_DCTL, 0, DCTL_SDIS) };
        Timer::after_millis(50).await;
        drop(device);
        BUILT.store(false, Ordering::Relaxed);
        HOST_ACTIVE.store(false, Ordering::Relaxed);
        generation += 1;
        usblog::note(why, detail);
        uart.write_str("[usb] link rebuilt, generation ");
        uart.write_u32(generation);
        uart.write_str(": ");
        uart.write_str(why);
        uart.write_str(" ");
        uart.write_u32(detail);
        uart.write_line("");
        Timer::after_millis(200).await;
    }
}

/// Resolves with the reason the link needs rebuilding, and a number for the
/// log.
async fn watch_for_wedge() -> (&'static str, u32) {
    let mut pending_ms: u32 = 0;
    loop {
        Timer::after_millis(u64::from(WEDGE_POLL_MS)).await;
        if crate::power::external_input() == Some(false) { return ("battery: USB parked", 0); }
        if REENUMERATED.load(Ordering::Relaxed) {
            return (
                "host reset a configured link; endpoints may hold abandoned transfers",
                0,
            );
        }
        let reset_pending = clocked() && unsafe { read32(OTG_GINTSTS) } & GINTSTS_USBRST != 0;
        pending_ms = if reset_pending {
            pending_ms + WEDGE_POLL_MS
        } else {
            0
        };
        if pending_ms >= WEDGE_MS {
            return ("bus reset unserviced for ms", pending_ms);
        }
    }
}

async fn pump<'d>(mut tx: Sender<'d, Usb<'d>>) -> ! {
    let uart = Uart::best_effort();
    let mut buf = [0u8; BULK_PACKET as usize];
    loop {
        tx.wait_connection().await;
        OUT.clear();
        TERMINAL_OPEN.store(true, Ordering::Relaxed);
        uart.write_line("[usb] console attached; 'help' lists commands");
        loop {
            let n = OUT.read(&mut buf).await;
            while QUIET.load(Ordering::Relaxed) {
                Timer::after_millis(20).await;
            }
            if tx.write_packet(&buf[..n]).await.is_err() {
                break;
            }
            // A zero-length packet ends a transfer that filled its last packet.
            if n == buf.len() && tx.write_packet(&[]).await.is_err() {
                break;
            }
        }
        TERMINAL_OPEN.store(false, Ordering::Relaxed);
    }
}

async fn listen<'d>(mut rx: Receiver<'d, Usb<'d>>, flash: &'static FlashMutex) -> ! {
    let mut packet = [0u8; BULK_PACKET as usize];
    let mut line = [0u8; 192];
    loop {
        rx.wait_connection().await;
        let mut len = 0usize;
        'link: loop {
            let n = match rx.read_packet(&mut packet).await {
                Ok(n) => n,
                Err(EndpointError::BufferOverflow) => {
                    Uart::best_effort().write_line("[usb] rx overflow: a packet was lost");
                    continue;
                }
                Err(EndpointError::Disabled) => break,
            };
            for &b in &packet[..n] {
                match b {
                    b'\r' | b'\n' => {
                        if len > 0 {
                            if let Some((target, size, crc)) = parse_transfer(&line[..len]) {
                                let uart = Uart::best_effort();
                                let outcome = match target {
                                    Target::Application => {
                                        update::receive(flash, size, crc, &mut rx).await
                                    }
                                    Target::Loader => {
                                        update::receive_loader(flash, size, crc, &mut rx).await
                                    }
                                };
                                if let Err(why) = outcome {
                                    uart.write_str(match target {
                                        Target::Application => "[update] error: ",
                                        Target::Loader => "[loader] error: ",
                                    });
                                    uart.write_line(why);
                                }
                                len = 0;
                                continue 'link;
                            }
                            command(&line[..len], flash);
                        }
                        len = 0;
                    }
                    0x08 | 0x7F => len = len.saturating_sub(1),
                    _ if len < line.len() => {
                        line[len] = b;
                        len += 1;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Application,
    Loader,
}

pub fn parse_transfer(cmd: &[u8]) -> Option<(Target, usize, u32)> {
    let (target, rest) = match cmd.strip_prefix(b"update ") {
        Some(rest) => (Target::Application, rest),
        None => (Target::Loader, cmd.strip_prefix(b"loader ")?),
    };
    let text = core::str::from_utf8(rest).ok()?;
    let mut parts = text.split_whitespace();
    let len: usize = parts.next()?.parse().ok()?;
    let crc = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((target, len, crc))
}

#[allow(clippy::too_many_lines)]
pub fn command(cmd: &[u8], flash: &'static FlashMutex) {
    let uart = Uart::best_effort();
    Uart::retry();
    match cmd.strip_suffix(b" ").unwrap_or(cmd) {
        b"help" | b"?" => {
            uart.write_line(
                "[usb] commands: help ping uptime version boot reset report usblog time [set YYYY-MM-DD HH:MM:SS|zone <min> [<utc s> <min>]...] power [sleep on|off|status|clear-crash] page [lag [s]|vcom [auto|low|high]|light on|off|dump|stress [...]] press <up|down|back|select> [long] modules [force view|link1] haptic [tap|alarm|stop] flash survey|ecc-test staging <len>|<off> <n> update <len> <crc32> loader <len> <crc32>",
            );
            uart.write_line("[usb]   boot: the loader's state word; update: stream a new application (tools/wrist-update.py)");
            uart.write_line("[usb]   time set: the calendar in local time (tools/wrist-time.py sends the host's)");
            uart.write_line("[usb]   time zone: minutes east of UTC, then up to two changes as UTC seconds since 1970 and the new offset");
            uart.write_line("[usb]   report: the full report (clocks, RTC, USB registers and log), written in pieces");
            uart.write_line("[usb]   reset: software reset; hold Select through it for the bootloader's USB mode");
            uart.write_line("[usb]   power: the input, the last stretch on battery, sleep counters, the retained crash record");
            uart.write_line("[usb]   page: the face as last drawn; frame, executor-latency and per-task CPU figures; vcom and the light");
            uart.write_line("[usb]   page dump: the frame last drawn, as run-length rows (tools/wrist-frame.py)");
            uart.write_line("[usb]   page stress: glyphs|churn|edge|full|pages in place of the page, held at a step or run every MS; off");
            uart.write_line("[usb]   press: a debounced press, as if from the button; never a hold");
            uart.write_line("[usb]   modules: what the display, link1 and GNSS slots' EEPROMs say");
            uart.write_line("[usb]   flash survey: which pages hold anything or raise ECC errors, a CRC per region, and the flash controller's status; never writes flash");
            uart.write_line("[usb]   flash ecc-test: reads ST's 1-bit and 2-bit ECC test words; the NMI must catch the second");
            #[cfg(feature = "ble")]
            uart.write_line("[usb]   ble [on|off|version|yes|no|bonds|forget|disconnect|profile auto|cable|battery|interval now <min> <max>]: the radio; off until `ble on`, which persists; `ble yes` confirms a pairing");
            uart.write_line("[usb]   haptic [tap|alarm|stop]: the vibration motor; bare, what the driver reads back (`haptic` feature)");
            #[cfg(feature = "ble")]
            uart.write_line("[usb]   ble scan [s] [interval window]: lists advertisers for s seconds (10 by default), listening for window ms in every interval ms");
        }
        #[cfg(feature = "ble")]
        b"ble" => crate::ble::command(b"", uart),
        #[cfg(feature = "ble")]
        rest if rest.starts_with(b"ble ") => crate::ble::command(&rest[4..], uart),
        rest if rest == b"haptic" || rest.starts_with(b"haptic ") => crate::haptic::command(&rest[6..], uart),
        b"usblog" => crate::REPORT.signal(crate::Report::UsbLog),
        b"report" => crate::REPORT.signal(crate::Report::Full),
        rest if rest == b"modules" || rest.starts_with(b"modules ") => crate::modules::command(&rest[7..], uart),
        b"rtc" | b"time" => crate::rtc::write_report(uart),
        rest if rest == b"power" || rest.starts_with(b"power ") => crate::power::command(&rest[5..], uart),
        set if set.starts_with(b"time set ") => {
            let text = core::str::from_utf8(&set[9..]).unwrap_or("");
            match crate::rtc::parse_civil(text) {
                None => uart.write_line("[rtc] usage: time set YYYY-MM-DD HH:MM:SS (24-hour)"),
                Some(civil) => {
                    match crate::rtc::start().and_then(|src| crate::rtc::set(civil).map(|()| src)) {
                        Ok(src) => {
                            crate::rtc::note_set(crate::rtc::Source::Host, civil);
                            // A bare `time set` says nothing about the zone; `time zone` follows it.
                            crate::rtc::clear_zone();
                            uart.write_str("[rtc] set; running from ");
                            uart.write_line(src);
                        }
                        Err(why) => {
                            uart.write_str("[rtc] not set: ");
                            uart.write_line(why);
                        }
                    }
                }
            }
        }
        zone if zone.starts_with(b"time zone ") => {
            match core::str::from_utf8(&zone[10..]).ok().and_then(crate::rtc::Zone::parse) {
                None => uart.write_line("[rtc] usage: time zone <minutes east> [<UTC seconds since 1970> <minutes east>], at most two changes, in order"),
                Some(z) => {
                    crate::rtc::set_zone(&z);
                    uart.write_str("[rtc] zone ");
                    crate::rtc::write_zone(uart, z.offset);
                    uart.write_str(", ");
                    uart.write_u32(u32::try_from(z.next.iter().flatten().count()).unwrap_or(0));
                    uart.write_line(" change(s) kept");
                    crate::wake_display();
                }
            }
        }
        rest if rest == b"flash" || rest.starts_with(b"flash ") => crate::flash::command(&rest[5..], uart),
        rest if rest.starts_with(b"staging ") => crate::update::staging_command(&rest[8..], uart),
        b"boot" => {
            uart.write_str("[boot] state ");
            uart.write_line(update::state_name(flash));
        }
        b"ping" => uart.write_line("[usb] pong"),
        rest if rest == b"press" || rest.starts_with(b"press ") => press(&rest[5..], uart),
        rest if rest == b"page" || rest.starts_with(b"page ") => crate::glass::command(&rest[4..], uart),
        b"uptime" => {
            uart.write_str("[usb] uptime ");
            uart.write_u32(u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX));
            uart.write_line(" s");
        }
        b"version" => {
            uart.write_str("[usb] standalone-boot ");
            uart.write_str(crate::version::VERSION);
            uart.write_str(" build ");
            uart.write_line(crate::version::BUILD_ID);
        }
        b"reset" => {
            uart.write_line("[usb] resetting now");
            uart.flush();
            watchdog::terminal_reset();
        }
        other => {
            uart.write_str("[usb] unknown command: ");
            uart.write_line(core::str::from_utf8(other).unwrap_or("?"));
        }
    }
}

/// `press <up|down|back|select> [long]`: a press for the button task, counted apart from real
/// ones. A long press is past a short one; nothing here can stand in for a hold.
fn press(args: &[u8], uart: Uart) {
    let mut words = core::str::from_utf8(args).unwrap_or("").split_whitespace();
    let button = words.next().and_then(wrist_pages::Button::parse);
    let long = match words.next() {
        None => Some(false),
        Some("long") => Some(true),
        Some(_) => None,
    };
    match (button, long, words.next()) {
        (Some(button), Some(long), None) => {
            if !crate::inject_press(wrist_pages::Press { button, long }) {
                uart.write_line("[button] not injected: the queue is full");
            }
        }
        _ => uart.write_line("[button] usage: press <up|down|back|select> [long]"),
    }
}
