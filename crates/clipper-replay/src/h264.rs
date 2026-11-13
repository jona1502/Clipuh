//! H.264 bitstream helpers: Annex B (encoder output) to AVCC (MP4 payload).

/// NAL unit types used here (ITU-T H.264 table 7-1).
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// One access unit converted from Annex B.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvccAccessUnit {
    /// NAL units with 4-byte big-endian length prefixes. SPS, PPS and access
    /// unit delimiters are removed: parameter sets live in the `avcC` box.
    pub avcc: Vec<u8>,
    /// First SPS NAL unit found (raw, without start code).
    pub sps: Option<Vec<u8>>,
    /// First PPS NAL unit found (raw, without start code).
    pub pps: Option<Vec<u8>>,
    /// Contains an IDR slice.
    pub idr: bool,
}

/// Splits an Annex B byte stream into NAL unit payloads (start codes
/// removed). Trailing zero bytes before a start code are dropped.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(begin) = start {
                units.push(trim_trailing_zeros(&data[begin..i]));
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(begin) = start
        && begin <= data.len()
    {
        units.push(&data[begin..]);
    }
    units.retain(|unit| !unit.is_empty());
    units
}

fn trim_trailing_zeros(mut unit: &[u8]) -> &[u8] {
    while let [rest @ .., 0] = unit {
        unit = rest;
    }
    unit
}

/// Converts one Annex B access unit into AVCC form.
pub fn annexb_to_avcc(data: &[u8]) -> AvccAccessUnit {
    let mut out = AvccAccessUnit {
        avcc: Vec::with_capacity(data.len()),
        ..AvccAccessUnit::default()
    };
    for unit in nal_units(data) {
        match unit[0] & 0x1f {
            NAL_SPS => {
                if out.sps.is_none() {
                    out.sps = Some(unit.to_vec());
                }
                continue;
            }
            NAL_PPS => {
                if out.pps.is_none() {
                    out.pps = Some(unit.to_vec());
                }
                continue;
            }
            NAL_AUD => continue,
            NAL_IDR => out.idr = true,
            _ => {}
        }
        let len = u32::try_from(unit.len()).unwrap_or(u32::MAX);
        out.avcc.extend_from_slice(&len.to_be_bytes());
        out.avcc.extend_from_slice(unit);
    }
    out
}

/// True when an AVCC payload contains an IDR slice.
pub fn avcc_has_idr(data: &[u8]) -> bool {
    let mut rest = data;
    while rest.len() >= 5 {
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let Some(unit) = rest.get(4..4 + len) else {
            return false;
        };
        if unit.first().is_some_and(|byte| byte & 0x1f == NAL_IDR) {
            return true;
        }
        rest = &rest[4 + len..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9,
        ];
        let units = nal_units(&data);
        assert_eq!(
            units,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]
        );
    }

    #[test]
    fn converts_idr_and_extracts_parameter_sets() {
        let data = [
            0, 0, 0, 1, 0x09, 0xf0, // AUD
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, // SPS
            0, 0, 0, 1, 0x68, 0xee, // PPS
            0, 0, 1, 0x65, 0x88, 0x84, // IDR slice
        ];
        let unit = annexb_to_avcc(&data);
        assert!(unit.idr);
        assert_eq!(unit.sps.as_deref(), Some(&[0x67, 0x64, 0x00, 0x28][..]));
        assert_eq!(unit.pps.as_deref(), Some(&[0x68, 0xee][..]));
        assert_eq!(unit.avcc, vec![0, 0, 0, 3, 0x65, 0x88, 0x84]);
        assert!(avcc_has_idr(&unit.avcc));
    }

    #[test]
    fn non_idr_slice_is_not_a_keyframe() {
        let unit = annexb_to_avcc(&[0, 0, 0, 1, 0x41, 0x9a]);
        assert!(!unit.idr);
        assert_eq!(unit.avcc, vec![0, 0, 0, 2, 0x41, 0x9a]);
        assert!(!avcc_has_idr(&unit.avcc));
        assert!(!avcc_has_idr(&[0, 0, 0, 9, 0x65]));
    }
}
