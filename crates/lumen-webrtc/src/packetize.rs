//! RTP packetization helpers for H.264 (RFC 6184) and Opus (RFC 7587).

/// Parse a byte slice as Annex-B NAL units and return each unit's data.
pub fn split_annex_b(_data: &[u8]) -> Vec<&[u8]> {
    // TODO: split on 0x00 0x00 0x01 and 0x00 0x00 0x00 0x01 start codes
    vec![]
}

/// Return true if any NAL unit in the slice is an IDR (type 5).
pub fn contains_idr(nal_units: &[&[u8]]) -> bool {
    nal_units.iter().any(|nal| {
        nal.first().map(|&b| (b & 0x1F) == 5).unwrap_or(false)
    })
}
