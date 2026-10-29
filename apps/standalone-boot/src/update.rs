//! An update's flash phase is the one place this image holds the executor for more than a few
//! milliseconds, with the console parked for it.

use core::cell::RefCell;

use core::sync::atomic::{AtomicBool, Ordering};
use embassy_boot_stm32::{
    AlignedBuffer, BlockingFirmwareState, BlockingFirmwareUpdater, FirmwareUpdaterConfig, State,
};
use embassy_stm32::flash::{Bank1Region, Blocking, Flash, WRITE_SIZE};
use embassy_stm32::peripherals::FLASH;
use embassy_stm32::Peri;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Timer;
use static_cell::StaticCell;
use wrist_flash::vectors::Target;

use embassy_usb::class::cdc_acm::Receiver;

use crate::console::Usb;
use crate::mmio::{
    modify32, read32, write32, ICACHE_CR, ICACHE_CR_CACHEINV, ICACHE_SR, ICACHE_SR_BUSYF, PWR_CR2,
    PWR_CR2_SRAM3_1_TO_8,
};
use crate::uart::Uart;
use crate::watchdog;

pub type FlashMutex = Mutex<NoopRawMutex, RefCell<Bank1Region<'static, Blocking>>>;

static FLASH: StaticCell<FlashMutex> = StaticCell::new();

pub fn init(flash: Peri<'static, FLASH>) -> &'static FlashMutex {
    let layout = Flash::new_blocking(flash).into_blocking_regions();
    FLASH.init(Mutex::new(RefCell::new(layout.bank1_region)))
}

pub fn state_name(flash: &'static FlashMutex) -> &'static str {
    invalidate_icache();
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    let mut state = BlockingFirmwareState::from_config(config, &mut aligned.0);
    match state.get_state() {
        Ok(State::Boot) => "boot",
        Ok(State::Swap) => "swap (just updated; not yet marked booted)",
        Ok(State::Revert) => "revert (the previous update failed to mark itself booted)",
        Ok(State::DfuDetach) => "dfu-detach",
        Err(_) => "unreadable",
    }
}

/// Swapped in by an update and not yet marked booted.
pub fn just_updated(flash: &'static FlashMutex) -> bool {
    invalidate_icache();
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    let mut state = BlockingFirmwareState::from_config(config, &mut aligned.0);
    matches!(state.get_state(), Ok(State::Swap))
}

/// Returns the state it replaces.
pub fn mark_booted(flash: &'static FlashMutex) -> Result<&'static str, ()> {
    let was = state_name(flash);
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    let mut state = BlockingFirmwareState::from_config(config, &mut aligned.0);
    state.mark_booted().map(|()| was).map_err(|_| ())
}

/// Before any read of flash this image may have written, which the instruction cache would
/// otherwise serve stale (RM0456 8.7.1); false if the invalidation never finished.
pub fn invalidate_icache() -> bool {
    unsafe {
        write32(ICACHE_CR, read32(ICACHE_CR) | ICACHE_CR_CACHEINV);
        for _ in 0..100_000 {
            if read32(ICACHE_SR) & ICACHE_SR_BUSYF == 0 {
                return true;
            }
        }
    }
    false
}

/// The whole image arrives here before flash is touched, so receiving and flashing never overlap:
/// SRAM3's first eight pages, outside the linker's RAM and kept through Stop only for a transfer.
const STAGING: usize = 0x200D_0000;
const STAGING_SIZE: usize = 512 * 1024;
static STAGING_BUSY: AtomicBool = AtomicBool::new(false);

fn staging() -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(STAGING as *const u8, STAGING_SIZE) }
}

/// The loader's region in bank 1 (`apps/wrist-loader/LAYOUT.md`).
const LOADER_OFFSET: u32 = 0x6_0000;
const LOADER_SIZE: usize = 32 * 1024;

pub struct Session {
    tag: &'static str,
    target: Target,
    len: usize,
    crc: u32,
    received: usize,
    next_report: usize,
}

impl Session {
    #[must_use]
    pub fn complete(&self) -> bool {
        self.received >= self.len
    }

    /// The first image byte is in.
    #[cfg(feature = "ble")]
    #[must_use]
    pub fn started(&self) -> bool {
        self.received > 0
    }
}

/// Also when the link is rebuilt under a transfer and the session goes with it.
impl Drop for Session {
    fn drop(&mut self) {
        release();
    }
}

pub fn begin(
    tag: &'static str,
    target: Target,
    len: usize,
    crc: u32,
) -> Result<Session, &'static str> {
    const _: () = assert!(Target::Application.size() as usize <= STAGING_SIZE);
    const _: () = assert!(Target::Loader.size() as usize == LOADER_SIZE);
    const _: () = assert!(Target::Loader.base() == wrist_flash::plan::BANK1_BASE + LOADER_OFFSET);
    if len == 0 || len > target.size() as usize {
        return Err("length out of range for this target");
    }
    if STAGING_BUSY.swap(true, Ordering::Acquire) {
        return Err("a transfer is already in progress");
    }
    unsafe { modify32(PWR_CR2, PWR_CR2_SRAM3_1_TO_8, 0) };
    let uart = Uart::best_effort();
    uart.write_str(tag);
    uart.write_str(" send ");
    uart.write_u32(u32::try_from(len).unwrap_or(u32::MAX));
    uart.write_line(" bytes");
    Ok(Session {
        tag,
        target,
        len,
        crc,
        received: 0,
        next_report: 16 * 1024,
    })
}

/// True once every byte is in.
pub fn feed(s: &mut Session, data: &[u8]) -> bool {
    let staging = unsafe { core::slice::from_raw_parts_mut(STAGING as *mut u8, STAGING_SIZE) };
    let take = data.len().min(s.len - s.received);
    staging[s.received..s.received + take].copy_from_slice(&data[..take]);
    s.received += take;
    if s.received >= s.next_report {
        let uart = Uart::best_effort();
        uart.write_str(s.tag);
        uart.write_str(" received ");
        uart.write_u32(u32::try_from(s.received).unwrap_or(u32::MAX));
        uart.write_line("");
        s.next_report += 16 * 1024;
    }
    s.complete()
}

pub fn staging_command(args: &[u8], uart: Uart) {
    let staging = staging();
    let text = core::str::from_utf8(args).unwrap_or("");
    let mut words = text.split_whitespace().map(|w| w.parse::<usize>().ok());
    match (words.next().flatten(), words.next().flatten()) {
        (Some(len), None) => crate::REPORT.signal(crate::Report::Staging(len.min(STAGING_SIZE))),
        (Some(off), Some(n)) => {
            let off = off.min(STAGING_SIZE);
            let end = (off + n.min(64)).min(STAGING_SIZE);
            uart.write_str("[staging] ");
            for &b in &staging[off..end] {
                uart.write_hex8(b);
            }
            uart.write_line("");
        }
        _ => uart.write_line("[staging] usage: staging <len> | staging <offset> <count>"),
    }
}

/// A 4 KiB block's CRC a line, yielding every kibibyte, since the CRC is bitwise.
pub async fn staging_crcs(uart: Uart, len: usize) {
    for (i, block) in staging()[..len.min(STAGING_SIZE)].chunks(4096).enumerate() {
        let mut crc = 0xFFFF_FFFF;
        for piece in block.chunks(1024) {
            crc = wrist_flash::plan::crc32_continue(crc, piece);
            embassy_futures::yield_now().await;
        }
        uart.write_str("[staging] block ");
        uart.write_u32(u32::try_from(i).unwrap_or(0));
        uart.write_str(" crc ");
        uart.write_hex32(!crc);
        uart.write_line("");
        crate::console::make_room().await;
    }
}

/// Takes the session, so nothing can feed it afterwards.
#[allow(clippy::needless_pass_by_value)]
pub fn abort(s: Session) {
    let uart = Uart::best_effort();
    uart.write_str(s.tag);
    uart.write_line(" abandoned");
}

fn verify(s: &Session) -> Result<&'static [u8], &'static str> {
    let uart = Uart::best_effort();
    let staging = staging();
    if !s.complete() {
        return Err("transfer incomplete");
    }
    let found = wrist_flash::plan::crc32(&staging[..s.len]);
    if found != s.crc {
        uart.write_str(s.tag);
        uart.write_str(" crc mismatch in RAM: got ");
        uart.write_hex32(found);
        uart.write_str(" declared ");
        uart.write_hex32(s.crc);
        uart.write_line("");
        return Err("crc mismatch; nothing written");
    }
    if !s.target.accepts(&staging[..s.len]) {
        return Err(match s.target {
            Target::Loader => "not a loader image: vector table rejected; nothing written",
            Target::Application => {
                "not an application image: vector table rejected; nothing written"
            }
        });
    }
    uart.write_str(s.tag);
    uart.write_str(" received and verified in RAM, crc ");
    uart.write_hex32(found);
    uart.write_line("; console pauses while flash is written");
    Ok(&staging[..s.len])
}

fn release() {
    if crate::park::on_smps() {
        unsafe { modify32(PWR_CR2, 0, PWR_CR2_SRAM3_1_TO_8) };
    }
    STAGING_BUSY.store(false, Ordering::Release);
}

pub fn app_update_allowed(flash: &'static FlashMutex) -> Result<(), &'static str> {
    invalidate_icache();
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    let mut updater = BlockingFirmwareUpdater::new(config, &mut aligned.0);
    match updater.get_state() {
        Ok(State::Boot) => Ok(()),
        Ok(_) => Err("this image is not marked booted yet; wait for [boot] marked, then retry"),
        Err(_) => Err("state partition unreadable"),
    }
}

pub async fn finish_app(flash: &'static FlashMutex, s: Session) -> Result<(), &'static str> {
    let uart = Uart::best_effort();
    let image = verify(&s)?;
    crate::console::quiet(true);
    Timer::after_millis(200).await;
    let result = write_and_mark(flash, image, s.crc);
    crate::console::quiet(false);
    result?;
    uart.write_str("[update] ok ");
    uart.write_u32(u32::try_from(s.len).unwrap_or(u32::MAX));
    uart.write_str(" bytes, crc ");
    uart.write_hex32(s.crc);
    uart.write_line(", swap marked; reset to apply");
    Ok(())
}

pub async fn finish_loader(flash: &'static FlashMutex, s: Session) -> Result<(), &'static str> {
    let uart = Uart::best_effort();
    let image = verify(&s)?;
    crate::console::quiet(true);
    Timer::after_millis(200).await;
    let result = write_loader(flash, image, s.crc);
    crate::console::quiet(false);
    result?;
    uart.write_str("[loader] ok ");
    uart.write_u32(u32::try_from(s.len).unwrap_or(u32::MAX));
    uart.write_str(" bytes, crc ");
    uart.write_hex32(s.crc);
    uart.write_line(", loader pages rewritten and read back; takes effect at the next reset");
    Ok(())
}

async fn stage_usb<'d>(s: &mut Session, rx: &mut Receiver<'d, Usb<'d>>) -> Result<(), &'static str> {
    let mut packet = [0u8; 64];
    while !s.complete() {
        let read = rx.read_packet(&mut packet);
        let n = match embassy_time::with_timeout(embassy_time::Duration::from_secs(5), read).await {
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return Err("link dropped"),
            Err(_) => return Err("no data for 5 s; transfer abandoned"),
        };
        feed(s, &packet[..n]);
    }
    Ok(())
}

pub async fn receive<'d>(
    flash: &'static FlashMutex,
    len: usize,
    crc: u32,
    rx: &mut Receiver<'d, Usb<'d>>,
) -> Result<(), &'static str> {
    app_update_allowed(flash)?;
    let mut s = begin("[update]", Target::Application, len, crc)?;
    if let Err(why) = stage_usb(&mut s, rx).await {
        abort(s);
        return Err(why);
    }
    finish_app(flash, s).await
}

pub async fn receive_loader<'d>(
    flash: &'static FlashMutex,
    len: usize,
    crc: u32,
    rx: &mut Receiver<'d, Usb<'d>>,
) -> Result<(), &'static str> {
    let mut s = begin("[loader]", Target::Loader, len, crc)?;
    if let Err(why) = stage_usb(&mut s, rx).await {
        abort(s);
        return Err(why);
    }
    finish_loader(flash, s).await
}

fn write_loader(flash: &'static FlashMutex, image: &[u8], crc: u32) -> Result<(), &'static str> {
    const CHUNK: usize = 2048;
    let mut chunk = [0u8; CHUNK];
    let end = LOADER_OFFSET + u32::try_from(LOADER_SIZE).unwrap_or(0);
    flash.lock(|cell| {
        let mut region = cell.borrow_mut();
        watchdog::refresh();
        region
            .blocking_erase(LOADER_OFFSET, end)
            .map_err(|_| "loader erase failed")?;
        let mut offset = LOADER_OFFSET;
        for piece in image.chunks(CHUNK) {
            watchdog::refresh();
            let padded = piece.len().div_ceil(WRITE_SIZE) * WRITE_SIZE;
            chunk[..piece.len()].copy_from_slice(piece);
            chunk[piece.len()..padded].fill(0xFF);
            region
                .blocking_write(offset, &chunk[..padded])
                .map_err(|_| "loader write failed")?;
            offset += u32::try_from(piece.len()).unwrap_or(0);
        }
        invalidate_icache();
        let mut hasher: u32 = 0xFFFF_FFFF;
        let mut done = 0usize;
        while done < image.len() {
            watchdog::refresh();
            let n = (image.len() - done).min(CHUNK);
            region
                .blocking_read(
                    LOADER_OFFSET + u32::try_from(done).unwrap_or(0),
                    &mut chunk[..n],
                )
                .map_err(|_| "loader read-back failed")?;
            hasher = wrist_flash::plan::crc32_continue(hasher, &chunk[..n]);
            done += n;
        }
        if !hasher == crc {
            Ok(())
        } else {
            Err("loader read-back does not match what was sent")
        }
    })
}

fn write_and_mark(flash: &'static FlashMutex, image: &[u8], crc: u32) -> Result<(), &'static str> {
    const CHUNK: usize = 2048;
    invalidate_icache();
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    let mut updater = BlockingFirmwareUpdater::new(config, &mut aligned.0);
    watchdog::refresh();
    updater.prepare_update().map_err(|_| "DFU erase failed")?;
    let mut offset = 0usize;
    let mut chunk = [0u8; CHUNK];
    for piece in image.chunks(CHUNK) {
        watchdog::refresh();
        let padded = piece.len().div_ceil(WRITE_SIZE) * WRITE_SIZE;
        chunk[..piece.len()].copy_from_slice(piece);
        chunk[piece.len()..padded].fill(0xFF);
        updater
            .write_firmware(offset, &chunk[..padded])
            .map_err(|_| "DFU write failed")?;
        offset += piece.len();
    }
    invalidate_icache();
    let mut hasher: u32 = 0xFFFF_FFFF;
    let mut done = 0usize;
    while done < image.len() {
        watchdog::refresh();
        let n = (image.len() - done).min(CHUNK);
        updater
            .read_dfu(u32::try_from(done).unwrap_or(u32::MAX), &mut chunk[..n])
            .map_err(|_| "DFU read-back failed")?;
        hasher = wrist_flash::plan::crc32_continue(hasher, &chunk[..n]);
        done += n;
    }
    if !hasher != crc {
        return Err("flash read-back does not match; nothing marked");
    }
    updater
        .mark_updated()
        .map_err(|_| "could not mark the swap")
}
