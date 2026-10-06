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
