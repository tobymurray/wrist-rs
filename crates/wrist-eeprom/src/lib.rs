#![cfg_attr(not(test), no_std)]

//! Read-only access to an onsemi N24S64B EEPROM (N24S64B/D): nothing here sends a data byte.

pub mod info;

use embedded_hal::i2c::I2c;
use info::{ModuleInfo, RECORD_BYTES};

const ARRAY_HEADER: u8 = 0x50;
const SPECIAL_HEADER: u8 = 0x58;

const SECURE_PAGE_WORD: u16 = 0x0000;
const UNIQUE_ID_WORD: u16 = 0x0200;
const CONFIG_WORD: u16 = 0x0600;

pub const ARRAY_BYTES: u16 = 8192;
pub const SECURE_PAGE_BYTES: usize = 32;
pub const UNIQUE_ID_BYTES: usize = 16;
/// Short enough that no one transfer holds the bus for long.
pub const RECORD_PIECE: usize = 25;

pub struct Eeprom<I2C> {
    bus: I2C,
    pins: u8,
}

impl<I2C: I2c> Eeprom<I2C> {
    pub fn new(bus: I2C) -> Self {
        Self::with_address_bits(bus, 0)
    }

    /// # Panics
    /// If `a2a1a0` is above 7.
    pub fn with_address_bits(bus: I2C, a2a1a0: u8) -> Self {
        assert!(a2a1a0 < 8);
        Self { bus, pins: a2a1a0 }
    }

    pub fn release(self) -> I2C {
        self.bus
    }

    pub fn present(&mut self) -> bool {
        let mut byte = [0u8; 1];
        self.read_array(0, &mut byte).is_ok()
    }

    /// # Errors
    /// The bus's error.
    pub fn read_array(&mut self, offset: u16, buf: &mut [u8]) -> Result<(), I2C::Error> {
        debug_assert!(offset as usize + buf.len() <= ARRAY_BYTES as usize);
        self.bus.write_read(ARRAY_HEADER | self.pins, &offset.to_be_bytes(), buf)
    }

    /// `None` when the array holds no valid record.
    ///
    /// # Errors
    /// The bus's error.
    pub fn module_info(&mut self) -> Result<Option<ModuleInfo>, I2C::Error> {
        Ok(ModuleInfo::parse(&self.record()?))
    }

    /// # Errors
    /// The bus's error.
    pub fn record(&mut self) -> Result<[u8; RECORD_BYTES], I2C::Error> {
        let mut record = [0u8; RECORD_BYTES];
        for (i, piece) in record.chunks_mut(RECORD_PIECE).enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            self.read_array((i * RECORD_PIECE) as u16, piece)?;
        }
        Ok(record)
    }

    /// # Errors
    /// The bus's error.
    pub fn unique_id(&mut self) -> Result<[u8; UNIQUE_ID_BYTES], I2C::Error> {
        let mut id = [0u8; UNIQUE_ID_BYTES];
        self.read_special(UNIQUE_ID_WORD, &mut id)?;
        Ok(id)
    }

    /// # Errors
    /// The bus's error.
    pub fn secure_page(&mut self) -> Result<[u8; SECURE_PAGE_BYTES], I2C::Error> {
        let mut page = [0u8; SECURE_PAGE_BYTES];
        self.read_special(SECURE_PAGE_WORD, &mut page)?;
        Ok(page)
    }

    /// The Device Configuration Register (N24S64B/D).
    ///
    /// # Errors
    /// The bus's error.
    pub fn config(&mut self) -> Result<u8, I2C::Error> {
        let mut byte = [0u8; 1];
        self.read_special(CONFIG_WORD, &mut byte)?;
        Ok(byte[0])
    }

    fn read_special(&mut self, word: u16, buf: &mut [u8]) -> Result<(), I2C::Error> {
        self.bus.write_read(SPECIAL_HEADER | self.pins, &word.to_be_bytes(), buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::i2c::{ErrorKind, ErrorType, NoAcknowledgeSource, Operation};

    const UID: [u8; 16] = *b"0123456789abcdef";

    struct FakeBus {
        pins: u8,
        array: std::vec::Vec<u8>,
        secure: [u8; 32],
        config: u8,
        sent: std::vec::Vec<(u8, std::vec::Vec<u8>)>,
    }

    impl FakeBus {
        fn new(pins: u8) -> Self {
            Self {
                pins,
                array: (0..ARRAY_BYTES as usize).map(|i| i.to_le_bytes()[0] ^ 0xA5).collect(),
                secure: [0x5E; 32],
                config: 0x1D | (pins << 5),
                sent: std::vec::Vec::new(),
            }
        }
    }

    impl ErrorType for FakeBus {
        type Error = ErrorKind;
    }

    impl I2c for FakeBus {
        fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
            let nack = ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address);
            let special = address == (SPECIAL_HEADER | self.pins);
            if !special && address != (ARRAY_HEADER | self.pins) {
                return Err(nack);
            }
            let mut word = 0usize;
            for op in ops {
                match op {
                    Operation::Write(bytes) => {
                        self.sent.push((address, bytes.to_vec()));
                        word = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
                    }
                    Operation::Read(buf) => {
                        for b in buf.iter_mut() {
                            *b = if special {
                                match word & 0x0600 {
                                    0x0000 => self.secure[word & 0x1F],
                                    0x0200 => UID[word & 0x0F],
                                    0x0600 => self.config,
                                    _ => 0xFF,
                                }
                            } else {
                                self.array[word]
                            };
                            word += 1;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn array_reads_start_at_the_offset_sent_big_endian() {
        let mut e = Eeprom::new(FakeBus::new(0));
        let mut out = [0u8; 2];
        e.read_array(0x1234, &mut out).unwrap();
        assert_eq!(out, [0x34 ^ 0xA5, 0x35 ^ 0xA5]);
        assert_eq!(e.release().sent, [(0x50, vec![0x12, 0x34])]);
    }

    #[test]
    fn unique_id_is_sixteen_bytes_from_word_0x0200_on_the_special_header() {
        let mut e = Eeprom::new(FakeBus::new(0));
        assert_eq!(e.unique_id().unwrap(), UID);
        assert_eq!(e.release().sent, [(0x58, vec![0x02, 0x00])]);
    }

    #[test]
    fn secure_page_and_config_use_their_own_words() {
        let mut e = Eeprom::new(FakeBus::new(0));
        assert_eq!(e.secure_page().unwrap(), [0x5E; 32]);
        assert_eq!(e.config().unwrap(), 0x1D);
        let sent = e.release().sent;
        assert_eq!(sent, [(0x58, vec![0x00, 0x00]), (0x58, vec![0x06, 0x00])]);
    }

    #[test]
    fn address_bits_move_both_headers() {
        let mut e = Eeprom::with_address_bits(FakeBus::new(5), 5);
        assert!(e.present());
        assert_eq!(e.unique_id().unwrap(), UID);
        let sent = e.release().sent;
        assert_eq!(sent[0].0, 0x55);
        assert_eq!(sent[1].0, 0x5D);
    }

    #[test]
    fn module_info_comes_from_the_start_of_the_array() {
        let mut bus = FakeBus::new(0);
        bus.array[0..2].copy_from_slice(b"MI");
        bus.array[2] = 1;
        bus.array[3..100].fill(0);
        bus.array[4..9].copy_from_slice(b"LS012");
        let mut e = Eeprom::new(bus);
        let info = e.module_info().unwrap().unwrap();
        assert_eq!(info.name(), Some("LS012"));
        let sent = e.release().sent;
        assert_eq!(sent.len(), 4);
        assert_eq!(sent[0], (0x50, vec![0x00, 0x00]));
    }

    #[test]
    fn the_record_is_read_in_four_pieces_and_put_back_together() {
        let mut e = Eeprom::new(FakeBus::new(0));
        let record = e.record().unwrap();
        let want: std::vec::Vec<u8> = (0..RECORD_BYTES).map(|i| i.to_le_bytes()[0] ^ 0xA5).collect();
        assert_eq!(record[..], want[..]);
        assert_eq!(
            e.release().sent,
            [(0x50, vec![0, 0]), (0x50, vec![0, 25]), (0x50, vec![0, 50]), (0x50, vec![0, 75])]
        );
    }

    #[test]
    fn a_device_at_another_address_is_absent() {
        let mut e = Eeprom::new(FakeBus::new(3));
        assert!(!e.present());
        assert!(e.unique_id().is_err());
    }

    #[test]
    fn nothing_sent_is_more_than_an_address_pointer() {
        let mut e = Eeprom::new(FakeBus::new(0));
        let _ = e.unique_id();
        let _ = e.secure_page();
        let _ = e.config();
        let _ = e.read_array(0, &mut [0u8; 4]);
        let _ = e.record();
        assert!(e.release().sent.iter().all(|(_, bytes)| bytes.len() == 2));
    }
}
