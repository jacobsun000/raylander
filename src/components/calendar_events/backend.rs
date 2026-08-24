use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarEvent {
    pub title: String,
    pub location: String,
    pub url: String,
    pub start: DateTime<Local>,
    pub end: Option<DateTime<Local>>,
    pub all_day: bool,
}

pub struct GoogleCalendarBackend;

impl GoogleCalendarBackend {
    pub fn events(&self, urls: &[String]) -> Result<Vec<CalendarEvent>> {
        let mut events = Vec::new();
        for url in urls {
            let output = Command::new("curl")
                .args([
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "10",
                    url,
                ])
                .output()
                .context("failed to fetch Google Calendar")?;
            if !output.status.success() {
                bail!("Google Calendar request failed");
            }
            events.extend(parse_ics(&String::from_utf8_lossy(&output.stdout)));
        }
        events.sort_by_key(|event| event.start);
        events.dedup_by(|a, b| a.start == b.start && a.title == b.title);
        Ok(events)
    }
}

fn parse_ics(input: &str) -> Vec<CalendarEvent> {
    let unfolded = input.replace("\r\n ", "").replace("\n ", "");
    unfolded
        .split("BEGIN:VEVENT")
        .skip(1)
        .filter_map(|block| {
            let value = |name: &str| {
                block.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.starts_with(name).then(|| unescape(value.trim()))
                })
            };
            let raw_start = value("DTSTART")?;
            let (start, all_day) = parse_date(&raw_start)?;
            Some(CalendarEvent {
                title: value("SUMMARY").unwrap_or_else(|| "Untitled event".into()),
                location: value("LOCATION").unwrap_or_default(),
                url: value("URL").unwrap_or_default(),
                start,
                end: value("DTEND").and_then(|value| parse_date(&value).map(|item| item.0)),
                all_day,
            })
        })
        .collect()
}

fn parse_date(value: &str) -> Option<(DateTime<Local>, bool)> {
    if value.len() == 8 {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|date| (date, true));
    }
    if value.ends_with('Z') {
        let date = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ").ok()?;
        return Some((Utc.from_utc_datetime(&date).with_timezone(&Local), false));
    }
    let date = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    Local
        .from_local_datetime(&date)
        .earliest()
        .map(|date| (date, false))
}

fn unescape(value: &str) -> String {
    value
        .replace("\\n", " ")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timed_and_all_day_events() {
        let events = parse_ics(
            "BEGIN:VEVENT\nDTSTART:20260320T150000Z\nDTEND:20260320T160000Z\nSUMMARY:Design review\nLOCATION:Room 4\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART;VALUE=DATE:20260321\nSUMMARY:Day off\nEND:VEVENT",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Design review");
        assert!(!events[0].all_day);
        assert!(events[1].all_day);
    }
}
