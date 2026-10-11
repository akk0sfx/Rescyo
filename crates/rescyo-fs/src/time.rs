//! Timestamps as file systems store them: some in UTC, some in an unknown local zone.

use serde::{Deserialize, Serialize};

/// Which clock a timestamp was recorded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeZone {
    /// Coordinated Universal Time (NTFS, exFAT with a UTC offset).
    Utc,
    /// The local wall clock of whatever machine wrote it (FAT); the zone is not recorded.
    Local,
}

/// Seconds since 1970-01-01 in the given clock, plus nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp {
    pub unix: i64,
    pub nanos: u32,
    pub zone: TimeZone,
}

impl Timestamp {
    /// Builds a timestamp from calendar fields, validating ranges. `None` for impossible dates.
    #[allow(clippy::too_many_arguments, reason = "one argument per calendar field")]
    pub fn from_civil(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        nanos: u32,
        zone: TimeZone,
    ) -> Option<Self> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        if hour > 23 || minute > 59 || second > 59 || nanos >= 1_000_000_000 {
            return None;
        }
        let days = days_from_civil(year, month, day);
        let unix = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
        Some(Self { unix, nanos, zone })
    }

    /// From a Windows FILETIME (100 ns ticks since 1601-01-01 UTC).
    pub fn from_filetime(ticks: u64) -> Option<Self> {
        const EPOCH_DIFF: i64 = 11_644_473_600;
        if ticks == 0 {
            return None;
        }
        let secs = i64::try_from(ticks / 10_000_000).ok()? - EPOCH_DIFF;
        let nanos = u32::try_from((ticks % 10_000_000) * 100).ok()?;
        Some(Self {
            unix: secs,
            nanos,
            zone: TimeZone::Utc,
        })
    }

    /// RFC 3339 text; local-zone stamps carry no offset.
    pub fn to_rfc3339(&self) -> String {
        let days = self.unix.div_euclid(86_400);
        let secs = self.unix.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        let suffix = match self.zone {
            TimeZone::Utc => "Z",
            TimeZone::Local => "",
        };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{suffix}",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60
        )
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip_and_validation() {
        let t = Timestamp::from_civil(2020, 9, 13, 12, 26, 40, 0, TimeZone::Utc).unwrap();
        assert_eq!(t.unix, 1_600_000_000);
        assert_eq!(t.to_rfc3339(), "2020-09-13T12:26:40Z");
        assert!(Timestamp::from_civil(2023, 2, 29, 0, 0, 0, 0, TimeZone::Local).is_none());
        assert!(Timestamp::from_civil(2024, 2, 29, 0, 0, 0, 0, TimeZone::Local).is_some());
        assert!(Timestamp::from_civil(1980, 13, 1, 0, 0, 0, 0, TimeZone::Local).is_none());
        assert!(Timestamp::from_civil(1980, 1, 1, 24, 0, 0, 0, TimeZone::Local).is_none());
        let old = Timestamp::from_civil(1969, 12, 31, 23, 59, 59, 0, TimeZone::Utc).unwrap();
        assert_eq!(old.unix, -1);
        assert_eq!(old.to_rfc3339(), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn filetime_conversion() {
        // 2020-09-13T12:26:40Z
        let t = Timestamp::from_filetime(132_444_736_000_000_000).unwrap();
        assert_eq!(t.unix, 1_600_000_000);
        assert!(Timestamp::from_filetime(0).is_none());
    }
}
