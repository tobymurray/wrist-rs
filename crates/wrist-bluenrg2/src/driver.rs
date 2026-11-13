//! The BlueNRG-2 behind its reset pin and the SPI handshake: a command and its reply, and every
//! packet the chip sends meanwhile handed out in order.

use bluenrg2::{BootReason, Event, VendorEvent, RETURN_PARAMS_MAX};
use embassy_futures::yield_now;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;
use wrist_ble::{Failure, Op, OpId, RadioEvent};

use crate::events::Translator;
use crate::gatt::Handles;
use crate::op_deadline;
use crate::ops::{self, Ops};
use crate::snapshot::Peers;
use crate::transport::{packets, Counts, Read, Transport, MAX_READ};

/// A command is answered within this, or not at all.
pub const REPLY_WITHIN_US: u64 = 1_000_000;
/// The chip announces itself within this of leaving reset (HARDWARE.md, the Bluetooth module).
pub const BOOT_WITHIN_US: u32 = 3_000_000;
/// The reset pin is held low this long.
pub const RESET_MS: u32 = 5;
/// The most reads one drain makes while IRQ stays high.
pub const DRAIN_READS: u32 = 8;

/// A monotonic clock, counting [`Clock::HZ`] ticks a second.
pub trait Clock {
    const HZ: u64;
    fn now(&self) -> u64;
}

/// `us` in `K`'s ticks, rounded up.
fn ticks<K: Clock>(us: u64) -> u64 {
    (us * K::HZ).div_ceil(1_000_000)
}

/// `ticks` of `K` in whole microseconds, rounded down.
fn micros<K: Clock>(ticks: u64) -> u64 {
    ticks * 1_000_000 / K::HZ
}

/// A command's reply: its status and the parameters after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u8,
    pub params: [u8; RETURN_PARAMS_MAX],
    pub len: usize,
}

/// Why a command has no reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// Its parameters run past 255 bytes; nothing was sent.
    TooLong,
    /// The chip did not take it.
    NotTaken,
    /// The chip restarted by itself since the last reset, so nothing was sent: in its updater,
    /// say, it takes commands of its own.
    Restarted,
    /// None came within [`REPLY_WITHIN_US`], or the chip restarted first.
    NoReply,
}

/// What a drain hands out next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "no allocation: a value travels inline")]
pub enum Got {
    /// A packet, copied out, and what it tells the core.
    Packet { len: usize, event: Option<RadioEvent> },
    /// The boot a reset asked for, copied out: its own, never an event.
    Boot { len: usize, reason: Option<BootReason> },
    /// The chip's header was unreadable, and the drain ends; `first` on the read that made the run.
    Unreadable { header: [u8; 5], first: bool },
    /// What the driver itself reports: an operation's end, or packets it dropped.
    Event(RadioEvent),
}

pub struct Driver<'c, B, C, I, D, R, K> {
    transport: Transport<'c, B, C, I, D>,
    delay: D,
    reset: R,
    clock: K,
    translator: Translator,
    ops: Ops,
    peers: Peers,
    /// The opcode whose reply is awaited, until `deadline`, and the reply once it came.
    awaited: Option<u16>,
    deadline: u64,
    reply: Option<Reply>,
    /// The chip announced a boot nobody asked for since the last reset.
    restarted: bool,
    /// A reset's boot is still to come: what arrives before it is from before the reset.
    booting: bool,
    /// A read was cut short while the boot was still to come: lost, unless the boot follows it.
    lost_early: bool,
    /// The last read, how far its packets have been handed out, and the drain it belongs to.
    read: [u8; MAX_READ],
    len: usize,
    at: usize,
    drain: Option<Drain>,
}

/// A drain under way: the reads it made, and whether the last gave bytes, after which it yields.
#[derive(Clone, Copy, Debug, Default)]
struct Drain {
    reads: u32,
    yield_due: bool,
}

impl<'c, B, C, I, D, R, K> Driver<'c, B, C, I, D, R, K>
where
    B: SpiBus,
    C: OutputPin,
    I: InputPin + Wait,
    D: DelayNs + Clone,
    R: OutputPin,
    K: Clock,
{
    pub fn new(bus: B, cs: C, irq: I, delay: D, counts: &'c Counts, reset: R, clock: K) -> Self {
        Self {
            transport: Transport::new(bus, cs, irq, delay.clone(), counts),
            delay,
            reset,
            clock,
            translator: Translator::default(),
            ops: Ops::default(),
            peers: Peers::default(),
            awaited: None,
            deadline: 0,
            reply: None,
            restarted: false,
            booting: false,
            lost_early: false,
            read: [0; MAX_READ],
            len: 0,
            at: 0,
            drain: None,
        }
    }

    #[must_use]
    pub fn counts(&self) -> &'c Counts {
        self.transport.counts()
    }

    /// Until a read is waiting.
    pub async fn wait_pending(&mut self) {
        self.transport.wait_pending().await;
    }

    /// The last reads' headers were unreadable: the caller reads no faster than it must.
    #[must_use]
    pub fn backing_off(&self) -> bool {
        self.transport.backing_off()
    }

    /// The chip held in reset.
    pub fn hold(&mut self) {
        let _ = self.reset.set_low();
    }

    /// The chip let out of reset, with no start waiting for its boot.
    pub fn release(&mut self) {
        let _ = self.reset.set_high();
    }

    /// The chip announced a boot nobody asked for since the last reset: [`Driver::send`] refuses.
    #[must_use]
    pub fn restarted(&self) -> bool {
        self.restarted
    }

    /// The local server as the chip built it, until the chip next boots.
    pub fn serve(&mut self, server: Handles) {
        self.translator.serve(server);
    }

    #[must_use]
    pub fn server(&self) -> Option<&Handles> {
        self.translator.server()
    }

    /// The chip reset through its pin, CS high first; what was read before is dropped, and the
    /// boot this reset gives is its own (see [`Driver::boot`]).
    pub async fn reset(&mut self) {
        self.transport.deselect();
        self.hold();
        self.delay.delay_ms(RESET_MS).await;
        self.release();
        self.awaited = None;
        self.reply = None;
        self.restarted = false;
        self.saw(&RadioEvent::ChipReset(wrist_ble::ResetCause::Other));
        self.len = 0;
        self.at = 0;
        self.drain = None;
        self.booting = true;
        self.lost_early = false;
    }

    /// Whether the chip announced itself within [`BOOT_WITHIN_US`] of the reset; if it did, a drain
    /// hands out [`Got::Boot`] for its boot and drops whatever came before it.
    pub async fn boot(&mut self) -> bool {
        let up = self.transport.wait_pending_within(BOOT_WITHIN_US).await;
        if up {
            self.drain();
        } else {
            self.booting = false;
        }
        up
    }

    /// Sends `opcode` with `params` and awaits its reply: [`Driver::awaiting`], then
    /// [`Driver::reply`].
    ///
    /// # Errors
    ///
    /// The chip restarted by itself, the parameters run past 255 bytes, or the chip did not take
    /// the command.
    pub async fn send(&mut self, opcode: u16, params: &[u8]) -> Result<(), Unanswered> {
        if self.restarted {
            return Err(Unanswered::Restarted);
        }
        let mut packet = [0u8; bluenrg2::MAX_COMMAND];
        let n = bluenrg2::command(opcode, &[params], &mut packet).ok_or(Unanswered::TooLong)?;
        self.awaited = Some(opcode);
        self.reply = None;
        if !self.transport.write(&packet[..n]).await {
            self.awaited = None;
            return Err(Unanswered::NotTaken);
        }
        self.deadline = self.clock.now() + ticks::<K>(REPLY_WITHIN_US);
        Ok(())
    }

    /// Whether the reply is still to come: until it does, the chip restarts or the time is up, each
    /// call waits for the chip and begins a drain for [`Driver::next`].
    pub async fn awaiting(&mut self) -> bool {
        if self.reply.is_some() || self.restarted {
            return false;
        }
        let left = self.deadline.saturating_sub(self.clock.now());
        if left == 0 || !self.transport.wait_pending_within(u32::try_from(micros::<K>(left)).unwrap_or(u32::MAX)).await {
            return false;
        }
        self.drain();
        true
    }

    /// The reply, which ends the exchange.
    ///
    /// # Errors
    ///
    /// None came in time, or the chip restarted first.
    pub fn reply(&mut self) -> Result<Reply, Unanswered> {
        self.awaited = None;
        self.reply.take().ok_or(Unanswered::NoReply)
    }

    /// Begins handing out what the chip has queued: up to [`DRAIN_READS`] reads while IRQ stays high.
    pub fn drain(&mut self) {
        self.drain = Some(Drain::default());
    }

    /// `op`, sent as its command and answered with `reply`: taken, with its id, or refused; its
    /// end comes as [`Got::Event`].
    ///
    /// # Errors
    ///
    /// The chip refused it, never answered, or restarted around its answer.
    pub fn settle(&mut self, op: &Op, reply: &Result<Reply, Unanswered>) -> Result<OpId, Failure> {
        match reply {
            Ok(r) if self.restarted && r.status == 0 && op.kind().ends_with_event() => Err(ops::unanswered(Unanswered::Restarted)),
            Ok(r) => self.ops.answered(op, r.status, self.clock.now() + ticks::<K>(u64::from(op_deadline(op.kind()).0) * 1_000)),
            Err(why) => Err(ops::unanswered(*why)),
        }
    }

    /// The earliest deadline on the clock of the operations taken and not ended.
    #[must_use]
    pub fn due(&self) -> Option<u64> {
        self.ops.due()
    }

    /// What the driver has to report, then the drain's next packet, copied into `out`; `None` once
    /// the drain is over.
    pub async fn next(&mut self, out: &mut [u8; MAX_READ]) -> Option<Got> {
        self.ops.expire(self.clock.now());
        if let Some(e) = self.ops.ended() {
            return Some(Got::Event(e));
        }
        loop {
            if self.at < self.len {
                let Some(packet) = packets(&self.read[self.at..self.len], self.transport.counts()).next() else {
                    self.at = self.len;
                    if self.booting {
                        self.lost_early = true;
                        continue;
                    }
                    return Some(self.lost());
                };
                let len = packet.len();
                out[..len].copy_from_slice(packet);
                self.at += len;
                if let Some(got) = self.take(&out[..len]) {
                    return Some(got);
                }
                continue;
            }
            let mut drain = self.drain?;
            if core::mem::take(&mut drain.yield_due) {
                yield_now().await;
            }
            if !self.transport.pending() || drain.reads >= DRAIN_READS {
                self.drain = None;
                self.booting = false;
                if core::mem::take(&mut self.lost_early) {
                    return Some(self.lost());
                }
                return None;
            }
            drain.reads += 1;
            match self.transport.read(&mut self.read).await {
                Read::Bytes(n) => {
                    self.len = n;
                    self.at = 0;
                    drain.yield_due = true;
                }
                Read::Empty => {}
                Read::Unreadable { header, first } => {
                    self.drain = None;
                    self.booting = false;
                    return Some(Got::Unreadable { header, first });
                }
            }
            self.drain = Some(drain);
        }
    }

    /// Events lost: what the operations knew of the links is let go.
    fn lost(&mut self) -> Got {
        self.ops.saw(&RadioEvent::EventsLost);
        Got::Event(RadioEvent::EventsLost)
    }

    /// One packet: the awaited reply kept, a reset's boot kept back, the rest translated; `None` for
    /// a packet from before a reset.
    fn take(&mut self, packet: &[u8]) -> Option<Got> {
        let len = packet.len();
        let parsed = bluenrg2::parse(packet);
        if self.booting {
            let Event::Vendor(VendorEvent::BlueInitialized { reason, .. }) = parsed else { return None };
            self.booting = false;
            self.lost_early = false;
            // What the chip held is gone, as for any boot; the boot itself is the reset's answer.
            if let Some(e) = self.translator.event(packet) {
                self.saw(&e);
            }
            return Some(Got::Boot { len, reason });
        }
        match parsed {
            Event::CommandComplete { cmd_opcode, status: Some(status), return_param_bytes: params, .. } if self.awaited == Some(cmd_opcode) => {
                let mut reply = Reply { status: status.into_inner(), params: [0; RETURN_PARAMS_MAX], len: params.len().min(RETURN_PARAMS_MAX) };
                reply.params[..reply.len].copy_from_slice(&params[..reply.len]);
                self.reply = Some(reply);
            }
            Event::CommandStatus { cmd_opcode, status, .. } if self.awaited == Some(cmd_opcode) => {
                self.reply = Some(Reply { status: status.into_inner(), params: [0; RETURN_PARAMS_MAX], len: 0 });
            }
            Event::Vendor(VendorEvent::BlueInitialized { .. }) => self.restarted = true,
            Event::Vendor(VendorEvent::L2capUpdateResponse { handle, accepted: true, .. }) => self.ops.accepted(handle),
            _ => {}
        }
        let event = self.translator.event(packet);
        if let Some(e) = &event {
            self.saw(e);
        }
        Some(Got::Packet { len, event })
    }

    /// What the operations and the links' peers make of `event`.
    fn saw(&mut self, event: &RadioEvent) {
        self.ops.saw(event);
        self.peers.saw(event);
    }

    /// A snapshot found only `links`: operations on the others end.
    pub(crate) fn resynced(&mut self, links: &[Option<wrist_ble::LinkStatus>]) {
        self.ops.resynced(links);
    }

    pub(crate) fn peers(&self) -> &Peers {
        &self.peers
    }

    pub(crate) fn now_ms(&self) -> u64 {
        micros::<K>(self.clock.now()) / 1_000
    }
}
