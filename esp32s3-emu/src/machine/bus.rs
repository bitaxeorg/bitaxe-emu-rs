//! ESP32-S3 address map: internal ROM and SRAM, RTC memories, external flash
//! through the cache MMU, and the peripheral bus.

use std::collections::BTreeMap;

use crate::devices::cache::MMU_ENTRIES;
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
    /// The core executing: 0 for PRO_CPU, 1 for APP_CPU.
    pub core: usize,
    /// Unmodelled peripheral registers: address → (reads, writes).
    pub unmodelled: BTreeMap<u32, (u64, u64)>,
    /// When set, every peripheral access: (address, value, write, PC), oldest first. The cores
    /// then run one instruction at a time to know the PC.
    pub mmio_log: Option<std::collections::VecDeque<(u32, u32, bool, u32)>>,
    /// PC of the instruction running, while [`Soc::mmio_log`] is set.
    pub trace_pc: u32,
    /// Entries kept in [`Soc::mmio_log`].
    pub mmio_log_cap: usize,
    /// Instruction count at the start of the current slice, and its time.
    slice_icount: u64,
    slice_ticks: u64,
    /// Ticks per CPU cycle during the slice.
    cycle_ticks: u64,
    /// Instruction count at the last [`XtensaBus::sync`].
    icount: u64,
    /// Latest time a peripheral access used: the cores run each slice in
    /// turn, and the peripherals must not see time go back.
    time_floor: u64,
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
    Psram,
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
            core: 0,
            unmodelled: BTreeMap::new(),
            mmio_log: None,
            trace_pc: 0,
            mmio_log_cap: 4096,
            slice_icount: 0,
            slice_ticks: 0,
            cycle_ticks: 60,
            icount: 0,
            time_floor: 0,
            last_lines: 0,
            fetch_page: None,
        }
    }

    fn region(&self, r: Region) -> &[u8] {
        match r {
            Region::Rom => &self.rom,
            Region::Sram => &self.sram,
            Region::Flash => &self.periph.flash.data,
            Region::Psram => &self.periph.psram.data,
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
            0x3c | 0x3d => self.cache_map(page - DCACHE_BASE)?,
            0x42 | 0x43 => self.cache_map(page - ICACHE_BASE)?,
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

    /// The time of a peripheral access: the executing core's time, or a
    /// later one already seen by the peripherals.
    fn access_time(&mut self) -> u64 {
        let t = self.now().max(self.time_floor);
        self.time_floor = t;
        t
    }

    /// Latest time a peripheral access used.
    pub fn time_floor(&self) -> u64 {
        self.time_floor
    }

    /// Ends the slice at instruction count `icount`, returning its time.
    pub fn end_slice(&mut self, icount: u64) -> u64 {
        self.icount = icount;
        self.now()
    }

    /// The processor interrupt lines of the executing core.
    pub fn irq_lines(&mut self) -> u32 {
        let lines = self.periph.irq_lines(self.core);
        self.last_lines = lines;
        lines
    }

    // ---- memory ----

    /// The memory (flash or PSRAM) and offset an address of an external memory
    /// window maps to, if its MMU entry is valid.
    #[inline]
    fn cache_map(&self, offset: u32) -> Option<(Region, usize)> {
        let entry = self.periph.cache.mmu[(offset >> MMU_PAGE_BITS) as usize & 511];
        if entry & MMU_ENTRY_INVALID != 0 {
            return None;
        }
        let page = (entry & MMU_ENTRY_PAGE) as usize;
        let at = (page << MMU_PAGE_BITS) | (offset as usize & ((1 << MMU_PAGE_BITS) - 1));
        let region = if entry & MMU_ENTRY_PSRAM != 0 {
            Region::Psram
        } else {
            Region::Flash
        };
        Some((region, at))
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
                let (region, off) = self.cache_map(addr - DCACHE_BASE)?;
                (self.region(region), off)
            }
            0x42 | 0x43 => {
                let (region, off) = self.cache_map(addr - ICACHE_BASE)?;
                (self.region(region), off)
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
            // Through the data window, PSRAM pages are writable; flash pages are not.
            0x3c | 0x3d => match self.cache_map(addr - DCACHE_BASE)? {
                (Region::Psram, off) => (&mut self.periph.psram.data, off),
                _ => return None,
            },
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
        let now = self.access_time();
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
            let entry = (addr, value, write, self.trace_pc);
            if log.back() == Some(&entry) {
                return;
            }
            if log.len() >= self.mmio_log_cap {
                log.pop_front();
            }
            log.push_back(entry);
        }
    }

    fn mmio_write(&mut self, addr: u32, value: u32, mask: u32) {
        let now = self.access_time();
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
            let mut mem = DmaBus {
                sram: &mut self.sram,
                psram: &mut self.periph.psram.data,
                mmu: &self.periph.cache.mmu,
            };
            if self.periph.gdma.service(&mut mem) {
                self.periph.irq_dirty = true;
            }
        }
    }
}

/// Memory as the DMA engines see it: internal SRAM through its D-bus
/// addresses, and PSRAM through the pages the MMU maps in the data window.
struct DmaBus<'a> {
    sram: &'a mut [u8],
    psram: &'a mut [u8],
    mmu: &'a [u32; MMU_ENTRIES],
}

/// Where a DMA address lands: internal SRAM or PSRAM, and the offset.
enum DmaTarget {
    Sram(usize),
    Psram(usize),
}

impl DmaBus<'_> {
    fn target(&self, addr: u32, len: usize) -> Option<DmaTarget> {
        if let Some(off) = addr.checked_sub(SRAM_D_BASE)
            && (off as usize).checked_add(len)? <= self.sram.len()
        {
            return Some(DmaTarget::Sram(off as usize));
        }
        let offset = addr
            .checked_sub(DCACHE_BASE)
            .filter(|&o| o < CACHE_WINDOW)?;
        let entry = self.mmu[(offset >> MMU_PAGE_BITS) as usize & (MMU_ENTRIES - 1)];
        if entry & MMU_ENTRY_INVALID != 0 || entry & MMU_ENTRY_PSRAM == 0 {
            return None;
        }
        let page_off = offset as usize & ((1 << MMU_PAGE_BITS) - 1);
        // A transfer may cross into the next pages when they map the next PSRAM
        // pages.
        let pages = (page_off + len).div_ceil(1 << MMU_PAGE_BITS);
        let first = (offset >> MMU_PAGE_BITS) as usize;
        for i in 1..pages {
            let next = self.mmu.get(first + i)?;
            if *next != entry + i as u32 {
                return None;
            }
        }
        let at = (((entry & MMU_ENTRY_PAGE) as usize) << MMU_PAGE_BITS) | page_off;
        (at.checked_add(len)? <= self.psram.len()).then_some(DmaTarget::Psram(at))
    }
}

impl DmaMemory for DmaBus<'_> {
    fn dma_read(&self, addr: u32, len: usize) -> Option<&[u8]> {
        match self.target(addr, len)? {
            DmaTarget::Sram(off) => Some(&self.sram[off..off + len]),
            DmaTarget::Psram(off) => Some(&self.psram[off..off + len]),
        }
    }

    fn dma_write_u32(&mut self, addr: u32, value: u32) -> bool {
        let bytes = value.to_le_bytes();
        match self.target(addr, 4) {
            Some(DmaTarget::Sram(off)) => self.sram[off..off + 4].copy_from_slice(&bytes),
            Some(DmaTarget::Psram(off)) => self.psram[off..off + 4].copy_from_slice(&bytes),
            None => return false,
        }
        true
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
