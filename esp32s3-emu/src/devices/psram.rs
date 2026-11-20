//! Octal PSRAM on chip select 1 of the memory SPI: its mode registers and its
//! array, reached by SPI1 commands and, through the cache MMU, by the external
//! memory windows.

/// Commands of the octal interface (16 bits, DTR).
const REG_READ: u16 = 0x4040;
const REG_WRITE: u16 = 0xc0c0;
const SYNC_READ: u16 = 0x0000;
const SYNC_WRITE: u16 = 0x8080;
const LINEAR_READ: u16 = 0x2020;
const LINEAR_WRITE: u16 = 0xa0a0;

/// Vendor id in MR1.
const VENDOR: u8 = 0x0d;

pub struct Psram {
    pub data: Vec<u8>,
    /// Mode registers MR0..MR8.
    mr: [u8; 9],
}

impl Psram {
    /// `size` bytes; 0 for no PSRAM (the bus then reads all zeros).
    pub fn new(size: usize) -> Self {
        // MR2 density code by size.
        let density = match size >> 20 {
            4 => 1,
            8 => 3,
            16 => 5,
            32 => 7,
            64 => 6,
            _ => 0,
        };
        let mut p = Psram {
            data: vec![0; size],
            mr: [0; 9],
        };
        if size > 0 {
            p.mr = [
                // MR0: half drive strength, read latency code 2.
                0x09,
                VENDOR,
                // MR2: density, generation 3, good die.
                0x80 | 0x10 | density,
                // MR3: fast self-refresh.
                0x20,
                // MR4: write latency code.
                0x20,
                0,
                0,
                0,
                // MR8: 32-byte hybrid burst.
                0x05,
            ];
        }
        p
    }

    pub fn present(&self) -> bool {
        !self.data.is_empty()
    }

    fn byte_index(&self, addr: u32, i: usize) -> usize {
        (addr as usize + i) % self.data.len()
    }

    /// One transaction: the command, its address, the bytes sent and the
    /// number of bytes read back.
    pub fn exec(&mut self, cmd: u16, addr: u32, mosi: &[u8], miso_len: usize) -> Vec<u8> {
        if !self.present() {
            return vec![0; miso_len];
        }
        match cmd {
            REG_READ => (0..miso_len)
                .map(|i| self.mr.get(addr as usize + i).copied().unwrap_or(0))
                .collect(),
            REG_WRITE => {
                if let (Some(&v), Some(r)) = (mosi.first(), self.mr.get_mut(addr as usize)) {
                    // MR1 and MR2 are read-only.
                    if !matches!(addr, 1 | 2) {
                        *r = v;
                    }
                }
                Vec::new()
            }
            SYNC_WRITE | LINEAR_WRITE => {
                for (i, &b) in mosi.iter().enumerate() {
                    let at = self.byte_index(addr, i);
                    self.data[at] = b;
                }
                Vec::new()
            }
            SYNC_READ | LINEAR_READ => (0..miso_len)
                .map(|i| self.data[self.byte_index(addr, i)])
                .collect(),
            _ => vec![0; miso_len],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_registers_and_array() {
        let mut p = Psram::new(8 << 20);
        assert_eq!(p.exec(REG_READ, 0, &[], 2), vec![0x09, VENDOR]);
        assert_eq!(p.exec(REG_READ, 2, &[], 2)[0] & 0x07, 3);
        p.exec(REG_WRITE, 8, &[0x04], 0);
        assert_eq!(p.exec(REG_READ, 8, &[], 1), vec![0x04]);
        p.exec(SYNC_WRITE, 0x100, &[1, 2, 3, 4], 0);
        assert_eq!(p.exec(SYNC_READ, 0x102, &[], 2), vec![3, 4]);
        let mut none = Psram::new(0);
        assert_eq!(none.exec(REG_READ, 0, &[], 2), vec![0, 0]);
    }
}
