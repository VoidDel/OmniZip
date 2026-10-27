//! Conversions between archive time formats and `SystemTime`.
//!
//! MS-DOS timestamps (ZIP, RAR) carry no time zone and are local time of the machine that
//! created the archive; the best available interpretation is the local zone of this machine.

use std::{
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

fn local_offset() -> UtcOffset {
    static OFFSET: OnceLock<UtcOffset> = OnceLock::new();
    *OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

/// Builds a `SystemTime` from local calendar fields.
pub fn local_to_system(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Option<SystemTime> {
    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    let odt = PrimitiveDateTime::new(date, time).assume_offset(local_offset());
    Some(from_offset(odt))
}

/// Decodes packed MS-DOS date and time words.
pub fn dos_to_system(date: u16, time: u16) -> Option<SystemTime> {
    let year = 1980 + i32::from(date >> 9);
    let month = ((date >> 5) & 0x0F) as u8;
    let day = (date & 0x1F) as u8;
    let hour = (time >> 11) as u8;
    let minute = ((time >> 5) & 0x3F) as u8;
    let second = ((time & 0x1F) * 2) as u8;
    local_to_system(year, month, day, hour, minute, second.min(59))
}

/// Decodes a 32-bit MS-DOS timestamp (`date << 16 | time`) as used by RAR headers.
pub fn dos32_to_system(v: u32) -> Option<SystemTime> {
    dos_to_system((v >> 16) as u16, (v & 0xFFFF) as u16)
}

/// Local calendar fields `(year, month, day, hour, minute, second)` of a `SystemTime`.
pub fn system_to_local_fields(t: SystemTime) -> (i32, u8, u8, u8, u8, u8) {
    let odt = to_offset(t).to_offset(local_offset());
    (
        odt.year(),
        u8::from(odt.month()),
        odt.day(),
        odt.hour(),
        odt.minute(),
        odt.second(),
    )
}

fn to_offset(t: SystemTime) -> OffsetDateTime {
    OffsetDateTime::from(t)
}

fn from_offset(odt: OffsetDateTime) -> SystemTime {
    let nanos = odt.unix_timestamp_nanos();
    if nanos >= 0 {
        UNIX_EPOCH + Duration::from_nanos(nanos.min(u64::MAX as i128) as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos((-nanos).min(u64::MAX as i128) as u64)
    }
}

/// Seconds since the Unix epoch, clamped at zero (tar headers).
pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `SystemTime` from Unix seconds.
pub fn from_unix_secs(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_round_trip() {
        let t = local_to_system(2026, 10, 27, 15, 16, 42).unwrap();
        assert_eq!(system_to_local_fields(t), (2026, 10, 27, 15, 16, 42));
        // 2026-10-27 15:16:42 packed as DOS fields.
        let date = (((2026 - 1980) << 9) | (10 << 5) | 27) as u16;
        let time = ((15 << 11) | (16 << 5) | (42 / 2)) as u16;
        assert_eq!(dos_to_system(date, time), Some(t));
        assert_eq!(
            dos32_to_system((u32::from(date) << 16) | u32::from(time)),
            Some(t)
        );
        assert_eq!(dos_to_system(0, 0), None, "month 0 is invalid");
    }
}
