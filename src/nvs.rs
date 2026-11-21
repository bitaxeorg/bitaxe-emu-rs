//! Changes to the settings ESP-IDF's NVS keeps in the flash image.
//!
//! The partition table at 0x8000 gives the `nvs` partition: pages of 4 KB, each
//! a 32-byte header, a 32-byte bitmap of the entry states (two bits each, 2 for
//! written) and 126 entries of 32 bytes. An entry holds its namespace index,
//! type, span, chunk index, CRC-32, a 16-byte key and 8 bytes of data; the CRC
//! covers the entry without the CRC field, computed like the ROM's
//! `crc32_le(0xffffffff, ...)`.

const TABLE: usize = 0x8000;
const TABLE_ENTRY: usize = 32;
const TABLE_MAGIC: [u8; 2] = [0xaa, 0x50];
const TYPE_DATA: u8 = 1;
const SUBTYPE_NVS: u8 = 2;
const PAGE: usize = 4096;
const ENTRIES: usize = 126;
const ENTRY: usize = 32;
const WRITTEN: u8 = 2;
/// Unsigned integer types: U8, U16, U32, U64.
const UNSIGNED: [u8; 4] = [0x01, 0x02, 0x04, 0x08];

/// Offset and size of the `nvs` data partition.
fn partition(flash: &[u8]) -> Option<(usize, usize)> {
    (0..95).find_map(|i| {
        let e = flash.get(TABLE + i * TABLE_ENTRY..TABLE + (i + 1) * TABLE_ENTRY)?;
        if e[..2] != TABLE_MAGIC || e[2] != TYPE_DATA || e[3] != SUBTYPE_NVS {
            return None;
        }
        let offset = u32::from_le_bytes(e[4..8].try_into().ok()?) as usize;
        let size = u32::from_le_bytes(e[8..12].try_into().ok()?) as usize;
        Some((offset, size))
    })
}

/// CRC-32 as the ROM's `crc32_le(0xffffffff, ...)`: the register starts at 0.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn entry_crc(entry: &[u8]) -> u32 {
    let mut covered = Vec::with_capacity(ENTRY - 4);
    covered.extend_from_slice(&entry[..4]);
    covered.extend_from_slice(&entry[8..ENTRY]);
    crc32(&covered)
}

/// Sets every written unsigned integer entry named `key` to `value`. Returns how
/// many it changed.
pub fn set_unsigned(flash: &mut [u8], key: &str, value: u64) -> usize {
    let Some((offset, size)) = partition(flash) else {
        return 0;
    };
    let mut changed = 0;
    for page in (offset..offset + size).step_by(PAGE) {
        let Some(bitmap) = flash.get(page + 32..page + 64).map(|b| b.to_vec()) else {
            break;
        };
        let mut i = 0;
        while i < ENTRIES {
            let state = (bitmap[i / 4] >> ((i % 4) * 2)) & 3;
            let at = page + 64 + i * ENTRY;
            let Some(entry) = flash.get_mut(at..at + ENTRY) else {
                break;
            };
            let span = usize::from(entry[2]).max(1);
            let name = entry[8..24].split(|&b| b == 0).next().unwrap_or_default();
            if state == WRITTEN && name == key.as_bytes() && UNSIGNED.contains(&entry[1]) {
                let width = usize::from(entry[1]);
                entry[24..24 + width].copy_from_slice(&value.to_le_bytes()[..width]);
                let crc = entry_crc(entry);
                entry[4..8].copy_from_slice(&crc.to_le_bytes());
                changed += 1;
            }
            i += if state == WRITTEN { span } else { 1 };
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flash with a partition table and an `nvs` partition at 0x9000 whose
    /// first page holds the namespace `main` and `selftest` = 1 (U16), as the
    /// factory images of ESP-Miner.
    fn flash() -> Vec<u8> {
        let mut f = vec![0xff; 0x10000];
        let t = &mut f[TABLE..TABLE + TABLE_ENTRY];
        t[..4].copy_from_slice(&[0xaa, 0x50, TYPE_DATA, SUBTYPE_NVS]);
        t[4..8].copy_from_slice(&0x9000u32.to_le_bytes());
        t[8..12].copy_from_slice(&0x6000u32.to_le_bytes());
        let page = 0x9000;
        f[page..page + 4].copy_from_slice(&0xffff_fffeu32.to_le_bytes());
        // entries 0 and 1 written, the others empty
        f[page + 32] = 0b1111_1010;
        let mut write = |i: usize, ns: u8, ty: u8, key: &str, data: [u8; 8]| {
            let at = page + 64 + i * ENTRY;
            let e = &mut f[at..at + ENTRY];
            e[..4].copy_from_slice(&[ns, ty, 1, 0xff]);
            e[8..24].fill(0);
            e[8..8 + key.len()].copy_from_slice(key.as_bytes());
            e[24..32].copy_from_slice(&data);
            let crc = entry_crc(e);
            e[4..8].copy_from_slice(&crc.to_le_bytes());
        };
        write(
            0,
            0,
            0x01,
            "main",
            [1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        );
        write(
            1,
            1,
            0x02,
            "selftest",
            [1, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        );
        f
    }

    #[test]
    fn crc_matches_an_esp_idf_entry() {
        // `selftest` = 1 in the NVS of an ESP-Miner v2.15.4 factory image
        let e = [
            0x01, 0x02, 0x01, 0xff, 0x82, 0x90, 0xb8, 0x55, 0x73, 0x65, 0x6c, 0x66, 0x74, 0x65,
            0x73, 0x74, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ];
        assert_eq!(entry_crc(&e), 0x55b8_9082);
    }

    #[test]
    fn clears_the_self_test_flag() {
        let mut f = flash();
        assert_eq!(set_unsigned(&mut f, "selftest", 0), 1);
        let e = &f[0x9000 + 64 + ENTRY..0x9000 + 64 + 2 * ENTRY];
        assert_eq!(&e[24..26], &[0, 0]);
        assert_eq!(&e[26..32], &[0xff; 6], "the rest of the data is left");
        assert_eq!(
            u32::from_le_bytes(e[4..8].try_into().unwrap()),
            entry_crc(e)
        );
        assert_eq!(set_unsigned(&mut f, "nothere", 0), 0);
    }
}
