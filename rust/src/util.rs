//! Small helpers shared by the modules: hashing, randomness and timestamps.

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Lower-case hex of the SHA-256 of `text` (as UTF-8).
pub fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// `bytes` random bytes as lower-case hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the operating system provides random bytes");
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `String(value)` for a JSON value, as JavaScript would print it.
pub fn js_string(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => match number.as_f64() {
            Some(float) if float.fract() == 0.0 && float.abs() < 1e15 => {
                format!("{}", float as i64)
            }
            Some(float) => float.to_string(),
            None => number.to_string(),
        },
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// A JSON number that is a positive integer (`Number.isInteger(v) && v > 0`), as u32.
pub fn positive_int(value: Option<&serde_json::Value>) -> Option<u32> {
    let float = value?.as_f64()?;
    (float.fract() == 0.0 && float > 0.0 && float <= f64::from(u32::MAX)).then_some(float as u32)
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// `Date#toISOString()`: UTC with millisecond precision, e.g. `2026-09-29T00:00:00.000Z`.
pub fn iso_timestamp(epoch_ms: u64) -> String {
    let seconds = epoch_ms / 1000;
    let millis = epoch_ms % 1000;
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Parses an ISO 8601 UTC timestamp such as `2026-10-03T07:00:00Z` or
/// `2026-10-03T07:00:00.123Z` to milliseconds since the epoch.
pub fn parse_iso_ms(text: &str) -> Option<u64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let (year, month, day): (i64, i64, i64) = (
        date_parts.next()?.parse().ok()?,
        date_parts.next()?.parse().ok()?,
        date_parts.next()?.parse().ok()?,
    );
    let (clock, millis) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, format!("{fraction:0<3}")[..3].parse::<u64>().ok()?),
        None => (time, 0),
    };
    let mut time_parts = clock.split(':');
    let (hour, minute, second): (u64, u64, u64) = (
        time_parts.next()?.parse().ok()?,
        time_parts.next()?.parse().ok()?,
        time_parts.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days-from-civil (Howard Hinnant's algorithm).
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year.rem_euclid(400);
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days)
        .ok()
        .map(|days| ((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000) + millis)
}

/// `HH:MM:SS` in the local time zone, like the clock in a status line.
pub fn local_clock(epoch_ms: u64) -> String {
    let seconds = libc::time_t::try_from(epoch_ms / 1000).unwrap_or(0);
    // SAFETY: localtime_r writes into the zeroed tm we own and reads the time_t we pass.
    let broken_down = unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut parts);
        parts
    };
    format!(
        "{:02}:{:02}:{:02}",
        broken_down.tm_hour, broken_down.tm_min, broken_down.tm_sec
    )
}

/// [`iso_timestamp`] for the current time.
pub fn iso_now() -> String {
    iso_timestamp(now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn iso_timestamps_parse_back_to_the_same_instant() {
        for ms in [0, 1_790_000_000_123, 1_709_164_800_000, 4_107_542_400_000] {
            assert_eq!(parse_iso_ms(&iso_timestamp(ms)), Some(ms), "{ms}");
        }
        assert_eq!(
            parse_iso_ms("2026-10-03T07:00:00Z"),
            Some(1_791_010_800_000)
        );
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(parse_iso_ms("nope"), None);
        assert_eq!(parse_iso_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso_ms("2026-10-03T07:00:00"), None);
    }

    #[test]
    fn local_clock_has_the_clock_shape() {
        let text = local_clock(1_790_000_000_000);
        assert_eq!(text.len(), 8);
        assert!(
            text.bytes()
                .enumerate()
                .all(|(index, byte)| if index == 2 || index == 5 {
                    byte == b':'
                } else {
                    byte.is_ascii_digit()
                }),
            "{text}"
        );
    }

    #[test]
    fn js_string_prints_values_like_javascript() {
        use serde_json::json;
        assert_eq!(js_string(&json!("a")), "a");
        assert_eq!(js_string(&json!(12)), "12");
        assert_eq!(js_string(&json!(12.0)), "12");
        assert_eq!(js_string(&json!(1.5)), "1.5");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!([1, "b"])), "1,b");
        assert_eq!(js_string(&json!({})), "[object Object]");
        assert_eq!(positive_int(Some(&json!(7))), Some(7));
        assert_eq!(positive_int(Some(&json!(7.0))), Some(7));
        assert_eq!(positive_int(Some(&json!(0))), None);
        assert_eq!(positive_int(Some(&json!(-1))), None);
        assert_eq!(positive_int(Some(&json!(1.5))), None);
        assert_eq!(positive_int(Some(&json!("7"))), None);
        assert_eq!(positive_int(None), None);
    }

    #[test]
    fn random_hex_has_the_requested_length_and_varies() {
        let first = random_hex(8);
        assert_eq!(first.len(), 16);
        assert!(first
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert_ne!(first, random_hex(8));
    }

    #[test]
    fn iso_timestamp_matches_date_to_iso_string() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_timestamp(1_790_000_000_123), "2026-09-21T14:13:20.123Z");
        // 2024-02-29 exists; 2100-03-01 follows a non-leap February.
        assert_eq!(iso_timestamp(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
        assert_eq!(iso_timestamp(4_107_542_400_000), "2100-03-01T00:00:00.000Z");
    }
}
