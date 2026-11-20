//! ESP32-S3 address map: internal ROM and SRAM, RTC memories, external flash
//! through the cache MMU, and the peripheral bus.

use std::collections::BTreeMap;

use crate::devices::gdma::DmaMemory;
use crate::devices::{GDMA_BASE, Peripherals};
use rax::isa::xtensa::XtensaBus;

/// Internal ROM, I-bus view.
pub const IROM_BASE: u32 = 0x4000_0000;
pub const ROM_SIZE: usize = 0x6_0000;
/// Internal ROM, D-bus view of its upper part.
pub const DROM_BASE: u32 = 0x3ff0_0000;
pub const DROM_OFFSET: usize = 0x4_0000;
pub const DROM_SIZE: usize = 0x2_0000;
/// Internal SRAM (SRAM0..2, 512 KB): the I-bus sees it at 0x4037_0000, the D-bus
/// at 0x3fc8_0000 (SRAM0 is I-bus only, SRAM2 D-bus only, SRAM1 on both).
pub const SRAM_SIZE: usize = 0x8_0000;
pub const SRAM_I_BASE: u32 = 0x4037_0000;
pub const SRAM_D_BASE: u32 = 0x3fc8_0000;
pub const RTC_SLOW_BASE: u32 = 0x5000_0000;
pub const RTC_FAST_BASE: u32 = 0x600f_e000;
pub const RTC_MEM_SIZE: usize = 0x2000;
/// External memory windows (32 MB each), mapped by the shared MMU table.
pub const DCACHE_BASE: u32 = 0x3c00_0000;
pub const ICACHE_BASE: u32 = 0x4200_0000;
pub const CACHE_WINDOW: u32 = 0x0200_0000;
pub const PERIPH_BASE: u32 = 0x6000_0000;
pub const PERIPH_END: u32 = 0x600d_1000;

/// MMU entry fields (64 KB pages).
pub const MMU_PAGE_BITS: u32 = 16;
pub const MMU_ENTRY_INVALID: u32 = 1 << 14;
pub const MMU_ENTRY_PSRAM: u32 = 1 << 15;
pub const MMU_ENTRY_PAGE: u32 = 0x3fff;

/// The SoC as seen from the processor.
pub struct Soc {
    pub rom: Vec<u8>,
    pub sram: Vec<u8>,
    pub rtc_slow: Vec<u8>,
    pub rtc_fast: Vec<u8>,
    pub periph: Peripherals,
    /// Unmodelled peripheral registers: address → (reads, writes).
    pub unmodelled: BTreeMap<u32, (u64, u64)>,
    /// When set, every peripheral access: (address, value, write), oldest first.
    pub mmio_log: Option<std::collections::VecDeque<(u32, u32, bool)>>,
    /// Entries kept in [`Soc::mmio_log`].
    pub mmio_log_cap: usize,
    /// Instruction count at the start of the current slice, and its time.
    slice_icount: u64,
    slice_ticks: u64,
    /// Ticks per CPU cycle during the slice.
    cycle_ticks: u64,
    /// Instruction count at the last [`XtensaBus::sync`].
    icount: u64,
    last_lines: u32,
    /// The last 4 KB page fetched from: its address, region and offset of the
    /// page in the region.
    fetch_page: Option<(u32, Region, usize)>,
}

/// Memories instructions are fetched from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Rom,
    Sram,
    Flash,
    RtcSlow,
    RtcFast,
}

impl Soc {
    pub fn new(rom: Vec<u8>, periph: Peripherals) -> Self {
        let mut rom = rom;
        rom.resize(ROM_SIZE, 0);
        Soc {
            rom,
            sram: vec![0; SRAM_SIZE],
            rtc_slow: vec![0; RTC_MEM_SIZE],
            rtc_fast: vec![0; RTC_MEM_SIZE],
            periph,
            unmodelled: BTreeMap::new(),
            mmio_log: None,
            mmio_log_cap: 4096,
            slice_icount: 0,
            slice_ticks: 0,
            cycle_ticks: 60,
            icount: 0,
            last_lines: 0,
            fetch_page: None,
        }
    }

    fn region(&self, r: Region) -> &[u8] {
        match r {
            Region::Rom => &self.rom,
            Region::Sram => &self.sram,
            Region::Flash => &self.periph.flash.data,
            Region::RtcSlow => &self.rtc_slow,
            Region::RtcFast => &self.rtc_fast,
        }
    }

    /// Region and offset of the 4 KB page at `page` (an address), if it is memory.
    fn locate_page(&self, page: u32) -> Option<(Region, usize)> {
        let r = match page >> 24 {
            0x3f if (DROM_BASE..DROM_BASE + DROM_SIZE as u32).contains(&page) => {
                (Region::Rom, DROM_OFFSET + (page - DROM_BASE) as usize)
            }
            0x3f if (SRAM_D_BASE..SRAM_D_BASE + SRAM_SIZE as u32).contains(&page) => {
                (Region::Sram, (page - SRAM_D_BASE) as usize)
            }
            0x40 if page >= SRAM_I_BASE => (Region::Sram, (page - SRAM_I_BASE) as usize),
            0x40 => (Region::Rom, (page - IROM_BASE) as usize),
            0x3c | 0x3d => (Region::Flash, self.cache_to_flash(page - DCACHE_BASE)?),
            0x42 | 0x43 => (Region::Flash, self.cache_to_flash(page - ICACHE_BASE)?),
            0x50 => (Region::RtcSlow, (page - RTC_SLOW_BASE) as usize),
            0x60 if page >= RTC_FAST_BASE => (Region::RtcFast, (page - RTC_FAST_BASE) as usize),
            _ => return None,
        };
        (r.1 + 0x1000 <= self.region(r.0).len()).then_some(r)
    }

    /// Forgets the fetch page (after the MMU changed).
    pub fn invalidate_fetch(&mut self) {
        self.fetch_page = None;
    }

    /// Starts an execution slice at `ticks`, the processor having executed `icount`
    /// instructions.
    pub fn begin_slice(&mut self, ticks: u64, icount: u64) {
        self.slice_ticks = ticks;
        self.slice_icount = icount;
        self.icount = icount;
        self.cycle_ticks = self.periph.cycle_ticks();
    }

    /// The current time, in ticks.
    #[inline]
    pub fn now(&self) -> u64 {
        self.slice_ticks + (self.icount - self.slice_icount) * self.cycle_ticks
    }

    /// Ends the slice at instruction count `icount`, returning its time.
    pub fn end_slice(&mut self, icount: u64) -> u64 {
        self.icount = icount;
        self.now()
    }

    /// The processor interrupt lines of PRO_CPU.
    pub fn irq_lines(&mut self) -> u32 {
        let now = self.now();
        let lines = self.periph.irq_lines(now);
        self.last_lines = lines;
        lines
    }

    // ---- memory ----

    /// Flash offset of an address in an external memory window, if mapped to flash.
    #[inline]
    pub fn cache_to_flash(&self, offset: u32) -> Option<usize> {
        let entry = self.periph.cache.mmu[(offset >> MMU_PAGE_BITS) as usize & 511];
        if entry & (MMU_ENTRY_INVALID | MMU_ENTRY_PSRAM) != 0 {
            return None;
        }
        let page = (entry & MMU_ENTRY_PAGE) as usize;
        Some((page << MMU_PAGE_BITS) | (offset as usize & ((1 << MMU_PAGE_BITS) - 1)))
    }

    /// Memory backing `addr` for `len` bytes, if it is RAM or ROM (not MMIO).
    #[inline]
    fn mem(&self, addr: u32, len: usize) -> Option<&[u8]> {
        let (buf, off): (&[u8], usize) = match addr >> 24 {
            0x3f => {
                if (DROM_BASE..DROM_BASE + DROM_SIZE as u32).contains(&addr) {
                    (&self.rom, DROM_OFFSET + (addr - DROM_BASE) as usize)
                } else if (SRAM_D_BASE..SRAM_D_BASE + SRAM_SIZE as u32).contains(&addr) {
                    (&self.sram, (addr - SRAM_D_BASE) as usize)
                } else {
                    return None;
                }
            }
            0x40 => {
                if addr >= SRAM_I_BASE {
                    (&self.sram, (addr - SRAM_I_BASE) as usize)
                } else {
                    (&self.rom, (addr - IROM_BASE) as usize)
                }
            }
            0x3c | 0x3d => {
                let off = self.cache_to_flash(addr - DCACHE_BASE)?;
                (&self.periph.flash.data, off)
            }
            0x42 | 0x43 => {
                let off = self.cache_to_flash(addr - ICACHE_BASE)?;
                (&self.periph.flash.data, off)
            }
            0x50 => (&self.rtc_slow, (addr - RTC_SLOW_BASE) as usize),
            0x60 if addr >= RTC_FAST_BASE => (&self.rtc_fast, (addr - RTC_FAST_BASE) as usize),
            _ => return None,
        };
        buf.get(off..off.checked_add(len)?)
    }

    #[inline]
    fn mem_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        let (buf, off): (&mut [u8], usize) = match addr >> 24 {
            0x3f if (SRAM_D_BASE..SRAM_D_BASE + SRAM_SIZE as u32).contains(&addr) => {
                (&mut self.sram, (addr - SRAM_D_BASE) as usize)
            }
            0x40 if addr >= SRAM_I_BASE => (&mut self.sram, (addr - SRAM_I_BASE) as usize),
            0x50 => (&mut self.rtc_slow, (addr - RTC_SLOW_BASE) as usize),
            0x60 if addr >= RTC_FAST_BASE => (&mut self.rtc_fast, (addr - RTC_FAST_BASE) as usize),
            _ => return None,
        };
        buf.get_mut(off..off.checked_add(len)?)
    }

    fn is_periph(addr: u32) -> bool {
        (PERIPH_BASE..PERIPH_END).contains(&addr)
    }

    /// Reads `len` bytes of guest memory (RAM, ROM or mapped flash) for the host.
    pub fn read_bytes(&self, addr: u32, len: usize) -> Option<Vec<u8>> {
        self.mem(addr, len).map(|b| b.to_vec())
    }

    /// Writes guest RAM for the host.
    pub fn write_bytes(&mut self, addr: u32, bytes: &[u8]) -> bool {
        match self.mem_mut(addr, bytes.len()) {
            Some(b) => {
                b.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }

    pub fn read_u32(&self, addr: u32) -> Option<u32> {
        self.mem(addr, 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn write_u32(&mut self, addr: u32, v: u32) -> bool {
        self.write_bytes(addr, &v.to_le_bytes())
    }

    fn mmio_read(&mut self, addr: u32) -> u32 {
        let now = self.now();
        let v = match self.periph.read(addr & !3, now) {
            Some(v) => v,
            None => {
                self.unmodelled.entry(addr & !3).or_default().0 += 1;
                0
            }
        };
        self.log_mmio(addr & !3, v, false);
        v
    }

    fn log_mmio(&mut self, addr: u32, value: u32, write: bool) {
        if let Some(log) = self.mmio_log.as_mut() {
            // Polling loops repeat the same access: keep one.
            if log.back() == Some(&(addr, value, write)) {
                return;
            }
            if log.len() >= self.mmio_log_cap {
                log.pop_front();
            }
            log.push_back((addr, value, write));
        }
    }

    fn mmio_write(&mut self, addr: u32, value: u32, mask: u32) {
        let now = self.now();
        let addr = addr & !3;
        let value = if mask == u32::MAX {
            value
        } else {
            let old = self.periph.read(addr, now).unwrap_or(0);
            (old & !mask) | (value & mask)
        };
        self.log_mmio(addr, value, true);
        if (0x600c_5000..0x600c_5800).contains(&addr) {
            self.fetch_page = None;
        }
        if !self.periph.write(addr, value, now) {
            self.unmodelled.entry(addr).or_default().1 += 1;
        }
        if (GDMA_BASE..GDMA_BASE + 0x1000).contains(&addr) && self.periph.gdma.needs_service() {
            let mut sram = DmaSram(&mut self.sram);
            if self.periph.gdma.service(&mut sram) {
                self.periph.irq_dirty = true;
            }
        }
    }
}

/// Internal SRAM as the DMA engines see it: through its D-bus addresses only.
struct DmaSram<'a>(&'a mut [u8]);

impl DmaSram<'_> {
    fn offset(&self, addr: u32, len: usize) -> Option<usize> {
        let off = addr.checked_sub(SRAM_D_BASE)? as usize;
        (off.checked_add(len)? <= self.0.len()).then_some(off)
    }
}

impl DmaMemory for DmaSram<'_> {
    fn dma_read(&self, addr: u32, len: usize) -> Option<&[u8]> {
        let off = self.offset(addr, len)?;
        Some(&self.0[off..off + len])
    }

    fn dma_write_u32(&mut self, addr: u32, value: u32) -> bool {
        match self.offset(addr, 4) {
            Some(off) => {
                self.0[off..off + 4].copy_from_slice(&value.to_le_bytes());
                true
            }
            None => false,
        }
    }
}

impl Soc {
    /// Fetch across a page end or region end.
    fn fetch_slow(&mut self, addr: u32) -> Result<u32, ()> {
        if let Some(b) = self.mem(addr, 4) {
            return Ok(u32::from_le_bytes(b.try_into().unwrap()));
        }
        let mut word = [0u8; 4];
        let first = self.mem(addr, 1).ok_or(())?;
        word[0] = first[0];
        for (i, b) in word.iter_mut().enumerate().skip(1) {
            if let Some(v) = self.mem(addr.wrapping_add(i as u32), 1) {
                *b = v[0];
            }
        }
        Ok(u32::from_le_bytes(word))
    }
}

impl XtensaBus for Soc {
    #[inline]
    fn fetch(&mut self, addr: u32) -> Result<u32, ()> {
        let page = addr & !0xfff;
        let off = (addr & 0xfff) as usize;
        if off <= 0xffc {
            let (region, base) = match self.fetch_page {
                Some((p, r, b)) if p == page => (r, b),
                _ => match self.locate_page(page) {
                    Some((r, b)) => {
                        self.fetch_page = Some((page, r, b));
                        (r, b)
                    }
                    None => return self.fetch_slow(addr),
                },
            };
            let b = &self.region(region)[base + off..base + off + 4];
            return Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        }
        self.fetch_slow(addr)
    }

    #[inline]
    fn read8(&mut self, addr: u32) -> Result<u8, ()> {
        if let Some(b) = self.mem(addr, 1) {
            return Ok(b[0]);
        }
        if Self::is_periph(addr) {
            return Ok((self.mmio_read(addr) >> ((addr & 3) * 8)) as u8);
        }
        Err(())
    }

    #[inline]
    fn read16(&mut self, addr: u32) -> Result<u16, ()> {
        if let Some(b) = self.mem(addr, 2) {
            return Ok(u16::from_le_bytes([b[0], b[1]]));
        }
        if Self::is_periph(addr) {
            return Ok((self.mmio_read(addr) >> ((addr & 2) * 8)) as u16);
        }
        Err(())
    }

    #[inline]
    fn read32(&mut self, addr: u32) -> Result<u32, ()> {
        if let Some(b) = self.mem(addr, 4) {
            return Ok(u32::from_le_bytes(b.try_into().unwrap()));
        }
        if Self::is_periph(addr) {
            return Ok(self.mmio_read(addr));
        }
        Err(())
    }

    #[inline]
    fn write8(&mut self, addr: u32, value: u8) -> Result<(), ()> {
        if let Some(b) = self.mem_mut(addr, 1) {
            b[0] = value;
            return Ok(());
        }
        if Self::is_periph(addr) {
            let sh = (addr & 3) * 8;
            self.mmio_write(addr, u32::from(value) << sh, 0xff << sh);
            return Ok(());
        }
        Err(())
    }

    #[inline]
    fn write16(&mut self, addr: u32, value: u16) -> Result<(), ()> {
        if let Some(b) = self.mem_mut(addr, 2) {
            b.copy_from_slice(&value.to_le_bytes());
            return Ok(());
        }
        if Self::is_periph(addr) {
            let sh = (addr & 2) * 8;
            self.mmio_write(addr, u32::from(value) << sh, 0xffff << sh);
            return Ok(());
        }
        Err(())
    }

    #[inline]
    fn write32(&mut self, addr: u32, value: u32) -> Result<(), ()> {
        if let Some(b) = self.mem_mut(addr, 4) {
            b.copy_from_slice(&value.to_le_bytes());
            return Ok(());
        }
        if Self::is_periph(addr) {
            self.mmio_write(addr, value, u32::MAX);
            return Ok(());
        }
        Err(())
    }

    #[inline]
    fn sync(&mut self, icount: u64) -> Option<u32> {
        self.icount = icount;
        if self.periph.irq_dirty {
            self.periph.irq_dirty = false;
            let lines = self.irq_lines();
            return Some(lines);
        }
        None
    }
}
