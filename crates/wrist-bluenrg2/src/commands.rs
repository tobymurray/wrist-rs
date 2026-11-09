//! The core's operations as BlueNRG-2 commands.

use bluenrg2::{ad, address, opcode, ADV_IND, NO_FILTER};
use wrist_ble::{ConnHandle, DisconnectReason, LeConnRole, Op};

/// The longest complete local name the chip's advertisement holds: its 31 bytes less the flags
/// and TX power level the chip adds (3 bytes each) and the name's own length and type (HARDWARE.md).
pub const MAX_NAME: usize = 31 - 3 - 3 - 2;

/// The longest parameters a command here writes: `ACI_GAP_SET_DISCOVERABLE` with the longest name.
pub const MAX_PARAMS: usize = 14 + MAX_NAME;

/// One command: its opcode and its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub opcode: u16,
    params: [u8; MAX_PARAMS],
    len: usize,
}

impl Command {
    #[must_use]
    pub fn params(&self) -> &[u8] {
        &self.params[..self.len]
    }

    fn new(opcode: u16, parts: &[&[u8]]) -> Option<Self> {
        let mut c = Self { opcode, params: [0; MAX_PARAMS], len: 0 };
        for part in parts {
            let end = c.len.checked_add(part.len()).filter(|&e| e <= MAX_PARAMS)?;
            c.params[c.len..end].copy_from_slice(part);
            c.len = end;
        }
        Some(c)
    }
}

/// The command that carries `op` out, the watch advertising as `name` (the complete local name)
/// and holding each link in the role `role` gives it. `None` for an operation no command here
/// carries out yet: as the central, scanning, GATT, and advertising that takes no connection;
/// for a name longer than [`MAX_NAME`]; and for a disconnect reason ST does not list.
pub fn command(op: &Op, name: &[u8], role: impl Fn(ConnHandle) -> Option<LeConnRole>) -> Option<Command> {
    let h = |l: &ConnHandle| l.raw().to_le_bytes();
    match op {
        Op::Advertise(Some(p)) if p.connectable && name.len() <= MAX_NAME => {
            let name_len = u8::try_from(name.len() + 1).ok()?;
            // Interval min and max, own address type, filter policy, the name as AD data, no
            // service UUIDs, no slave connection interval.
            Command::new(
                opcode::ACI_GAP_SET_DISCOVERABLE,
                &[
                    &[ADV_IND],
                    &p.interval_min.as_u16().to_le_bytes(),
                    &p.interval_max.as_u16().to_le_bytes(),
                    &[address::STATIC_RANDOM, NO_FILTER, name_len, ad::NAME_COMPLETE],
                    name,
                    &[0, 0, 0, 0, 0],
                ],
            )
        }
        Op::Advertise(None) => Command::new(opcode::ACI_GAP_SET_NON_DISCOVERABLE, &[]),
        Op::UpdateParams(l, p) if role(*l) == Some(LeConnRole::Peripheral) => Command::new(
            opcode::ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ,
            &[
                &h(l),
                &p.interval_min.as_u16().to_le_bytes(),
                &p.interval_max.as_u16().to_le_bytes(),
                &p.latency.to_le_bytes(),
                &p.timeout.as_u16().to_le_bytes(),
            ],
        ),
        Op::RequestSecurity(l) => Command::new(opcode::ACI_GAP_SLAVE_SECURITY_REQ, &[&h(l)]),
        Op::AllowRebond(l) => Command::new(opcode::ACI_GAP_ALLOW_REBOND, &[&h(l)]),
        Op::ConfirmNumeric(l, yes) => Command::new(opcode::ACI_GAP_NUMERIC_COMPARISON_CONFIRM, &[&h(l), &[u8::from(*yes)]]),
        Op::Passkey(l, key) => Command::new(opcode::ACI_GAP_PASS_KEY_RESP, &[&h(l), &key.to_le_bytes()]),
        // ST lists 0x05, 0x13, 0x14, 0x15, 0x1A and 0x3B for `ACI_GAP_TERMINATE`; the chip takes
        // 0x29 too (HARDWARE.md), but it is a BR/EDR reason no link here ends with.
        Op::Disconnect(_, DisconnectReason::PairingWithUnitKeyNotSupported) => None,
        Op::Disconnect(l, reason) => Command::new(opcode::ACI_GAP_TERMINATE, &[&h(l), &[*reason as u8]]),
        _ => None,
    }
}
