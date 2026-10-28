//! Monaco 0.55.1's cursor grapheme boundaries (base/common/strings.js).
//! Copyright Microsoft Corporation, MIT; permission/warranty notice:
//! public/assets/translator-codicon-LICENSE.txt.
//!
//! grapheme-breaks.bin is its pinned getGraphemeBreakRawData table: heap-ordered
//! (start, end, class) triples, each encoded as three little-endian u32 values.
const DATA: &[u8] = include_bytes!("grapheme-breaks.bin");

const OTHER: u32 = 0;
const PREPEND: u32 = 1;
const CR: u32 = 2;
const LF: u32 = 3;
const CONTROL: u32 = 4;
const EXTEND: u32 = 5;
const REGIONAL: u32 = 6;
const SPACING: u32 = 7;
const L: u32 = 8;
const V: u32 = 9;
const T: u32 = 10;
const LV: u32 = 11;
const LVT: u32 = 12;
const ZWJ: u32 = 13;
const PICTOGRAPHIC: u32 = 14;

fn class(code: u32) -> u32 {
    if code < 32 {
        return match code {
            10 => LF,
            13 => CR,
            _ => CONTROL,
        };
    }
    if code < 127 {
        return OTHER;
    }
    let mut index = 1;
    while let Some(node) = DATA.get(index * 12..(index + 1) * 12) {
        let start = u32::from_le_bytes(node[..4].try_into().unwrap());
        let end = u32::from_le_bytes(node[4..8].try_into().unwrap());
        if code < start {
            index *= 2;
        } else if code > end {
            index = index * 2 + 1;
        } else {
            return u32::from_le_bytes(node[8..].try_into().unwrap());
        }
    }
    OTHER
}

fn breaks(a: u32, b: u32) -> bool {
    // Retain the pinned implementation's fast path and its deliberately
    // stateless ZWJ/regional-indicator rules, not a newer Unicode algorithm.
    match (a, b) {
        (OTHER, _) => !matches!(b, EXTEND | SPACING),
        (CR, LF) => false,
        (CONTROL | CR | LF, _) | (_, CONTROL | CR | LF) => true,
        (L, L | V | LV | LVT) | (LV | V, V | T) | (LVT | T, T) => false,
        (_, EXTEND | ZWJ | SPACING) | (PREPEND, _) => false,
        (ZWJ, PICTOGRAPHIC) | (REGIONAL, REGIONAL) => false,
        _ => true,
    }
}

fn code_point(units: &[u16], offset: usize) -> (u32, usize) {
    let first = units[offset];
    if (0xd800..=0xdbff).contains(&first)
        && let Some(&second) = units.get(offset + 1)
        && (0xdc00..=0xdfff).contains(&second)
    {
        return (
            0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00,
            offset + 2,
        );
    }
    (u32::from(first), offset + 1)
}

fn previous_point(units: &[u16], offset: usize) -> (u32, usize) {
    let mut start = offset - 1;
    if (0xdc00..=0xdfff).contains(&units[start])
        && start > 0
        && (0xd800..=0xdbff).contains(&units[start - 1])
    {
        start -= 1;
    }
    (code_point(units, start).0, start)
}

pub(super) fn next(units: &[u16], offset: usize) -> usize {
    if offset == units.len() {
        return offset;
    }
    let (code, mut position) = code_point(units, offset);
    let mut last = class(code);
    while position < units.len() {
        let (code, end) = code_point(units, position);
        let current = class(code);
        if breaks(last, current) {
            break;
        }
        last = current;
        position = end;
    }
    position
}

pub(super) fn previous(units: &[u16], offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let (code, mut position) = previous_point(units, offset);
    let mut last = class(code);
    while position > 0 {
        let (code, start) = previous_point(units, position);
        let current = class(code);
        if breaks(current, last) {
            break;
        }
        last = current;
        position = start;
    }
    position
}
