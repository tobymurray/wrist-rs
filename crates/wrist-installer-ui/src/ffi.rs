//! The C interface the GUI's shell calls, declared in `apps/image-installer/GUI/installer_ui.h`.

use core::cell::UnsafeCell;

use inscribed_disc::{Abgr2222, Surface};

use crate::draw::render;
use crate::flow::Input;
use crate::installer::{Installer, Reply, State, Verdict};

/// `WRIST_INSTALLER_UI_ABI`: bump with any change to these signatures or constants.
pub const ABI: u32 = 1;

pub const INPUT_R1_PRESS: u8 = 0;
pub const INPUT_R1_RELEASE: u8 = 1;
pub const INPUT_R2_CLICK: u8 = 2;
pub const INPUT_TICK: u8 = 3;
pub const INPUT_SUSPEND: u8 = 4;

pub const REPLY_REDRAW: u32 = 1;
pub const REPLY_CONFIRM: u32 = 2;
pub const REPLY_EXIT: u32 = 4;

/// The GUI process runs one thread, and only the shell's message loop calls in.
struct One(UnsafeCell<Installer>);

// SAFETY: one thread; see above.
unsafe impl Sync for One {}

static INSTALLER: One = One(UnsafeCell::new(Installer::new()));

fn installer() -> &'static mut Installer {
    // SAFETY: one thread, and no call holds the reference past its return.
    unsafe { &mut *INSTALLER.0.get() }
}

fn flags(reply: Reply) -> u32 {
    let mut out = 0;
    if reply.redraw {
        out |= REPLY_REDRAW;
    }
    if reply.confirm.is_some() {
        out |= REPLY_CONFIRM;
    }
    if reply.exit {
        out |= REPLY_EXIT;
    }
    out
}

#[no_mangle]
pub extern "C" fn wrist_installer_ui_abi() -> u32 {
    ABI
}

/// The service's `InstallerMessage::Report`.
#[no_mangle]
pub extern "C" fn wrist_installer_ui_report(state: u8, code: i32, crc: u32, now_ms: u32) -> u32 {
    flags(installer().report(Verdict { state: State::from_wire(state), code, crc }, now_ms))
}

/// A button, a GUI tick or a suspend; with `REPLY_CONFIRM`, `*confirm_crc` is the CRC the
/// confirm must name.
///
/// # Safety
/// `confirm_crc` is null or points to a writable `u32`.
#[no_mangle]
pub unsafe extern "C" fn wrist_installer_ui_input(input: u8, now_ms: u32, confirm_crc: *mut u32) -> u32 {
    let input = match input {
        INPUT_R1_PRESS => Input::R1Press,
        INPUT_R1_RELEASE => Input::R1Release,
        INPUT_R2_CLICK => Input::R2Click,
        INPUT_TICK => Input::Tick,
        INPUT_SUSPEND => Input::Suspend,
        _ => return 0,
    };
    let reply = installer().input(input, now_ms);
    if let Some(crc) = reply.confirm {
        if !confirm_crc.is_null() {
            *confirm_crc = crc;
        }
    }
    flags(reply)
}

/// Draws the screen as it is at `now_ms` into `buf`, `width` by `height` bytes of ABGR2222.
///
/// # Safety
/// `buf` points to `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn wrist_installer_ui_render(buf: *mut u8, len: u32, width: u16, height: u16, now_ms: u32) {
    if buf.is_null() {
        return;
    }
    let frame = core::slice::from_raw_parts_mut(buf, len as usize);
    if let Some(mut surface) = Surface::<Abgr2222>::round(frame, u32::from(width), u32::from(height)) {
        render(&mut surface, &installer().screen(now_ms));
    }
}

#[cfg(all(target_arch = "arm", not(test)))]
extern "C" {
    /// The shell logs where, and ends the process.
    fn wrist_installer_ui_panicked(file: *const u8, len: u32, line: u32) -> !;
}

#[cfg(all(target_arch = "arm", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let (file, line) = info.location().map_or(("?", 0), |l| (l.file(), l.line()));
    // SAFETY: the shell defines it; `file` is valid for its length.
    unsafe { wrist_installer_ui_panicked(file.as_ptr(), u32::try_from(file.len()).unwrap_or(0), line) }
}
