//! How long each task holds the executor, from its trace hooks and the cycle counter.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m::peripheral::{DCB, DWT};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

use crate::uart::Uart;

const CORE_MHZ: u32 = 48;
const TASKS: usize = 12;

#[derive(Clone, Copy)]
struct Task {
    id: u32,
    name: &'static str,
    polls: u32,
    cycles: u32,
    longest: u32,
}

const NONE: Task = Task { id: 0, name: "?", polls: 0, cycles: 0, longest: 0 };

struct Book {
    tasks: [Task; TASKS],
    begun: u32,
    awake_from: u32,
    awake: u32,
    wakes: u32,
    longest: u32,
    longest_task: &'static str,
    longest_at_ms: u32,
}

static BOOK: Mutex<CriticalSectionRawMutex, RefCell<Book>> = Mutex::new(RefCell::new(Book {
    tasks: [NONE; TASKS],
    begun: 0,
    awake_from: 0,
    awake: 0,
    wakes: 0,
    longest: 0,
    longest_task: "",
    longest_at_ms: 0,
}));

static SINCE_MS: AtomicU32 = AtomicU32::new(0);

/// Starts the cycle counter.
pub fn init() {
    unsafe {
        (*DCB::PTR).demcr.modify(|w| w | (1 << 24));
        (*DWT::PTR).ctrl.modify(|w| w | 1);
    }
}

pub fn cycles() -> u32 {
    DWT::cycle_count()
}

/// Gives the task `id` belongs to its name in the report.
pub fn name(id: u32, name: &'static str) {
    BOOK.lock(|b| {
        let mut b = b.borrow_mut();
        if let Some(t) = b.tasks.iter_mut().find(|t| t.id == id || t.id == 0) {
            t.id = id;
            t.name = name;
        }
    });
}

#[no_mangle]
extern "Rust" fn _embassy_trace_poll_start(_executor: u32) {
    let now = cycles();
    BOOK.lock(|b| {
        let mut b = b.borrow_mut();
        b.awake_from = now;
        b.wakes = b.wakes.wrapping_add(1);
    });
}

#[no_mangle]
extern "Rust" fn _embassy_trace_executor_idle(_executor: u32) {
    let now = cycles();
    BOOK.lock(|b| {
        let mut b = b.borrow_mut();
        b.awake = b.awake.wrapping_add(now.wrapping_sub(b.awake_from));
    });
}

#[no_mangle]
extern "Rust" fn _embassy_trace_task_exec_begin(_executor: u32, _task: u32) {
    let now = cycles();
    BOOK.lock(|b| b.borrow_mut().begun = now);
}

#[no_mangle]
extern "Rust" fn _embassy_trace_task_exec_end(_executor: u32, task: u32) {
    let now = cycles();
    BOOK.lock(|b| {
        let mut b = b.borrow_mut();
        let took = now.wrapping_sub(b.begun);
        let mut name = "?";
        if let Some(t) = b.tasks.iter_mut().find(|t| t.id == task || t.id == 0) {
            t.id = task;
            t.polls = t.polls.wrapping_add(1);
            t.cycles = t.cycles.wrapping_add(took);
            t.longest = t.longest.max(took);
            name = t.name;
        }
        if took > b.longest {
            b.longest = took;
            b.longest_task = name;
            b.longest_at_ms = crate::glass::now_ms();
        }
    });
}

#[no_mangle]
extern "Rust" fn _embassy_trace_task_new(_executor: u32, _task: u32) {}

#[no_mangle]
extern "Rust" fn _embassy_trace_task_end(_executor: u32, _task: u32) {}

#[no_mangle]
extern "Rust" fn _embassy_trace_task_ready_begin(_executor: u32, _task: u32) {}

fn us(cycles: u32) -> u32 {
    cycles / CORE_MHZ
}

/// The figures since the last call, then zeroed.
pub fn write(uart: Uart) {
    let book = BOOK.lock(|b| {
        let mut b = b.borrow_mut();
        let copy = (b.tasks, b.awake, b.wakes, b.longest, b.longest_task, b.longest_at_ms);
        for t in &mut b.tasks {
            (t.polls, t.cycles, t.longest) = (0, 0, 0);
        }
        (b.awake, b.wakes, b.longest) = (0, 0, 0);
        copy
    });
    let (tasks, awake, wakes, longest, longest_task, at) = book;
    let now = crate::glass::now_ms();
    let span = now.wrapping_sub(SINCE_MS.swap(now, Ordering::Relaxed));
    uart.write_str("[busy] over ");
    uart.write_u32(span);
    uart.write_str(" ms: awake ");
    uart.write_u32(us(awake));
    uart.write_str(" us in ");
    uart.write_u32(wakes);
    uart.write_str(" wakes; longest poll ");
    uart.write_u32(us(longest));
    uart.write_str(" us by ");
    uart.write_str(longest_task);
    uart.write_str(" at ");
    uart.write_u32(at);
    uart.write_line(" ms");
    uart.write_str("[busy] task polls/us/longest us:");
    for t in tasks.iter().filter(|t| t.id != 0) {
        uart.write_str(" ");
        uart.write_str(t.name);
        uart.write_str(" ");
        uart.write_u32(t.polls);
        uart.write_str("/");
        uart.write_u32(us(t.cycles));
        uart.write_str("/");
        uart.write_u32(us(t.longest));
    }
    uart.write_line("");
}
