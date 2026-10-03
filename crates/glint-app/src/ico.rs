//! Windows .ico container with PNG-compressed entries (supported since Windows Vista).

/// One icon image: its square size in pixels and the PNG bytes.
pub struct IcoEntry {
    pub size: u32,
    pub png: Vec<u8>,
}

const HEADER_BYTES: usize = 6;
const DIRECTORY_ENTRY_BYTES: usize = 16;

/// Serializes `entries` (sizes 1..=256) into an .ico file.
pub fn ico_bytes(entries: &[IcoEntry]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = HEADER_BYTES + DIRECTORY_ENTRY_BYTES * entries.len();
    for entry in entries {
        let side = if entry.size >= 256 { 0 } else { entry.size as u8 };
        bytes.extend_from_slice(&[side, side, 0, 0]);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&(entry.png.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += entry.png.len();
    }
    for entry in entries {
        bytes.extend_from_slice(&entry.png);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn directory_points_at_each_png() {
        let entries = [IcoEntry { size: 16, png: vec![1, 2, 3] }, IcoEntry { size: 256, png: vec![9; 5] }];
        let bytes = ico_bytes(&entries);
        assert_eq!((u16_at(&bytes, 0), u16_at(&bytes, 2), u16_at(&bytes, 4)), (0, 1, 2));
        assert_eq!(bytes[6], 16);
        assert_eq!(bytes[22], 0, "256 px is stored as 0");
        let first_offset = u32_at(&bytes, 6 + 12) as usize;
        assert_eq!(first_offset, 6 + 32);
        assert_eq!(&bytes[first_offset..first_offset + 3], &[1, 2, 3]);
        let second_offset = u32_at(&bytes, 22 + 12) as usize;
        assert_eq!(u32_at(&bytes, 22 + 8), 5);
        assert_eq!(&bytes[second_offset..], &[9; 5]);
        assert_eq!(u16_at(&bytes, 6 + 6), 32, "32 bits per pixel");
    }
}
