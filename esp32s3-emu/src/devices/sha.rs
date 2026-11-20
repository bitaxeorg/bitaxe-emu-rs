//! SHA accelerator (0x6003_B000), register mode: SHA-1, SHA-224/256 and the
//! SHA-384/512 family, one block per START or CONTINUE.

use sha2::digest::generic_array::GenericArray;

const MODE: u32 = 0x00;
const T_STRING: u32 = 0x04;
const T_LENGTH: u32 = 0x08;
const DMA_BLOCK_NUM: u32 = 0x0c;
const START: u32 = 0x10;
const CONTINUE: u32 = 0x14;
const BUSY: u32 = 0x18;
const CLEAR_IRQ: u32 = 0x24;
const IRQ_ENA: u32 = 0x28;
const DATE: u32 = 0x2c;
const H_MEM: u32 = 0x40;
const M_MEM: u32 = 0x80;

const SHA1_IV: [u32; 5] = [
    0x6745_2301,
    0xefcd_ab89,
    0x98ba_dcfe,
    0x1032_5476,
    0xc3d2_e1f0,
];
const SHA224_IV: [u32; 8] = [
    0xc105_9ed8,
    0x367c_d507,
    0x3070_dd17,
    0xf70e_5939,
    0xffc0_0b31,
    0x6858_1511,
    0x64f9_8fa7,
    0xbefa_4fa4,
];
const SHA256_IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];
const SHA384_IV: [u64; 8] = [
    0xcbbb_9d5d_c105_9ed8,
    0x629a_292a_367c_d507,
    0x9159_015a_3070_dd17,
    0x152f_ecd8_f70e_5939,
    0x6733_2667_ffc0_0b31,
    0x8eb4_4a87_6858_1511,
    0xdb0c_2e0d_64f9_8fa7,
    0x47b5_481d_befa_4fa4,
];
const SHA512_IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];
const SHA512_224_IV: [u64; 8] = [
    0x8c3d_37c8_1954_4da2,
    0x73e1_9966_89dc_d4d6,
    0x1dfa_b7ae_32ff_9c82,
    0x679d_d514_582f_9fcf,
    0x0f6d_2b69_7bd4_4da8,
    0x77e3_6f73_04c4_8942,
    0x3f9d_85a8_6a1d_36c8,
    0x1112_e6ad_91d6_92a1,
];
const SHA512_256_IV: [u64; 8] = [
    0x2231_2194_fc2b_f72c,
    0x9f55_5fa3_c84c_64c2,
    0x2393_b86b_6f53_b151,
    0x9638_7719_5940_eabd,
    0x9628_3ee2_a88e_ffe3,
    0xbe5e_1e25_5386_3992,
    0x2b01_99fc_2c85_b8aa,
    0x0eb7_2ddc_81c5_2ca2,
];

fn sha1_compress(state: &mut [u32; 8], block: &[u8; 128]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (state[0], state[1], state[2], state[3], state[4]);
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
            20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
        *s = s.wrapping_add(v);
    }
}

pub struct Sha {
    mode: u32,
    /// Hash state as 32-bit words (the 64-bit words of SHA-512 as high, low).
    h: [u32; 16],
    m: [u8; 128],
    t_string: u32,
    t_length: u32,
    dma_block_num: u32,
    irq_ena: u32,
}

impl Default for Sha {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha {
    pub fn new() -> Self {
        Sha {
            mode: 0,
            h: [0; 16],
            m: [0; 128],
            t_string: 0,
            t_length: 0,
            dma_block_num: 0,
            irq_ena: 0,
        }
    }

    pub fn reset(&mut self) {
        self.h[..8].fill(0);
        self.m = [0; 128];
        self.dma_block_num = 0;
        self.irq_ena = 0;
    }

    fn init(&mut self) {
        let wide = |iv: [u64; 8]| {
            let mut h = [0u32; 16];
            for (i, v) in iv.iter().enumerate() {
                h[2 * i] = (v >> 32) as u32;
                h[2 * i + 1] = *v as u32;
            }
            h
        };
        self.h = match self.mode {
            0 => {
                let mut h = [0; 16];
                h[..5].copy_from_slice(&SHA1_IV);
                h
            }
            1 => {
                let mut h = [0; 16];
                h[..8].copy_from_slice(&SHA224_IV);
                h
            }
            2 => {
                let mut h = [0; 16];
                h[..8].copy_from_slice(&SHA256_IV);
                h
            }
            3 => wide(SHA384_IV),
            4 => wide(SHA512_IV),
            5 => wide(SHA512_224_IV),
            6 => wide(SHA512_256_IV),
            _ => wide(SHA512_IV.map(|v| v ^ 0xa5a5_a5a5_a5a5_a5a5)),
        };
    }

    fn compress(&mut self) {
        match self.mode {
            0 => {
                let mut st = [0u32; 8];
                st.copy_from_slice(&self.h[..8]);
                sha1_compress(&mut st, &self.m);
                self.h[..5].copy_from_slice(&st[..5]);
            }
            1 | 2 => {
                let mut st = [0u32; 8];
                st.copy_from_slice(&self.h[..8]);
                sha2::compress256(&mut st, &[*GenericArray::from_slice(&self.m[..64])]);
                self.h[..8].copy_from_slice(&st);
            }
            _ => {
                let mut st = [0u64; 8];
                for (i, s) in st.iter_mut().enumerate() {
                    *s = (u64::from(self.h[2 * i]) << 32) | u64::from(self.h[2 * i + 1]);
                }
                sha2::compress512(&mut st, &[*GenericArray::from_slice(&self.m)]);
                for (i, s) in st.iter().enumerate() {
                    self.h[2 * i] = (s >> 32) as u32;
                    self.h[2 * i + 1] = *s as u32;
                }
            }
        }
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            MODE => self.mode,
            DMA_BLOCK_NUM => self.dma_block_num,
            BUSY => 0,
            IRQ_ENA => self.irq_ena,
            DATE => 0x2019_0402,
            o if (H_MEM..M_MEM).contains(&o) => self.h[((o - H_MEM) / 4) as usize].swap_bytes(),
            o if (M_MEM..M_MEM + 128).contains(&o) => {
                let i = (o - M_MEM) as usize;
                u32::from_le_bytes(self.m[i..i + 4].try_into().unwrap())
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            MODE => self.mode = v & 7,
            T_STRING => self.t_string = v,
            T_LENGTH => self.t_length = v & 0x7f,
            DMA_BLOCK_NUM => self.dma_block_num = v & 0x3f,
            START => {
                self.init();
                if self.mode == 7 {
                    // SHA-512/t IV generation block: "SHA-512/" then t in ASCII.
                    self.m = [0; 128];
                    self.m[..8].copy_from_slice(b"SHA-512/");
                    self.m[8..12].copy_from_slice(&self.t_string.to_le_bytes());
                    self.m[124..128].copy_from_slice(&(self.t_length.swap_bytes()).to_le_bytes());
                }
                self.compress();
            }
            CONTINUE => self.compress(),
            CLEAR_IRQ => {}
            IRQ_ENA => self.irq_ena = v & 1,
            o if (H_MEM..M_MEM).contains(&o) => self.h[((o - H_MEM) / 4) as usize] = v.swap_bytes(),
            o if (M_MEM..M_MEM + 128).contains(&o) => {
                let i = (o - M_MEM) as usize;
                self.m[i..i + 4].copy_from_slice(&v.to_le_bytes());
            }
            _ => {}
        }
    }
}
