pub fn to_string(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_first_nul() {
        let buffer: Vec<u16> = "DELL U2723QE".encode_utf16().chain([0, 0x41, 0x42]).collect();
        assert_eq!(to_string(&buffer), "DELL U2723QE");
    }

    #[test]
    fn full_buffer_without_nul() {
        let buffer: Vec<u16> = "abc".encode_utf16().collect();
        assert_eq!(to_string(&buffer), "abc");
    }
}
