use std::time::Instant;

#[derive(Debug, Clone)]
pub struct AccessUnit {
    pub payload: Vec<u8>,
    pub keyframe: bool,
    pub read_at: Instant,
    /// Capture time in microseconds, relative to the first frame.
    /// Empty when the producer has no capture clock and the publisher should
    /// stamp frames from their index.
    pub timestamp_us: Option<i64>,
}

/// Splits an Annex-B byte stream into access units.
///
/// A unit is one displayed frame. Parameter sets in front of an IDR stay with
/// that keyframe. If a later keyframe omits SPS/PPS, the last seen sets are
/// prepended so a subscriber can start on it.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub struct AnnexBSplitter {
    buf: Vec<u8>,
    au: Vec<u8>,
    au_has_vcl: bool,
    au_is_key: bool,
    au_has_sps: bool,
    au_has_pps: bool,
    last_sps: Vec<u8>,
    last_pps: Vec<u8>,
}

impl Default for AnnexBSplitter {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            au: Vec::new(),
            au_has_vcl: false,
            au_is_key: false,
            au_has_sps: false,
            au_has_pps: false,
            last_sps: Vec::new(),
            last_pps: Vec::new(),
        }
    }
}

#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
impl AnnexBSplitter {
    pub fn push(&mut self, data: &[u8]) -> Vec<AccessUnit> {
        self.buf.extend_from_slice(data);
        let mut units = Vec::new();
        loop {
            let Some((start, start_len)) = find_start(&self.buf, 0) else {
                if self.buf.len() > 3 {
                    self.buf.drain(..self.buf.len() - 3);
                }
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            let Some((next, _)) = find_start(&self.buf, start_len) else {
                break;
            };
            let nal = self.buf[..next].to_vec();
            self.buf.drain(..next);
            if let Some(unit) = self.push_nal(nal) {
                units.push(unit);
            }
        }
        units
    }

    pub fn finish(&mut self) -> Vec<AccessUnit> {
        let mut units = Vec::new();
        if let Some((start, _)) = find_start(&self.buf, 0) {
            if start > 0 {
                self.buf.drain(..start);
            }
            if nal_type(&self.buf).is_some() {
                let nal = std::mem::take(&mut self.buf);
                if let Some(unit) = self.push_nal(nal) {
                    units.push(unit);
                }
            }
        }
        if let Some(unit) = self.flush_au() {
            units.push(unit);
        }
        units
    }

    fn push_nal(&mut self, nal: Vec<u8>) -> Option<AccessUnit> {
        let Some(kind) = nal_type(&nal) else {
            return None;
        };
        let mut flushed = None;
        let is_vcl = kind == 1 || kind == 5;
        if kind == 9 || (is_vcl && self.au_has_vcl) {
            flushed = self.flush_au();
        }
        if kind == 9 {
            return flushed;
        }
        if kind == 7 {
            self.last_sps = nal.clone();
            self.au_has_sps = true;
        }
        if kind == 8 {
            self.last_pps = nal.clone();
            self.au_has_pps = true;
        }
        if kind == 5 {
            self.au_is_key = true;
        }
        if is_vcl {
            self.au_has_vcl = true;
        }
        self.au.extend_from_slice(&nal);
        flushed
    }

    fn flush_au(&mut self) -> Option<AccessUnit> {
        if self.au.is_empty() || !self.au_has_vcl {
            self.reset_au();
            return None;
        }
        let mut payload = Vec::new();
        if self.au_is_key {
            if !self.au_has_sps && !self.last_sps.is_empty() {
                payload.extend_from_slice(&self.last_sps);
            }
            if !self.au_has_pps && !self.last_pps.is_empty() {
                payload.extend_from_slice(&self.last_pps);
            }
        }
        payload.extend_from_slice(&self.au);
        let unit = AccessUnit {
            payload,
            keyframe: self.au_is_key,
            read_at: Instant::now(),
            timestamp_us: None,
        };
        self.reset_au();
        Some(unit)
    }

    fn reset_au(&mut self) {
        self.au.clear();
        self.au_has_vcl = false;
        self.au_is_key = false;
        self.au_has_sps = false;
        self.au_has_pps = false;
    }
}

#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn nal_type(nal: &[u8]) -> Option<u8> {
    let offset = start_code_len(nal)?;
    nal.get(offset).map(|byte| byte & 0x1f)
}

#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn start_code_len(data: &[u8]) -> Option<usize> {
    if data.len() >= 4 && data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1 {
        Some(4)
    } else if data.len() >= 3 && data[0] == 0 && data[1] == 0 && data[2] == 1 {
        Some(3)
    } else {
        None
    }
}

/// Converts length-prefixed NALs into one Annex-B access unit.
///
/// `parameter_sets` are the SPS and PPS from the format description. They are
/// written in front of a keyframe and any SPS/PPS already inside the payload
/// are skipped so the unit does not carry them twice.
pub fn length_prefixed_to_annexb(
    data: &[u8],
    nal_length_size: usize,
    parameter_sets: &[impl AsRef<[u8]>],
    keyframe: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    if keyframe {
        for set in parameter_sets {
            let set = set.as_ref();
            if set.is_empty() {
                continue;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(set);
        }
    }
    if !(1..=4).contains(&nal_length_size) {
        return out;
    }
    let mut offset = 0;
    while offset + nal_length_size <= data.len() {
        let mut raw = [0u8; 4];
        raw[4 - nal_length_size..].copy_from_slice(&data[offset..offset + nal_length_size]);
        let nal_len = u32::from_be_bytes(raw) as usize;
        offset += nal_length_size;
        if nal_len == 0 || offset + nal_len > data.len() {
            break;
        }
        let nal = &data[offset..offset + nal_len];
        offset += nal_len;
        let kind = nal.first().copied().unwrap_or(0) & 0x1f;
        if keyframe && (kind == 7 || kind == 8) {
            continue;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

pub fn length_prefixed_contains_idr(data: &[u8], nal_length_size: usize) -> bool {
    if !(1..=4).contains(&nal_length_size) {
        return false;
    }
    let mut offset = 0;
    while offset + nal_length_size <= data.len() {
        let mut raw = [0u8; 4];
        raw[4 - nal_length_size..].copy_from_slice(&data[offset..offset + nal_length_size]);
        let nal_len = u32::from_be_bytes(raw) as usize;
        offset += nal_length_size;
        if nal_len == 0 || offset + nal_len > data.len() {
            break;
        }
        if data[offset] & 0x1f == 5 {
            return true;
        }
        offset += nal_len;
    }
    false
}

#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn find_start(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index + 3 <= buf.len() {
        if index + 4 <= buf.len()
            && buf[index] == 0
            && buf[index + 1] == 0
            && buf[index + 2] == 0
            && buf[index + 3] == 1
        {
            return Some((index, 4));
        }
        if buf[index] == 0 && buf[index + 1] == 0 && buf[index + 2] == 1 {
            return Some((index, 3));
        }
        index += 1;
    }
    None
}

#[cfg(test)]
fn nal(header: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 1, header];
    bytes.extend_from_slice(body);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_idr_and_delta_and_keeps_parameter_sets_on_the_keyframe() {
        let mut stream = Vec::new();
        stream.extend(nal(0x67, b"sps"));
        stream.extend(nal(0x68, b"pps"));
        stream.extend(nal(0x65, b"idr"));
        stream.extend(nal(0x41, b"p"));

        let mut splitter = AnnexBSplitter::default();
        let mut units = Vec::new();
        for chunk in stream.chunks(3) {
            units.extend(splitter.push(chunk));
        }
        units.extend(splitter.finish());

        assert_eq!(units.len(), 2);
        assert!(units[0].keyframe);
        assert!(!units[1].keyframe);
        assert!(units[0].payload.windows(4).any(|win| win == [0, 0, 0, 1]));
        assert!(units[0].payload.windows(5).any(|win| win[4] == 0x67));
        assert!(units[0].payload.windows(5).any(|win| win[4] == 0x65));
        assert!(units[1].payload.windows(5).any(|win| win[4] == 0x41));
    }

    #[test]
    fn prepends_cached_parameter_sets_when_a_keyframe_omits_them() {
        let mut splitter = AnnexBSplitter::default();
        let mut first = Vec::new();
        first.extend(nal(0x09, &[0xf0]));
        first.extend(nal(0x67, b"sps"));
        first.extend(nal(0x68, b"pps"));
        first.extend(nal(0x65, b"idr"));
        let mut units = splitter.push(&first);
        units.extend(splitter.push(&nal(0x65, b"idr2")));
        units.extend(splitter.finish());

        let bare = units
            .iter()
            .find(|unit| unit.payload.windows(4).any(|w| w == b"idr2"));
        let bare = bare.expect("second keyframe");
        assert!(bare.keyframe);
        assert!(bare.payload.windows(b"sps".len()).any(|win| win == b"sps"));
        assert!(bare.payload.windows(b"pps".len()).any(|win| win == b"pps"));
    }

    #[test]
    fn converts_length_prefixed_idr_and_prepends_parameter_sets_once() {
        let sps = [0x67, b's', b'p', b's'];
        let pps = [0x68, b'p', b'p', b's'];
        let mut payload = Vec::new();
        for nal in [sps.as_slice(), pps.as_slice(), &[0x65, b'i', b'd', b'r']] {
            payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            payload.extend_from_slice(nal);
        }
        let annexb =
            length_prefixed_to_annexb(&payload, 4, &[sps.as_slice(), pps.as_slice()], true);
        let starts = annexb.windows(4).filter(|win| *win == [0, 0, 0, 1]).count();
        assert_eq!(starts, 3);
        assert!(length_prefixed_contains_idr(&payload, 4));
        assert!(!length_prefixed_contains_idr(&payload[..8], 4));
    }
}
