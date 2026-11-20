//! A chain of BM13xx chips on the ASIC UART: command and job frames from the
//! controller, register and nonce responses back.
//!
//! The chips answer CHIP_ID enumeration, address assignment and register reads
//! and writes, and accept jobs. Nonces are searched on the host at the
//! difficulty of TICKET_MASK within a small hash budget per call of
//! [`Chain::update`], which is far below what a real chip hashes: at the usual
//! ticket difficulties a nonce is rare. Jobs made of midstates (BM1397) are
//! counted but not hashed.
//!
//! While a chip has work, its hash counters (one per domain and their total)
//! advance at the rate of its PLL frequency and small cores, one count per 2^32
//! hashes, as a healthy chip's do.

use std::collections::{BTreeMap, VecDeque};

use sha2::{Digest, Sha256};

const CHIP_ID: u8 = 0x00;
const PLL0_PARAMETER: u8 = 0x08;
const TICKET_MASK: u8 = 0x14;
/// Hash counter of each domain, in domain order.
const DOMAIN_COUNTERS: [u8; 8] = [0x88, 0x89, 0x8a, 0x8b, 0xac, 0xad, 0xae, 0xaf];
/// Sum of the domain counters.
const TOTAL_COUNTER: u8 = 0x8c;
const VERSION_ROLLING: u8 = 0xa4;
/// Hashes per count of the hash counters.
const HASHES_PER_COUNT: f64 = 4_294_967_296.0;

const CMD_JOB: u8 = 0x21;
const CMD_SET_ADDR: u8 = 0x00;
const CMD_WRITE_REG: u8 = 0x01;
const CMD_READ_REG: u8 = 0x02;
const CMD_INACTIVE: u8 = 0x03;
const BROADCAST: u8 = 0x10;

/// The chip model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asic {
    Bm1397,
    Bm1366,
    Bm1368,
    Bm1370,
    Bm1373,
}

/// Operating point a chip's power is given at.
#[derive(Clone, Copy, Debug)]
pub struct PowerPoint {
    /// Core voltage of one chip, V.
    pub vcore: f32,
    /// Hash frequency, MHz.
    pub freq_mhz: f32,
    /// Power while hashing, W.
    pub hashing_w: f32,
    /// Power while powered without work, W.
    pub idle_w: f32,
}

impl Asic {
    pub fn name(self) -> &'static str {
        match self {
            Asic::Bm1397 => "BM1397",
            Asic::Bm1366 => "BM1366",
            Asic::Bm1368 => "BM1368",
            Asic::Bm1370 => "BM1370",
            Asic::Bm1373 => "BM1373",
        }
    }

    pub fn chip_id(self) -> u16 {
        match self {
            Asic::Bm1397 => 0x1397,
            Asic::Bm1366 => 0x1366,
            Asic::Bm1368 => 0x1368,
            Asic::Bm1370 => 0x1370,
            Asic::Bm1373 => 0x1373,
        }
    }

    /// The CORE_NUM byte of the chip identification register.
    fn core_num(self) -> u8 {
        match self {
            Asic::Bm1397 => 0x18,
            _ => 0x00,
        }
    }

    /// Jobs carry midstates instead of the block header.
    fn midstate_jobs(self) -> bool {
        self == Asic::Bm1397
    }

    /// Hashes per clock cycle.
    fn small_cores(self) -> f64 {
        match self {
            Asic::Bm1397 => 672.0,
            Asic::Bm1366 => 894.0,
            Asic::Bm1368 => 1276.0,
            Asic::Bm1370 => 2040.0,
            Asic::Bm1373 => 6900.0,
        }
    }

    /// Hash domains, each with its counter (the BM1397 has none).
    fn domains(self) -> usize {
        match self {
            Asic::Bm1397 => 0,
            Asic::Bm1373 => 8,
            _ => 4,
        }
    }

    pub fn power_point(self) -> PowerPoint {
        let (vcore, freq_mhz, hashing_w, idle_w) = match self {
            Asic::Bm1397 => (1.4, 425.0, 14.0, 1.5),
            Asic::Bm1366 => (1.2, 485.0, 11.0, 1.2),
            Asic::Bm1368 => (1.166, 490.0, 14.0, 1.3),
            Asic::Bm1370 => (1.15, 525.0, 18.0, 1.5),
            Asic::Bm1373 => (1.0, 327.0, 24.0, 2.0),
        };
        PowerPoint {
            vcore,
            freq_mhz,
            hashing_w,
            idle_w,
        }
    }
}

/// CRC-5 of the chips (polynomial 0x05, initial value 0x1f, MSB first).
pub fn crc5(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x1f;
    for &byte in data {
        for bit in (0..8).rev() {
            let inp = (byte >> bit) & 1;
            let top = (crc >> 4) & 1;
            crc = (crc << 1) & 0x1f;
            if inp ^ top != 0 {
                crc ^= 0x05;
            }
        }
    }
    crc
}

/// CRC-16/CCITT-FALSE of the job frames.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Sets the CRC bits of the last byte of a response so that the CRC of the
/// frame after the preamble is zero.
fn seal(frame: &mut [u8]) {
    let last = frame.len() - 1;
    let flags = frame[last] & 0xe0;
    for c in 0..0x20 {
        frame[last] = flags | c;
        if crc5(&frame[2..]) == 0 {
            return;
        }
    }
}

struct Job {
    header: [u8; 80],
    next_nonce: u64,
}

struct Chip {
    addr: u8,
    /// The chip took an address since the chain went inactive.
    addressed: bool,
    regs: BTreeMap<u8, u32>,
    /// Fraction of a count of each domain counter not added yet.
    count_frac: f64,
}

impl Chip {
    /// Hash frequency in MHz (25 MHz reference and PLL0).
    fn frequency_mhz(&self) -> f64 {
        let v = self.regs.get(&PLL0_PARAMETER).copied().unwrap_or(0);
        let fbdiv = f64::from((v >> 16) & 0xfff);
        let refdiv = f64::from((v >> 8) & 0x3f).max(1.0);
        let post1 = f64::from(((v >> 4) & 7) + 1);
        let post2 = f64::from((v & 7) + 1);
        25.0 * fbdiv / (refdiv * post1 * post2)
    }
}

pub struct Chain {
    pub asic: Asic,
    chips: Vec<Chip>,
    rx: Vec<u8>,
    /// Bytes for the controller.
    pub tx: VecDeque<u8>,
    jobs: BTreeMap<u8, Job>,
    current_job: Option<u8>,
    /// Chips held in reset (NRST low, or no core power).
    pub in_reset: bool,
    /// Hashes tried per [`Chain::update`].
    pub hash_budget: u64,
    pub hashes: u64,
    pub nonces_found: u64,
    pub jobs_received: u64,
    /// Time of the last [`Chain::update`], s.
    last_update: Option<f64>,
}

impl Chain {
    pub fn new(asic: Asic, count: usize) -> Self {
        let mut chain = Chain {
            asic,
            chips: Vec::new(),
            rx: Vec::new(),
            tx: VecDeque::new(),
            jobs: BTreeMap::new(),
            current_job: None,
            in_reset: true,
            hash_budget: 2000,
            hashes: 0,
            nonces_found: 0,
            jobs_received: 0,
            last_update: None,
        };
        chain.chips = (0..count).map(|_| chain.reset_chip()).collect();
        chain
    }

    fn reset_chip(&self) -> Chip {
        let mut regs = BTreeMap::new();
        regs.insert(
            CHIP_ID,
            (u32::from(self.asic.chip_id()) << 16) | (u32::from(self.asic.core_num()) << 8),
        );
        regs.insert(PLL0_PARAMETER, 0xc054_0165);
        regs.insert(TICKET_MASK, 0);
        Chip {
            addr: 0,
            addressed: false,
            regs,
            count_frac: 0.0,
        }
    }

    /// Holds the chips in reset or releases them.
    pub fn set_reset(&mut self, reset: bool) {
        if reset && !self.in_reset {
            let fresh: Vec<Chip> = (0..self.chips.len()).map(|_| self.reset_chip()).collect();
            self.chips = fresh;
            self.jobs.clear();
            self.current_job = None;
            self.rx.clear();
        }
        self.in_reset = reset;
    }

    /// Hash frequency of the first chip, in MHz (25 MHz reference and PLL0).
    pub fn frequency_mhz(&self) -> f32 {
        self.chips.first().map_or(0.0, |c| c.frequency_mhz() as f32)
    }

    /// Responses carry the version bits (11 bytes) once version rolling is on.
    fn long_frames(&self) -> bool {
        self.chips
            .first()
            .is_some_and(|c| c.regs.get(&VERSION_ROLLING).copied().unwrap_or(0) & (1 << 31) != 0)
    }

    /// The chips have work.
    pub fn hashing(&self) -> bool {
        !self.in_reset && self.current_job.is_some()
    }

    /// Bytes from the controller.
    pub fn receive(&mut self, bytes: &[u8]) {
        if self.in_reset {
            return;
        }
        self.rx.extend_from_slice(bytes);
        self.parse();
    }

    fn parse(&mut self) {
        loop {
            let Some(start) = self.rx.windows(2).position(|w| w == [0x55, 0xaa]) else {
                let keep = usize::from(self.rx.last() == Some(&0x55));
                self.rx.drain(..self.rx.len() - keep);
                return;
            };
            self.rx.drain(..start);
            if self.rx.len() < 4 {
                return;
            }
            let header = self.rx[2];
            let len = usize::from(self.rx[3]);
            // A header job is 88 bytes. Its length byte counts the bytes after the
            // preamble (86), or leaves out the two hashes as well (54).
            let total = match header {
                CMD_JOB if self.asic.midstate_jobs() => len + 2,
                CMD_JOB if len >= 86 => len + 2,
                CMD_JOB => len + 34,
                _ => len + 2,
            };
            if total < 7 {
                self.rx.drain(..2);
                continue;
            }
            if self.rx.len() < total {
                return;
            }
            let frame: Vec<u8> = self.rx.drain(..total).collect();
            self.frame(&frame);
        }
    }

    fn frame(&mut self, f: &[u8]) {
        let header = f[2];
        if header == CMD_JOB {
            let crc_ok =
                crc16(&f[2..f.len() - 2]) == u16::from_be_bytes([f[f.len() - 2], f[f.len() - 1]]);
            if crc_ok && self.asic.midstate_jobs() {
                self.jobs_received += 1;
            } else if crc_ok && f.len() == 88 {
                self.job(f);
            }
            return;
        }
        // A command ends with the CRC-5 of the bytes after the preamble.
        if crc5(&f[2..f.len() - 1]) != f[f.len() - 1] & 0x1f {
            return;
        }
        let broadcast = header & BROADCAST != 0;
        let addr = f[4];
        let reg = f[5];
        match header & 0x0f {
            CMD_INACTIVE => {
                for chip in &mut self.chips {
                    chip.addressed = false;
                }
            }
            CMD_SET_ADDR => {
                // The first chip of the chain without an address takes it.
                if let Some(chip) = self.chips.iter_mut().find(|c| !c.addressed) {
                    chip.addr = addr;
                    chip.addressed = true;
                    let id = chip.regs.get(&CHIP_ID).copied().unwrap_or(0);
                    chip.regs.insert(CHIP_ID, (id & !0xff) | u32::from(addr));
                }
            }
            CMD_WRITE_REG if f.len() >= 11 => {
                let value = u32::from_be_bytes([f[6], f[7], f[8], f[9]]);
                for chip in self
                    .chips
                    .iter_mut()
                    .filter(|c| broadcast || c.addr == addr)
                {
                    if reg != CHIP_ID {
                        chip.regs.insert(reg, value);
                    }
                }
            }
            CMD_READ_REG => {
                let long = self.long_frames();
                let replies: Vec<(u8, u32)> = self
                    .chips
                    .iter()
                    .filter(|c| broadcast || c.addr == addr)
                    .map(|c| (c.addr, c.regs.get(&reg).copied().unwrap_or(0)))
                    .collect();
                for (chip_addr, value) in replies {
                    let mut r = vec![0xaa, 0x55, 0, 0, 0, 0, chip_addr, reg, 0];
                    if long {
                        r.extend([0, 0]);
                    }
                    r[2..6].copy_from_slice(&value.to_be_bytes());
                    seal(&mut r);
                    self.tx.extend(r);
                }
            }
            _ => {}
        }
    }

    fn job(&mut self, f: &[u8]) {
        let job_id = f[4] >> 3;
        let nbits = &f[10..14];
        let ntime = &f[14..18];
        let mut header = [0u8; 80];
        header[0..4].copy_from_slice(&f[82..86]);
        // The frame holds the hashes as 32-bit words reversed.
        for (i, chunk) in f[50..82].chunks(4).enumerate() {
            for (j, b) in chunk.iter().rev().enumerate() {
                header[4 + i * 4 + j] = *b;
            }
        }
        for (i, chunk) in f[18..50].chunks(4).enumerate() {
            for (j, b) in chunk.iter().rev().enumerate() {
                header[36 + i * 4 + j] = *b;
            }
        }
        header[68..72].copy_from_slice(ntime);
        header[72..76].copy_from_slice(nbits);
        self.jobs.insert(
            job_id,
            Job {
                header,
                next_nonce: 0,
            },
        );
        self.current_job = Some(job_id);
        self.jobs_received += 1;
    }

    /// Advances the hash counters to `now` (seconds) and searches nonces of the
    /// current job within the hash budget.
    pub fn update(&mut self, now: f64) {
        let dt = self.last_update.map_or(0.0, |t| (now - t).max(0.0));
        self.last_update = Some(now);
        if !self.hashing() {
            return;
        }
        self.count(dt);
        let Some(id) = self.current_job else { return };
        let mask = self.chips[0].regs.get(&TICKET_MASK).copied().unwrap_or(0);
        // The ticket mask holds the difficulty - 1, bytes bit-reversed.
        let difficulty =
            u64::from(u32::from_be_bytes(mask.to_le_bytes().map(u8::reverse_bits))) + 1;
        let Some(job) = self.jobs.get_mut(&id) else {
            return;
        };
        let mut found = Vec::new();
        for _ in 0..self.hash_budget {
            if job.next_nonce >= 1 << 32 {
                break;
            }
            let nonce = job.next_nonce as u32;
            job.next_nonce += 1;
            let mut h = job.header;
            h[76..80].copy_from_slice(&nonce.to_le_bytes());
            let hash = Sha256::digest(Sha256::digest(h));
            // Difficulty of the hash: 2^224 / hash (little-endian 256-bit number).
            let top = u64::from_le_bytes(hash[24..32].try_into().unwrap());
            if top == 0 || (u64::MAX / top) >> 32 >= difficulty {
                found.push(nonce);
            }
        }
        self.hashes += self.hash_budget;
        let long = self.long_frames();
        for nonce in found {
            let mut r = if long {
                vec![0xaa, 0x55, 0, 0, 0, 0, 0, 0, 0, 0, 0x80]
            } else {
                vec![0xaa, 0x55, 0, 0, 0, 0, 0, 0, 0x80]
            };
            r[2..6].copy_from_slice(&nonce.to_le_bytes());
            let chunk: u16 = u16::from(id) << 3;
            r[6..8].copy_from_slice(&chunk.to_be_bytes());
            seal(&mut r);
            self.tx.extend(r);
            self.nonces_found += 1;
        }
    }

    /// Adds `dt` seconds of hashing to the counters of every chip.
    fn count(&mut self, dt: f64) {
        let domains = self.asic.domains();
        if domains == 0 {
            return;
        }
        let per_mhz = self.asic.small_cores() * 1e6 / domains as f64 / HASHES_PER_COUNT;
        for chip in &mut self.chips {
            chip.count_frac += chip.frequency_mhz() * per_mhz * dt;
            let whole = chip.count_frac.floor();
            chip.count_frac -= whole;
            let n = whole as u32;
            if n == 0 {
                continue;
            }
            for reg in &DOMAIN_COUNTERS[..domains] {
                let v = chip.regs.entry(*reg).or_insert(0);
                *v = v.wrapping_add(n);
            }
            let total = chip.regs.entry(TOTAL_COUNTER).or_insert(0);
            *total = total.wrapping_add(n.wrapping_mul(domains as u32));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(header: u8, addr: u8, reg: u8, value: Option<u32>) -> Vec<u8> {
        let mut f = vec![0x55, 0xaa, header, 0, addr, reg];
        if let Some(v) = value {
            f.extend(v.to_be_bytes());
        }
        f[3] = (f.len() - 2 + 1) as u8;
        let crc = crc5(&f[2..]);
        f.push(crc);
        f
    }

    #[test]
    fn crcs_match_the_protocol() {
        // Command::chain_inactive() and Command::read_reg(0, All) of bm13xx-protocol.
        assert_eq!(crc5(&[0x53, 0x05, 0x00, 0x00]), 0x03);
        assert_eq!(crc5(&[0x52, 0x05, 0x00, 0x00]), 0x0a);
        assert_eq!(crc5(&[0x40, 0x05, 0x08, 0x00]), 0x07);
        assert_eq!(crc16(b"123456789"), 0x29b1);
    }

    #[test]
    fn enumerates_one_chip() {
        let mut chain = Chain::new(Asic::Bm1370, 1);
        chain.set_reset(false);
        chain.receive(&[0x55, 0xaa, 0x52, 0x05, 0x00, 0x00, 0x0a]);
        let r: Vec<u8> = chain.tx.drain(..).collect();
        assert_eq!(&r[..8], &[0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(crc5(&r[2..]), 0);
    }

    #[test]
    fn addresses_every_chip_of_a_chain() {
        let mut chain = Chain::new(Asic::Bm1366, 6);
        chain.set_reset(false);
        chain.receive(&command(0x52, 0, CHIP_ID, None));
        assert_eq!(chain.tx.len(), 6 * 9);
        chain.tx.clear();
        chain.receive(&command(0x53, 0, 0, None));
        for i in 0..6u8 {
            chain.receive(&command(0x40, i * 0x2a, 0, None));
        }
        chain.receive(&command(0x52, 0, CHIP_ID, None));
        let r: Vec<u8> = chain.tx.drain(..).collect();
        let addrs: Vec<u8> = r.chunks(9).map(|f| f[6]).collect();
        assert_eq!(addrs, [0x00, 0x2a, 0x54, 0x7e, 0xa8, 0xd2]);
        assert!(r.chunks(9).all(|f| f[2..4] == [0x13, 0x66]));
        // A register write to one chip leaves the others.
        chain.receive(&command(0x41, 0x54, TICKET_MASK, Some(0xff)));
        chain.receive(&command(0x52, 0, TICKET_MASK, None));
        let r: Vec<u8> = chain.tx.drain(..).collect();
        let values: Vec<u8> = r.chunks(9).map(|f| f[5]).collect();
        assert_eq!(values, [0, 0, 0xff, 0, 0, 0]);
    }

    fn read(chain: &mut Chain, reg: u8) -> u32 {
        chain.tx.clear();
        chain.receive(&command(0x52, 0, reg, None));
        let r: Vec<u8> = chain.tx.drain(..).collect();
        u32::from_be_bytes(r[2..6].try_into().unwrap())
    }

    #[test]
    fn hash_counters_follow_the_frequency_while_hashing() {
        let mut chain = Chain::new(Asic::Bm1370, 1);
        chain.set_reset(false);
        // 525 MHz: FBDIV 168, REFDIV 2, POSTDIV1 4, POSTDIV2 1.
        chain.receive(&command(0x51, 0, PLL0_PARAMETER, Some(0x40a8_0230)));
        chain.update(0.0);
        chain.update(10.0);
        assert_eq!(read(&mut chain, 0x88), 0, "no work, no hashing");
        chain.jobs.insert(
            0,
            Job {
                header: [0; 80],
                next_nonce: 0,
            },
        );
        chain.current_job = Some(0);
        chain.update(20.0);
        // 525e6 × 2040 / 4 / 2^32 per second, for 10 s.
        let per_domain = 623;
        for reg in [0x88, 0x89, 0x8a, 0x8b] {
            assert_eq!(read(&mut chain, reg), per_domain);
        }
        assert_eq!(read(&mut chain, 0x8c), 4 * per_domain);
        assert_eq!(read(&mut chain, 0xac), 0, "four domains only");
        // A write clears a counter.
        chain.receive(&command(0x51, 0, 0x88, Some(0)));
        assert_eq!(read(&mut chain, 0x88), 0);
    }
}
