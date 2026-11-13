use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A frame older than this is already late for a live share. Sending it adds
/// delay, and the viewer would rather get the next fresh one.
pub const MAX_FRAME_AGE: Duration = Duration::from_millis(150);

/// Pointer position inside the captured frame, independent of the video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorSample {
    pub visible: bool,
    /// 0 is the left edge and 65535 is the right edge of the encoded frame.
    pub x: u16,
    /// 0 is the top edge and 65535 is the bottom edge of the encoded frame.
    pub y: u16,
}

impl CursorSample {
    pub fn to_bytes(self) -> [u8; 5] {
        let mut bytes = [0u8; 5];
        bytes[0] = u8::from(self.visible);
        bytes[1..3].copy_from_slice(&self.x.to_le_bytes());
        bytes[3..5].copy_from_slice(&self.y.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 5 {
            return None;
        }
        Some(Self {
            visible: bytes[0] != 0,
            x: u16::from_le_bytes([bytes[1], bytes[2]]),
            y: u16::from_le_bytes([bytes[3], bytes[4]]),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AccessUnit {
    pub payload: Vec<u8>,
    pub keyframe: bool,
    pub read_at: Instant,
    /// When ScreenCaptureKit handed us the pixels. The publisher uses this to
    /// drop a frame that has already missed the live window.
    pub captured_at: Instant,
    /// When the pixels were captured, in microseconds since the Unix epoch.
    /// Empty when the producer has no capture clock and the publisher should
    /// stamp the frame from the wall clock. Never a small value that starts at
    /// zero: those are read as 1970 and the receiver drops the chain.
    pub timestamp_us: Option<i64>,
    pub cursor: Option<CursorSample>,
}

/// Microseconds since the Unix epoch. Never zero.
pub fn unix_us_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as i64)
        .unwrap_or(1)
        .max(1)
}

/// Keeps capture timestamps strictly increasing without moving them off the
/// clock they were stamped with.
pub fn monotonic_unix_us(last: &mut i64, stamped: i64) -> i64 {
    let timestamp = stamped.max(1).max(last.saturating_add(1));
    *last = timestamp;
    timestamp
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
            captured_at: Instant::now(),
            timestamp_us: None,
            cursor: None,
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

    #[test]
    fn capture_timestamps_stay_on_the_wall_clock_and_move_forward() {
        use super::monotonic_unix_us;
        let mut last = 0;
        let first = monotonic_unix_us(&mut last, 1_700_000_000_000_000);
        let same = monotonic_unix_us(&mut last, first);
        let backwards = monotonic_unix_us(&mut last, first - 50);
        assert!(first > 1_600_000_000_000_000);
        assert_eq!(same, first + 1);
        assert_eq!(backwards, same + 1);
    }
}
