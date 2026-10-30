use crate::BluetoothUuid;

/// An attribute on a peer's GATT server, as the driver reported it in a discovery outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttRef(pub u16);

/// A peer's service, from its first attribute to its last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ServiceRef {
    pub start: AttRef,
    pub end: AttRef,
}

/// A peer's characteristic: its value and, if it has one, its configuration descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CharRef {
    pub value: AttRef,
    pub cccd: Option<AttRef>,
}

/// A characteristic of the local GATT server: its position in [`ServerTable`], counting every
/// service's characteristics in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ServerAttr(pub u8);

/// Characteristic properties (Core Vol 3 Part G §3.3.1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Props(pub u8);

impl Props {
    pub const READ: Self = Self(0x02);
    pub const WRITE_WITHOUT_RESPONSE: Self = Self(0x04);
    pub const WRITE: Self = Self(0x08);
    pub const NOTIFY: Self = Self(0x10);
    pub const INDICATE: Self = Self(0x20);

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// What a peer must have done before it may read or write a characteristic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    Open,
    Encrypted,
    /// Encrypted with keys from an authenticated (MITM-protected) pairing.
    Authenticated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharDecl {
    pub uuid: BluetoothUuid,
    pub props: Props,
    pub permission: Permission,
    pub max_len: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceDecl {
    pub uuid: BluetoothUuid,
    pub chars: &'static [CharDecl],
}

/// The local GATT server, declared once; the driver maps each [`ServerAttr`] to its handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerTable {
    pub services: &'static [ServiceDecl],
}

impl ServerTable {
    /// The characteristic `attr` names, if the table has it.
    #[must_use]
    pub fn get(&self, attr: ServerAttr) -> Option<&'static CharDecl> {
        self.services.iter().flat_map(|s| s.chars.iter()).nth(usize::from(attr.0))
    }
}
