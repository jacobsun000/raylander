use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CivilDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockState {
    pub date: CivilDate,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalendarDay {
    pub date: CivilDate,
    pub in_current_month: bool,
    pub is_today: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthGrid {
    pub year: i32,
    pub month: u8,
    /// Six complete Monday-first weeks.
    pub days: [CalendarDay; 42],
}

impl ClockState {
    /// Current UTC civil time. Local timezone presentation can be added by an adapter.
    pub fn now_utc() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    pub fn from_system_time(time: SystemTime) -> Self {
        let seconds = match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_secs() as i64,
            Err(error) => {
                let duration = error.duration();
                -(duration.as_secs() as i64) - i64::from(duration.subsec_nanos() != 0)
            }
        };
        let days = seconds.div_euclid(86_400);
        let within_day = seconds.rem_euclid(86_400);
        Self {
            date: civil_from_days(days),
            hour: (within_day / 3_600) as u8,
            minute: (within_day % 3_600 / 60) as u8,
            second: (within_day % 60) as u8,
        }
    }
}

pub struct LocalClockBackend;

impl LocalClockBackend {
    /// Queries the machine's configured local timezone through the system `date` utility.
    pub fn current(&self) -> Result<ClockState> {
        let output = Command::new("date")
            .arg("+%Y-%m-%d %H:%M:%S")
            .output()
            .context("failed to execute date for local time")?;
        if !output.status.success() {
            bail!("date exited with {}", output.status);
        }
        let stdout = String::from_utf8(output.stdout).context("date returned non-UTF-8 output")?;
        parse_local_clock(&stdout)
    }
}

pub fn parse_local_clock(value: &str) -> Result<ClockState> {
    let value = value.trim();
    if value.len() != 19 || !value.is_ascii() {
        bail!("local date output had an unexpected length");
    }
    let number = |range: std::ops::Range<usize>, name: &str| -> Result<u32> {
        value[range]
            .parse()
            .with_context(|| format!("local date output contained an invalid {name}"))
    };
    if &value[4..5] != "-"
        || &value[7..8] != "-"
        || &value[10..11] != " "
        || &value[13..14] != ":"
        || &value[16..17] != ":"
    {
        bail!("local date output had invalid separators");
    }
    let state = ClockState {
        date: CivilDate {
            year: number(0..4, "year")? as i32,
            month: number(5..7, "month")? as u8,
            day: number(8..10, "day")? as u8,
        },
        hour: number(11..13, "hour")? as u8,
        minute: number(14..16, "minute")? as u8,
        second: number(17..19, "second")? as u8,
    };
    let max_day = days_in_month(state.date.year, state.date.month)
        .context("local date output contained an invalid month")?;
    if state.date.day == 0
        || state.date.day > max_day
        || state.hour > 23
        || state.minute > 59
        || state.second > 59
    {
        bail!("local date output contained an out-of-range value");
    }
    Ok(state)
}

impl MonthGrid {
    pub fn new(year: i32, month: u8, today: CivilDate) -> Option<Self> {
        if !(1..=12).contains(&month) {
            return None;
        }
        let first = days_from_civil(CivilDate {
            year,
            month,
            day: 1,
        });
        // 1970-01-01 was Thursday; Monday is zero.
        let weekday = (first + 3).rem_euclid(7);
        let start = first - weekday;
        let days = std::array::from_fn(|index| {
            let date = civil_from_days(start + index as i64);
            CalendarDay {
                date,
                in_current_month: date.year == year && date.month == month,
                is_today: date == today,
            }
        });
        Some(Self { year, month, days })
    }
}

pub fn is_leap_year(year: i32) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

pub fn days_in_month(year: i32, month: u8) -> Option<u8> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

fn days_from_civil(date: CivilDate) -> i64 {
    let mut year = i64::from(date.year);
    let month = i64::from(date.month);
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(date.day) - 1;
    let day_of_era = yoe * 365 + yoe / 4 - yoe / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> CivilDate {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    CivilDate {
        year: year as i32,
        month: month as u8,
        day: day as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn leap_years_follow_gregorian_rules() {
        assert!(is_leap_year(2000));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(1900));
        assert!(!is_leap_year(2023));
        assert_eq!(days_in_month(2024, 2), Some(29));
        assert_eq!(days_in_month(2023, 2), Some(28));
        assert_eq!(days_in_month(2024, 13), None);
    }

    #[test]
    fn converts_epoch_and_pre_epoch_time() {
        assert_eq!(
            ClockState::from_system_time(UNIX_EPOCH),
            ClockState {
                date: CivilDate {
                    year: 1970,
                    month: 1,
                    day: 1
                },
                hour: 0,
                minute: 0,
                second: 0
            }
        );
        assert_eq!(
            ClockState::from_system_time(UNIX_EPOCH + Duration::from_secs(86_399)).date,
            CivilDate {
                year: 1970,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            ClockState::from_system_time(UNIX_EPOCH - Duration::from_secs(1)),
            ClockState {
                date: CivilDate {
                    year: 1969,
                    month: 12,
                    day: 31
                },
                hour: 23,
                minute: 59,
                second: 59
            }
        );
    }

    #[test]
    fn parses_and_validates_local_clock_output() {
        assert_eq!(
            parse_local_clock("2024-02-29 23:07:05\n").unwrap(),
            ClockState {
                date: CivilDate {
                    year: 2024,
                    month: 2,
                    day: 29
                },
                hour: 23,
                minute: 7,
                second: 5,
            }
        );
        assert!(parse_local_clock("2023-02-29 12:00:00").is_err());
        assert!(parse_local_clock("2024/02/29 12:00:00").is_err());
        assert!(parse_local_clock("2024-02-29 24:00:00").is_err());
    }

    #[test]
    fn month_grid_crosses_year_boundary_and_marks_today() {
        let today = CivilDate {
            year: 2025,
            month: 1,
            day: 1,
        };
        let grid = MonthGrid::new(2025, 1, today).unwrap();
        assert_eq!(
            grid.days[0].date,
            CivilDate {
                year: 2024,
                month: 12,
                day: 30
            }
        );
        assert_eq!(grid.days[2].date, today);
        assert!(grid.days[2].is_today);
        assert_eq!(
            grid.days[41].date,
            CivilDate {
                year: 2025,
                month: 2,
                day: 9
            }
        );
        assert_eq!(
            grid.days.iter().filter(|day| day.in_current_month).count(),
            31
        );
    }

    #[test]
    fn leap_day_appears_in_february_grid() {
        let grid = MonthGrid::new(
            2024,
            2,
            CivilDate {
                year: 2024,
                month: 2,
                day: 29,
            },
        )
        .unwrap();
        let leap_day = grid
            .days
            .iter()
            .find(|day| day.date.day == 29 && day.in_current_month)
            .unwrap();
        assert!(leap_day.is_today);
        assert_eq!(
            grid.days.iter().filter(|day| day.in_current_month).count(),
            29
        );
        assert!(MonthGrid::new(2024, 0, leap_day.date).is_none());
    }
}
