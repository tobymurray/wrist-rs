//! A start: the chip reset, its boot, then configured and its local server built in ST's sample
//! order, each step pulled by the caller with [`Driver::starting`].

use bluenrg2::{config, gap_role, opcode, privacy, AuthenticationRequirement};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;
use wrist_ble::ServerTable;

use crate::commands::Command;
use crate::driver::{Clock, Driver, Got, Reply, Unanswered};
use crate::gatt::{self, Handles};
use crate::transport::MAX_READ;

/// What the chip is configured with.
#[derive(Clone, Copy, Debug)]
pub struct Setup<'a> {
    /// The static random address, as the chip holds it.
    pub address: [u8; 6],
    /// `ACI_HAL_SET_TX_POWER_LEVEL`'s parameters.
    pub tx_power: [u8; 2],
    pub event_mask: [u8; 8],
    /// The GAP device name, and the room `ACI_GAP_INIT` keeps for it.
    pub name: &'a [u8],
    pub name_room: u8,
    pub io_capability: u8,
    pub pairing: AuthenticationRequirement,
}

/// The step a start stopped at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    StaticRandomAddress,
    /// Pairing failed at the `DHKey` check until this slot had been written (HARDWARE.md).
    PublicAddressSlot,
    TxPower,
    EventMask,
    GattInit,
    GapInit,
    DeviceName,
    IoCapability,
    SecurityRequirements,
    /// Adding the service so named, one of its characteristics, or setting one's value.
    Service(&'static str),
    Characteristic(&'static str),
    Value(&'static str),
}

/// What a start hands out next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "no allocation: a value travels inline")]
pub enum Starting {
    Got(Got),
    /// The command about to be sent.
    Sending(Command),
    /// No boot within [`crate::driver::BOOT_WITHIN_US`] of the reset; the start goes on.
    Silent,
    /// The boot's window is over and the next step is the first command: the caller may stop here.
    Booted,
    /// `ACI_GAP_INIT` refused these roles, or went unanswered; fewer roles are asked next.
    GapRefused { roles: u8, status: u8 },
    GapUnanswered { why: Unanswered },
    /// The roles the chip took: whether the central's among them.
    Roles { central: bool },
    /// The start is over, at this step.
    Failed(Step),
    /// The start is over: the chip is configured and the server built, with these handles.
    Up(Handles),
}

/// The roles `ACI_GAP_INIT` is asked for, in turn: the peripheral alone if the chip refuses both,
/// so the console never depends on the central role.
const ROLES: [u8; 2] = [gap_role::PERIPHERAL | gap_role::CENTRAL, gap_role::PERIPHERAL];

const EARLY: [Step; 5] = [Step::StaticRandomAddress, Step::PublicAddressSlot, Step::TxPower, Step::EventMask, Step::GattInit];
const LATE: [Step; 3] = [Step::DeviceName, Step::IoCapability, Step::SecurityRequirements];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Reset,
    Boot,
    Booted,
    Send(Cmd),
    Write(Cmd),
    Wait(Cmd),
    Fail(Step),
    Up,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmd {
    Early(usize),
    Gap(usize),
    Late(usize),
    Service(usize),
    Char(usize, usize),
    Value(usize, usize),
}

/// A start in progress.
pub struct Start<'a> {
    server: &'a ServerTable,
    setup: Setup<'a>,
    stage: Stage,
    /// GAP's service and device-name characteristic, the service being built and the last
    /// characteristic's declaration.
    gap: (u16, u16),
    service: u16,
    declaration: u16,
    handles: Handles,
}

impl<'a> Start<'a> {
    #[must_use]
    pub fn new(server: &'a ServerTable, setup: Setup<'a>) -> Self {
        Self { server, setup, stage: Stage::Reset, gap: (0, 0), service: 0, declaration: 0, handles: Handles::default() }
    }

    fn name(&self, service: usize) -> &'static str {
        self.server.services[service].name
    }

    /// The step `cmd` is part of, for the line that names it.
    fn step(&self, cmd: Cmd) -> Step {
        match cmd {
            Cmd::Early(i) => EARLY[i],
            Cmd::Gap(_) => Step::GapInit,
            Cmd::Late(i) => LATE[i],
            Cmd::Service(i) => Step::Service(self.name(i)),
            Cmd::Char(i, _) => Step::Characteristic(self.name(i)),
            Cmd::Value(i, _) => Step::Value(self.name(i)),
        }
    }

    /// The command `cmd` sends; `None` for a declaration no command holds.
    fn command(&self, cmd: Cmd) -> Option<Command> {
        let s = &self.setup;
        let address = |slot: u8| {
            let mut p = [slot, 6, 0, 0, 0, 0, 0, 0];
            p[2..].copy_from_slice(&s.address);
            p
        };
        match cmd {
            Cmd::Early(0) => Command::new(opcode::ACI_HAL_WRITE_CONFIG_DATA, &[&address(config::STATIC_RANDOM_ADDRESS)]),
            Cmd::Early(1) => Command::new(opcode::ACI_HAL_WRITE_CONFIG_DATA, &[&address(config::PUBLIC_ADDRESS)]),
            Cmd::Early(2) => Command::new(opcode::ACI_HAL_SET_TX_POWER_LEVEL, &[&s.tx_power]),
            Cmd::Early(3) => Command::new(opcode::HCI_SET_EVENT_MASK, &[&s.event_mask]),
            Cmd::Early(_) => Command::new(opcode::ACI_GATT_INIT, &[]),
            Cmd::Gap(i) => Command::new(opcode::ACI_GAP_INIT, &[&[ROLES[i], privacy::DISABLED, s.name_room]]),
            Cmd::Late(0) => {
                let (service, name) = self.gap;
                let len = u8::try_from(s.name.len()).ok()?;
                Command::new(opcode::ACI_GATT_UPDATE_CHAR_VALUE, &[&service.to_le_bytes(), &name.to_le_bytes(), &[0, len], s.name])
            }
            Cmd::Late(1) => Command::new(opcode::ACI_GAP_SET_IO_CAPABILITY, &[&[s.io_capability]]),
            Cmd::Late(_) => Command::new(opcode::ACI_GAP_SET_AUTHENTICATION_REQUIREMENT, &[&s.pairing.params()]),
            Cmd::Service(i) => gatt::add_service(&self.server.services[i]),
            Cmd::Char(i, j) => gatt::add_char(self.service, &self.server.services[i].chars[j]),
            Cmd::Value(i, j) => gatt::initial(self.service, self.declaration, &self.server.services[i].chars[j]),
        }
    }

    /// The characteristic after `(i, j)`, the next service's first, or the end.
    fn after_char(&self, i: usize, j: usize) -> Stage {
        if j + 1 < self.server.services[i].chars.len() {
            Stage::Send(Cmd::Char(i, j + 1))
        } else {
            self.after_service(i)
        }
    }

    fn first_service(&self) -> Stage {
        if self.server.services.is_empty() {
            Stage::Up
        } else {
            Stage::Send(Cmd::Service(0))
        }
    }

    fn after_service(&self, i: usize) -> Stage {
        if i + 1 < self.server.services.len() {
            Stage::Send(Cmd::Service(i + 1))
        } else {
            Stage::Up
        }
    }

    /// What follows `cmd`, answered with `reply`, and what to hand out on the way.
    fn after(&mut self, cmd: Cmd, reply: &Result<Reply, Unanswered>) -> (Stage, Option<Starting>) {
        let ok = matches!(reply, Ok(r) if r.status == 0);
        let handle = match reply {
            Ok(r) if r.status == 0 && r.len >= 2 => Some(u16::from_le_bytes([r.params[0], r.params[1]])),
            _ => None,
        };
        let fail = (Stage::Fail(self.step(cmd)), None);
        match cmd {
            Cmd::Early(i) if ok => (Stage::Send(if i + 1 < EARLY.len() { Cmd::Early(i + 1) } else { Cmd::Gap(0) }), None),
            Cmd::Late(i) if ok => (if i + 1 < LATE.len() { Stage::Send(Cmd::Late(i + 1)) } else { self.first_service() }, None),
            Cmd::Gap(i) => match reply {
                Ok(r) if r.status == 0 && r.len >= 6 => {
                    self.gap = (u16::from_le_bytes([r.params[0], r.params[1]]), u16::from_le_bytes([r.params[2], r.params[3]]));
                    (Stage::Send(Cmd::Late(0)), Some(Starting::Roles { central: ROLES[i] & gap_role::CENTRAL != 0 }))
                }
                answered => {
                    let note = match answered {
                        Ok(r) => Starting::GapRefused { roles: ROLES[i], status: r.status },
                        Err(why) => Starting::GapUnanswered { why: *why },
                    };
                    (if i + 1 < ROLES.len() { Stage::Send(Cmd::Gap(i + 1)) } else { Stage::Fail(Step::GapInit) }, Some(note))
                }
            },
            Cmd::Service(i) => match handle {
                Some(h) => {
                    self.service = h;
                    (if self.server.services[i].chars.is_empty() { self.after_service(i) } else { Stage::Send(Cmd::Char(i, 0)) }, None)
                }
                None => fail,
            },
            Cmd::Char(i, j) => match handle.filter(|&h| self.handles.push(self.service, h, &self.server.services[i].chars[j])) {
                Some(h) => {
                    self.declaration = h;
                    let value = gatt::initial(self.service, h, &self.server.services[i].chars[j]).is_some();
                    (if value { Stage::Send(Cmd::Value(i, j)) } else { self.after_char(i, j) }, None)
                }
                None => fail,
            },
            Cmd::Value(i, j) if ok => (self.after_char(i, j), None),
            Cmd::Early(_) | Cmd::Late(_) | Cmd::Value(..) => fail,
        }
    }
}

impl<B, C, I, D, R, K> Driver<'_, B, C, I, D, R, K>
where
    B: SpiBus,
    C: OutputPin,
    I: InputPin + Wait,
    D: DelayNs + Clone,
    R: OutputPin,
    K: Clock,
{
    /// The start's next step: what a drain hands out first, then each note, until
    /// [`Starting::Failed`] or [`Starting::Up`]; `None` after.
    pub async fn starting(&mut self, s: &mut Start<'_>, out: &mut [u8; MAX_READ]) -> Option<Starting> {
        loop {
            if let Some(got) = self.next(out).await {
                return Some(Starting::Got(got));
            }
            match s.stage {
                Stage::Reset => {
                    self.reset().await;
                    s.stage = Stage::Boot;
                }
                Stage::Boot => {
                    s.stage = Stage::Booted;
                    if !self.boot().await {
                        return Some(Starting::Silent);
                    }
                }
                Stage::Booted => {
                    s.stage = Stage::Send(Cmd::Early(0));
                    return Some(Starting::Booted);
                }
                Stage::Send(cmd) => {
                    let Some(c) = s.command(cmd) else {
                        s.stage = Stage::Fail(s.step(cmd));
                        continue;
                    };
                    if !self.restarted() {
                        s.stage = Stage::Write(cmd);
                        return Some(Starting::Sending(c));
                    }
                    let (stage, note) = s.after(cmd, &Err(Unanswered::Restarted));
                    s.stage = stage;
                    if note.is_some() {
                        return note;
                    }
                }
                Stage::Write(cmd) => {
                    let Some(c) = s.command(cmd) else {
                        s.stage = Stage::Fail(s.step(cmd));
                        continue;
                    };
                    match self.send(c.opcode, c.params()).await {
                        Ok(()) => s.stage = Stage::Wait(cmd),
                        Err(why) => {
                            let (stage, note) = s.after(cmd, &Err(why));
                            s.stage = stage;
                            if note.is_some() {
                                return note;
                            }
                        }
                    }
                }
                Stage::Wait(cmd) => {
                    if self.awaiting().await {
                        continue;
                    }
                    let reply = self.reply();
                    let (stage, note) = s.after(cmd, &reply);
                    s.stage = stage;
                    if note.is_some() {
                        return note;
                    }
                }
                Stage::Fail(step) => {
                    s.stage = Stage::Done;
                    return Some(Starting::Failed(step));
                }
                Stage::Up => {
                    s.stage = Stage::Done;
                    return Some(Starting::Up(s.handles));
                }
                Stage::Done => return None,
            }
        }
    }
}
