//! What time it is, in the one spelling a prompt carries.
//!
//! See `docs/notes/server-clock.md`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, to the minute, as `2026-11-01 21:03 UTC`.
///
/// # Invariant
///
/// Minute precision on purpose: the text lands in the last line of the system
/// prompt, so every change to it re-bills the transcript behind it. Two turns in
/// one minute are one text, and a session that needs the second reads its own
/// clock from a shell.
pub fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    at(seconds)
}

/// `seconds` since the epoch, as [`now`] spells it.
pub fn at(seconds: u64) -> String {
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    let (year, month, day) = civil(days);
    let (hour, minute) = (rest / 3600, (rest % 3600) / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// The Gregorian date `days` after 1970-01-01, as Howard Hinnant's `civil_from_days`.
fn civil(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let of_era = days.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A date the epoch is not: leap years, century leaps, and a month boundary.
    #[test]
    fn a_known_instant_reads_as_its_date_and_minute() {
        assert_eq!(at(0), "1970-01-01 00:00 UTC");
        assert_eq!(at(1_000_000_000), "2001-09-09 01:46 UTC");
        // 2024 was a leap year, and this is its 29th of February.
        assert_eq!(at(1_709_164_800), "2024-02-29 00:00 UTC");
        assert_eq!(at(1_735_689_600), "2025-01-01 00:00 UTC");
    }

    /// The minute is a minute: 59 seconds past it is still that minute.
    #[test]
    fn seconds_do_not_reach_the_text() {
        let minute = 1_735_689_600;
        assert_eq!(at(minute), at(minute + 59));
        assert_ne!(at(minute), at(minute + 60));
    }
}
