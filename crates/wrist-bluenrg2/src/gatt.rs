//! A declared GATT server as the chip's commands, and the handles the chip gives it.

use bluenrg2::{enc_key_size, evt_mask, opcode, permission, service_type, uuid_type, value_length};
use wrist_ble::{BluetoothUuid, CharDecl, Permission, Props, ServerAttr, ServiceDecl, ATT_MAX_MTU};

use crate::commands::Command;

/// The longest value set here: what a notification at the largest MTU carries.
pub const MAX_VALUE: usize = ATT_MAX_MTU - 3;

/// The longest parameters here: a whole value after its handles, offset and length.
pub const MAX_PARAMS: usize = 6 + MAX_VALUE;

/// Writes only over a link encrypted with authenticated keys.
const AUTHENTICATED_WRITE: u8 = permission::AUTHEN_WRITE | permission::ENCRY_WRITE;

/// The most characteristics a server here holds.
pub const MAX_CHARS: usize = 16;

/// The chip's UUID type and the bytes, little-endian; a 32-bit UUID goes as its 128 bits.
fn uuid(u: &BluetoothUuid) -> (u8, [u8; 16], usize) {
    match u {
        BluetoothUuid::Uuid16(_) => {
            let mut b = [0; 16];
            b[..2].copy_from_slice(u.as_le_slice());
            (uuid_type::U16, b, 2)
        }
        _ => (uuid_type::U128, u.to_u128().to_le_bytes(), 16),
    }
}

fn notifies(c: &CharDecl) -> bool {
    c.props.has(Props::NOTIFY) || c.props.has(Props::INDICATE)
}

fn writable(c: &CharDecl) -> bool {
    c.props.has(Props::WRITE) || c.props.has(Props::WRITE_WITHOUT_RESPONSE)
}

/// The attribute records a service takes: its declaration, two per characteristic and a
/// configuration descriptor for each that notifies or indicates.
#[must_use]
pub fn records(s: &ServiceDecl) -> usize {
    1 + s.chars.iter().map(|c| 2 + usize::from(notifies(c))).sum::<usize>()
}

/// `ACI_GATT_ADD_SERVICE` for `s`; `None` for one with more records than the command holds.
#[must_use]
pub fn add_service(s: &ServiceDecl) -> Option<Command<MAX_PARAMS>> {
    let (kind, bytes, n) = uuid(&s.uuid);
    let bytes = &bytes[..n];
    let records = u8::try_from(records(s)).ok()?;
    Command::new(opcode::ACI_GATT_ADD_SERVICE, &[&[kind], bytes, &[service_type::PRIMARY, records]])
}

/// `ACI_GATT_ADD_CHAR` for `c` in the service at `service`, its value variable up to its length,
/// and every write reported. `None` for a readable characteristic that is not open, whose read
/// permission no command here sets yet.
#[must_use]
pub fn add_char(service: u16, c: &CharDecl) -> Option<Command<MAX_PARAMS>> {
    let (kind, bytes, n) = uuid(&c.uuid);
    let bytes = &bytes[..n];
    if c.props.has(Props::READ) && c.permission != Permission::Open {
        return None;
    }
    let security = match c.permission {
        _ if !writable(c) => permission::NONE,
        Permission::Open => permission::NONE,
        Permission::Encrypted => permission::ENCRY_WRITE,
        Permission::Authenticated => AUTHENTICATED_WRITE,
    };
    let events = if writable(c) { evt_mask::ATTRIBUTE_WRITE } else { evt_mask::NONE };
    Command::new(
        opcode::ACI_GATT_ADD_CHAR,
        &[
            &service.to_le_bytes(),
            &[kind],
            bytes,
            &c.max_len.to_le_bytes(),
            &[c.props.0, security, events, enc_key_size::MIN, value_length::VARIABLE],
        ],
    )
}

/// `ACI_GATT_UPDATE_CHAR_VALUE`: the characteristic at `char` in the service at `service` holds
/// `value` from its start, and a subscribed peer is notified. `None` for a value longer than
/// [`MAX_VALUE`].
#[must_use]
pub fn set_value(service: u16, char: u16, value: &[u8]) -> Option<Command<MAX_PARAMS>> {
    let len = u8::try_from(value.len()).ok().filter(|&l| usize::from(l) <= MAX_VALUE)?;
    Command::new(opcode::ACI_GATT_UPDATE_CHAR_VALUE, &[&service.to_le_bytes(), &char.to_le_bytes(), &[0, len], value])
}

/// [`set_value`] with what `c` holds from the start, cut to its length; `None` for none.
#[must_use]
pub fn initial(service: u16, char: u16, c: &CharDecl) -> Option<Command<MAX_PARAMS>> {
    let value = &c.value[..c.value.len().min(usize::from(c.max_len))];
    if value.is_empty() {
        return None;
    }
    set_value(service, char, value)
}

/// Which attribute of a characteristic a handle is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Value,
    /// Its client characteristic configuration descriptor.
    Config,
}

/// The chip's handles for each characteristic of a server, in [`ServerAttr`] order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Handles {
    /// The service's handle, the characteristic's declaration handle, and whether it has a
    /// configuration descriptor.
    chars: [(u16, u16, bool); MAX_CHARS],
    len: usize,
}

impl Handles {
    /// The next characteristic, `c`, as the chip answered `ACI_GATT_ADD_CHAR`; false once full.
    pub fn push(&mut self, service: u16, declaration: u16, c: &CharDecl) -> bool {
        let Some(slot) = self.chars.get_mut(self.len) else { return false };
        *slot = (service, declaration, notifies(c));
        self.len += 1;
        true
    }

    /// The service's handle and the characteristic's declaration handle.
    #[must_use]
    pub fn get(&self, attr: ServerAttr) -> Option<(u16, u16)> {
        self.chars[..self.len].get(usize::from(attr.0)).map(|&(s, d, _)| (s, d))
    }

    /// The characteristic a handle the chip reports belongs to, and which part of it.
    #[must_use]
    pub fn find(&self, handle: u16) -> Option<(ServerAttr, Part)> {
        self.chars[..self.len].iter().enumerate().find_map(|(i, &(_, decl, config))| {
            let part = match handle.checked_sub(decl)? {
                1 => Part::Value,
                2 if config => Part::Config,
                _ => return None,
            };
            Some((ServerAttr(u8::try_from(i).ok()?), part))
        })
    }
}
