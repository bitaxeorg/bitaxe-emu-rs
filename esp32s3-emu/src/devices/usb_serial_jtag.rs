//! USB-Serial-JTAG (0x6003_8000), serial side: bytes written to EP1 reach the
//! host (as if a terminal were connected), bytes from the host can be read.
//! The host sends a start-of-frame packet every millisecond, so SOF_INT_RAW is
//! set again as soon as it is cleared.

use std::collections::VecDeque;

const EP1: u32 = 0x00;
const EP1_CONF: u32 = 0x04;
const INT_RAW: u32 = 0x08;
const INT_ST: u32 = 0x0c;
const INT_ENA: u32 = 0x10;
const INT_CLR: u32 = 0x14;
const DATE: u32 = 0x80;

const CONF_IN_EP_DATA_FREE: u32 = 1 << 1;
const CONF_OUT_EP_DATA_AVAIL: u32 = 1 << 2;
const INT_SOF: u32 = 1 << 1;
const INT_SERIAL_OUT_RECV_PKT: u32 = 1 << 2;
const INT_SERIAL_IN_EMPTY: u32 = 1 << 3;

#[derive(Default)]
pub struct UsbSerialJtag {
    regs: Vec<u32>,
    pending: Vec<u8>,
    /// Bytes the chip sent, for the host.
    pub tx_out: VecDeque<u8>,
    rx: VecDeque<u8>,
    int_ena: u32,
    int_raw: u32,
}

impl UsbSerialJtag {
    pub fn reset(&mut self) {
        self.regs = vec![0; 0x21];
        self.pending.clear();
        self.int_ena = 0;
        self.int_raw = INT_SERIAL_IN_EMPTY | INT_SOF;
    }

    /// The host sends bytes to the chip.
    pub fn receive(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes);
        self.int_raw |= INT_SERIAL_OUT_RECV_PKT;
    }

    pub fn irq(&self) -> bool {
        self.int_raw & self.int_ena != 0
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            EP1 => self.rx.pop_front().map_or(0, u32::from),
            EP1_CONF => {
                CONF_IN_EP_DATA_FREE
                    | if self.rx.is_empty() {
                        0
                    } else {
                        CONF_OUT_EP_DATA_AVAIL
                    }
            }
            INT_RAW => self.int_raw,
            INT_ST => self.int_raw & self.int_ena,
            INT_ENA => self.int_ena,
            DATE => 0x2010_1100,
            o if o <= 0x80 => self.regs[(o / 4) as usize],
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) -> bool {
        match off {
            EP1 => {
                self.pending.push(v as u8);
                if self.pending.len() >= 64 {
                    self.tx_out.extend(self.pending.drain(..));
                }
            }
            EP1_CONF => {
                if v & 1 != 0 {
                    self.tx_out.extend(self.pending.drain(..));
                    self.int_raw |= INT_SERIAL_IN_EMPTY;
                }
            }
            INT_ENA => self.int_ena = v,
            INT_CLR => {
                self.int_raw &= !v;
                self.int_raw |= INT_SERIAL_IN_EMPTY | INT_SOF;
                if !self.rx.is_empty() {
                    self.int_raw |= INT_SERIAL_OUT_RECV_PKT;
                }
            }
            INT_RAW | INT_ST | DATE => return false,
            o if o <= 0x80 => self.regs[(o / 4) as usize] = v,
            _ => return false,
        }
        true
    }
}
