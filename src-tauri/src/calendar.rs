//! Upcoming calendar events for the home window's "Upcoming events" card (see
//! CLAUDE.md's "Calendar events").
//!
//! Sources are plain iCalendar (ICS) feeds fetched over HTTPS: a user's
//! Google Calendar "Secret address in iCal format" for personal events, and
//! Google's public per-country holiday feeds. No OAuth and no account -- the
//! secret URL *is* the credential, so the source list is stored encrypted
//! (`app_setting.calendar_sources`, an `enc1:` value) and never logged, exported
//! or synced.
//!
//! Events are never persisted: parsed feeds live in an in-memory cache with a
//! short TTL, so personal event titles never sit in `pomodoro.db`.
//!
//! The ICS parser here is deliberately small and in-house rather than the
//! `ical` + `rrule` crates: the feature set needed (VEVENT, DATE/UTC/TZID
//! times, DAILY/WEEKLY/MONTHLY/YEARLY rules with COUNT/UNTIL/BYDAY/BYMONTHDAY,
//! EXDATE, RECURRENCE-ID overrides) is modest, and it keeps the Android/iOS
//! cross-compile surface unchanged. A rule using anything beyond that (BYSETPOS,
//! BYWEEKNO, sub-daily frequencies, ...) degrades to showing just its first
//! instance rather than failing the whole calendar.
//!
//! Nothing here logs a URL, a title or a response body -- only source ids,
//! status classes and counts, same rule as the rest of the app.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, SecondsFormat,
    TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

const MAX_ICS_BYTES: usize = 5 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
/// Guards a pathological rule (e.g. a DAILY rule from 1970) from spinning.
const MAX_RECUR_ITER: usize = 20_000;
const MAX_TITLE_CHARS: usize = 200;

const DEFAULT_LOOKAHEAD_DAYS: i64 = 14;
const DEFAULT_MAX_ITEMS: usize = 8;

/// Device-local and a credential (secret iCal URLs): excluded from Data
/// export, skipped on import, kept across a Replace import -- see import.rs
/// and `exportAllData` in db.ts, which must stay in step with this key.
pub(crate) const SOURCES_KEY: &str = "calendar_sources";
const KEY_SOURCES: &str = SOURCES_KEY;
const KEY_LOOKAHEAD: &str = "calendar_lookahead_days";
const KEY_MAX_ITEMS: &str = "calendar_max_items";
const KEY_PUBLIC_ONLY: &str = "calendar_holiday_public_only";

// --- Public types -----------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
struct CalendarSource {
    id: String,
    name: String,
    /// `"personal"` or `"holiday"`.
    kind: String,
    url: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub title: String,
    /// `YYYY-MM-DD` for an all-day event, otherwise an RFC 3339 UTC instant.
    pub start: String,
    /// Same format as `start`; for an all-day event this is the last day
    /// *inclusive* (a one-day event has `end == start`).
    pub end: String,
    pub all_day: bool,
    pub source_name: String,
    pub kind: String,
    pub location: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceError {
    pub source_id: String,
    pub name: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpcomingEvents {
    pub events: Vec<CalendarEvent>,
    pub errors: Vec<SourceError>,
    /// At least one enabled source exists. The home card hides itself when
    /// this is false, so "nothing configured" never renders an empty box.
    pub configured: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarCheck {
    pub name: String,
    pub event_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolidayCountry {
    /// ISO 3166-1 alpha-2, so the UI can suggest one from the OS locale.
    pub code: String,
    pub name: String,
    pub url: String,
}

// --- Holiday feeds ----------------------------------------------------------

/// `(ISO code, display name, Google holiday calendar id)`. Every id here was
/// probed against the live feed and returns real events; ones that 500 (e.g.
/// Switzerland, Pakistan, UAE) are deliberately left out.
const HOLIDAY_COUNTRIES: &[(&str, &str, &str)] = &[
    ("AT", "Austria", "en.austrian"),
    ("AU", "Australia", "en.australian"),
    ("BR", "Brazil", "en.brazilian"),
    ("CA", "Canada", "en.canadian"),
    ("CN", "China", "en.china"),
    ("DE", "Germany", "en.german"),
    ("DK", "Denmark", "en.danish"),
    ("ES", "Spain", "en.spain"),
    ("FI", "Finland", "en.finnish"),
    ("FR", "France", "en.french"),
    ("GB", "United Kingdom", "en.uk"),
    ("GR", "Greece", "en.greek"),
    ("HK", "Hong Kong", "en.hong_kong"),
    ("ID", "Indonesia", "en.indonesian"),
    ("IE", "Ireland", "en.irish"),
    ("IN", "India", "en.indian"),
    ("IT", "Italy", "en.italian"),
    ("JP", "Japan", "en.japanese"),
    ("KR", "South Korea", "en.south_korea"),
    ("MX", "Mexico", "en.mexican"),
    ("MY", "Malaysia", "en.malaysia"),
    ("NL", "Netherlands", "en.dutch"),
    ("NO", "Norway", "en.norwegian"),
    ("NZ", "New Zealand", "en.new_zealand"),
    ("PH", "Philippines", "en.philippines"),
    ("PL", "Poland", "en.polish"),
    ("PT", "Portugal", "en.portuguese"),
    ("RU", "Russia", "en.russian"),
    ("SE", "Sweden", "en.swedish"),
    ("SG", "Singapore", "en.singapore"),
    ("TR", "Turkey", "en.turkish"),
    ("TW", "Taiwan", "en.taiwan"),
    ("US", "United States", "en.usa"),
    ("VN", "Vietnam", "en.vietnamese"),
    ("ZA", "South Africa", "en.sa"),
];

fn holiday_feed_url(calendar_id: &str) -> String {
    format!("https://calendar.google.com/calendar/ical/{calendar_id}%23holiday%40group.v.calendar.google.com/public/basic.ics")
}

#[tauri::command]
pub fn list_holiday_countries() -> Vec<HolidayCountry> {
    HOLIDAY_COUNTRIES
        .iter()
        .map(|(code, name, id)| HolidayCountry {
            code: (*code).to_string(),
            name: (*name).to_string(),
            url: holiday_feed_url(id),
        })
        .collect()
}

// --- ICS model --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Zone {
    Utc,
    Named(Tz),
    /// No `Z` and no `TZID`: wall-clock time in the viewer's own zone.
    Floating,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum When {
    Date(NaiveDate),
    Time(NaiveDateTime, Zone),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<usize>,
    until: Option<When>,
    /// `(ordinal, weekday)`; ordinal 0 means "every such weekday".
    by_day: Vec<(i32, Weekday)>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
}

#[derive(Clone, Debug)]
struct RawEvent {
    uid: String,
    title: String,
    location: String,
    start: When,
    end: Option<When>,
    duration: Option<ChronoDuration>,
    rule: Option<Rule>,
    exdates: Vec<When>,
    recurrence_id: Option<When>,
    cancelled: bool,
    /// Google's holiday feeds tag each event "Public holiday" or "Observance"
    /// in the first line of its DESCRIPTION.
    observance: bool,
}

#[derive(Debug, Default)]
struct ParsedCalendar {
    name: String,
    events: Vec<RawEvent>,
}

struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

fn to_utc(n: NaiveDateTime, z: Zone) -> Option<DateTime<Utc>> {
    match z {
        Zone::Utc => Some(Utc.from_utc_datetime(&n)),
        Zone::Named(tz) => tz.from_local_datetime(&n).earliest().map(|d| d.with_timezone(&Utc)),
        Zone::Floating => Local.from_local_datetime(&n).earliest().map(|d| d.with_timezone(&Utc)),
    }
}

/// A comparable identity for an occurrence's start, for EXDATE/RECURRENCE-ID
/// matching. Dates and instants can't collide in practice (~7e5 vs ~2e9).
fn when_key(w: &When) -> Option<i64> {
    match w {
        When::Date(d) => Some(i64::from(d.num_days_from_ce())),
        When::Time(n, z) => to_utc(*n, *z).map(|t| t.timestamp()),
    }
}

// --- ICS parsing ------------------------------------------------------------

/// RFC 5545 line unfolding: a line starting with a space or tab continues the
/// previous one.
fn unfold(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            if let Some(last) = out.last_mut() {
                last.push_str(&line[1..]);
            }
        } else {
            out.push(line.to_string());
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Prop> {
    let mut in_quote = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let head = &line[..colon];
    let value = &line[colon + 1..];

    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0;
    in_quote = false;
    for (i, c) in head.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ';' if !in_quote => {
                parts.push(&head[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&head[start..]);

    let name = parts[0].trim().to_ascii_uppercase();
    let params = parts[1..]
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        })
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

/// TEXT unescaping. Newlines become spaces: titles are shown on one line.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out.trim().to_string()
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn parse_when(value: &str, params: &[(String, String)]) -> Option<When> {
    let v = value.trim();
    let is_date = params.iter().any(|(k, val)| k == "VALUE" && val.eq_ignore_ascii_case("DATE")) || v.len() == 8;
    if is_date {
        return NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d").ok().map(When::Date);
    }
    let (core, utc) = match v.strip_suffix('Z') {
        Some(c) => (c, true),
        None => (v, false),
    };
    let naive = NaiveDateTime::parse_from_str(core, "%Y%m%dT%H%M%S").ok()?;
    let zone = if utc {
        Zone::Utc
    } else if let Some((_, tz)) = params.iter().find(|(k, _)| k == "TZID") {
        // An unrecognised TZID (e.g. Outlook's "Eastern Standard Time") falls
        // back to floating rather than dropping the event.
        tz.parse::<Tz>().map(Zone::Named).unwrap_or(Zone::Floating)
    } else {
        Zone::Floating
    };
    Some(When::Time(naive, zone))
}

fn parse_duration(s: &str) -> Option<ChronoDuration> {
    let rest = s.trim().trim_start_matches('+').strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (c, in_time) {
                    ('W', _) => n * 7 * 86_400,
                    ('D', _) => n * 86_400,
                    ('H', true) => n * 3_600,
                    ('M', true) => n * 60,
                    ('S', true) => n,
                    _ => return None,
                };
            }
            _ => return None,
        }
    }
    Some(ChronoDuration::seconds(total))
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

/// `None` means "can't expand this rule faithfully": the caller then shows the
/// event's first instance only.
fn parse_rrule(value: &str) -> Option<Rule> {
    let mut freq = None;
    let mut rule = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: Vec::new(),
        by_month_day: Vec::new(),
        by_month: Vec::new(),
    };
    for part in value.split(';') {
        let (k, v) = part.split_once('=')?;
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                });
            }
            "INTERVAL" => rule.interval = v.trim().parse::<i64>().ok()?.max(1),
            "COUNT" => rule.count = Some(v.trim().parse().ok()?),
            "UNTIL" => rule.until = Some(parse_when(v, &[])?),
            "BYDAY" => {
                for tok in v.split(',') {
                    let tok = tok.trim().to_ascii_uppercase();
                    if tok.len() < 2 {
                        return None;
                    }
                    let (ord, wd) = tok.split_at(tok.len() - 2);
                    let ord = if ord.is_empty() { 0 } else { ord.parse::<i32>().ok()? };
                    rule.by_day.push((ord, parse_weekday(wd)?));
                }
            }
            "BYMONTHDAY" => {
                for tok in v.split(',') {
                    rule.by_month_day.push(tok.trim().parse().ok()?);
                }
            }
            "BYMONTH" => {
                for tok in v.split(',') {
                    rule.by_month.push(tok.trim().parse().ok()?);
                }
            }
            "WKST" => {}
            _ => return None,
        }
    }
    rule.freq = freq?;
    Some(rule)
}

fn parse_ics(body: &str) -> ParsedCalendar {
    let mut cal = ParsedCalendar::default();
    let mut current: Option<RawEvent> = None;
    let mut in_alarm = false;

    for line in unfold(body.trim_start_matches('\u{feff}')) {
        let Some(prop) = parse_line(&line) else { continue };
        match (prop.name.as_str(), current.as_mut()) {
            ("BEGIN", _) if prop.value.eq_ignore_ascii_case("VEVENT") => {
                current = Some(RawEvent {
                    uid: String::new(),
                    title: String::new(),
                    location: String::new(),
                    // Replaced by DTSTART; an event that never gets one is
                    // dropped at END:VEVENT below.
                    start: When::Date(NaiveDate::MIN),
                    end: None,
                    duration: None,
                    rule: None,
                    exdates: Vec::new(),
                    recurrence_id: None,
                    cancelled: false,
                    observance: false,
                });
                in_alarm = false;
            }
            ("BEGIN", Some(_)) if prop.value.eq_ignore_ascii_case("VALARM") => in_alarm = true,
            ("END", Some(_)) if prop.value.eq_ignore_ascii_case("VALARM") => in_alarm = false,
            ("END", Some(_)) if prop.value.eq_ignore_ascii_case("VEVENT") => {
                if let Some(ev) = current.take() {
                    if ev.start != When::Date(NaiveDate::MIN) {
                        cal.events.push(ev);
                    }
                }
            }
            ("X-WR-CALNAME", None) => cal.name = truncate(&unescape_text(&prop.value), MAX_TITLE_CHARS),
            (_, Some(ev)) if !in_alarm => match prop.name.as_str() {
                "UID" => ev.uid = prop.value.trim().to_string(),
                "SUMMARY" => ev.title = truncate(&unescape_text(&prop.value), MAX_TITLE_CHARS),
                "LOCATION" => ev.location = truncate(&unescape_text(&prop.value), MAX_TITLE_CHARS),
                "DESCRIPTION" => {
                    let first = prop.value.split("\\n").next().unwrap_or("");
                    ev.observance = unescape_text(first).eq_ignore_ascii_case("Observance");
                }
                "STATUS" => ev.cancelled = prop.value.trim().eq_ignore_ascii_case("CANCELLED"),
                "DTSTART" => {
                    if let Some(w) = parse_when(&prop.value, &prop.params) {
                        ev.start = w;
                    }
                }
                "DTEND" => ev.end = parse_when(&prop.value, &prop.params),
                "DURATION" => ev.duration = parse_duration(&prop.value),
                "RRULE" => ev.rule = parse_rrule(&prop.value),
                "EXDATE" => {
                    for part in prop.value.split(',') {
                        if let Some(w) = parse_when(part, &prop.params) {
                            ev.exdates.push(w);
                        }
                    }
                }
                "RECURRENCE-ID" => ev.recurrence_id = parse_when(&prop.value, &prop.params),
                _ => {}
            },
            _ => {}
        }
    }
    cal
}

// --- Recurrence expansion ---------------------------------------------------

fn last_day_of_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1).and_then(|d| d.pred_opt()).map(|d| d.day()).unwrap_or(28)
}

fn nth_weekday(y: i32, m: u32, ord: i32, wd: Weekday) -> Option<NaiveDate> {
    if ord > 0 {
        return NaiveDate::from_weekday_of_month_opt(y, m, wd, u8::try_from(ord).ok()?);
    }
    let last = NaiveDate::from_ymd_opt(y, m, last_day_of_month(y, m))?;
    let back = (last.weekday().num_days_from_monday() + 7 - wd.num_days_from_monday()) % 7;
    let d = last - ChronoDuration::days(i64::from(back) + i64::from(ord.abs() - 1) * 7);
    (d.month() == m).then_some(d)
}

/// The dates within one month a MONTHLY/YEARLY rule selects.
fn month_days(y: i32, m: u32, rule: &Rule, start: NaiveDate) -> Vec<NaiveDate> {
    let mut v = Vec::new();
    let last = last_day_of_month(y, m);
    if !rule.by_month_day.is_empty() {
        for &md in &rule.by_month_day {
            let day = if md > 0 { md } else { last as i32 + 1 + md };
            if (1..=last as i32).contains(&day) {
                if let Some(d) = NaiveDate::from_ymd_opt(y, m, day as u32) {
                    v.push(d);
                }
            }
        }
    } else if !rule.by_day.is_empty() {
        for &(ord, wd) in &rule.by_day {
            if ord == 0 {
                for day in 1..=last {
                    if let Some(d) = NaiveDate::from_ymd_opt(y, m, day) {
                        if d.weekday() == wd {
                            v.push(d);
                        }
                    }
                }
            } else if let Some(d) = nth_weekday(y, m, ord, wd) {
                v.push(d);
            }
        }
    } else if let Some(d) = NaiveDate::from_ymd_opt(y, m, start.day()) {
        // A 31st (or Feb 29th) simply skips months/years that lack it, per RFC 5545.
        v.push(d);
    }
    v.sort();
    v.dedup();
    v
}

/// Every date the rule produces from `start` up to `limit`. COUNT counts every
/// generated date including ones later excluded by EXDATE, as the RFC says.
fn recur_dates(start: NaiveDate, rule: &Rule, limit: NaiveDate) -> Vec<NaiveDate> {
    let mut out: Vec<NaiveDate> = Vec::new();
    let max = rule.count.unwrap_or(usize::MAX);
    let interval = rule.interval.max(1);

    match rule.freq {
        Freq::Daily => {
            let mut d = start;
            let mut iter = 0;
            while d <= limit && out.len() < max && iter < MAX_RECUR_ITER {
                iter += 1;
                if rule.by_day.is_empty() || rule.by_day.iter().any(|(_, w)| *w == d.weekday()) {
                    out.push(d);
                }
                match d.checked_add_signed(ChronoDuration::days(interval)) {
                    Some(next) => d = next,
                    None => break,
                }
            }
        }
        Freq::Weekly => {
            let anchor = start - ChronoDuration::days(i64::from(start.weekday().num_days_from_monday()));
            let mut offsets: Vec<i64> = if rule.by_day.is_empty() {
                vec![i64::from(start.weekday().num_days_from_monday())]
            } else {
                rule.by_day.iter().map(|(_, w)| i64::from(w.num_days_from_monday())).collect()
            };
            offsets.sort_unstable();
            offsets.dedup();
            let mut k = 0i64;
            'weeks: while (k as usize) < MAX_RECUR_ITER {
                let Some(week) = anchor.checked_add_signed(ChronoDuration::days(k * interval * 7)) else { break };
                if week > limit {
                    break;
                }
                for off in &offsets {
                    let d = week + ChronoDuration::days(*off);
                    if d < start {
                        continue;
                    }
                    if d > limit || out.len() >= max {
                        break 'weeks;
                    }
                    out.push(d);
                }
                k += 1;
            }
        }
        Freq::Monthly => {
            let m0 = i64::from(start.year()) * 12 + i64::from(start.month0());
            let mut k = 0i64;
            'months: while (k as usize) < MAX_RECUR_ITER {
                let total = m0 + k * interval;
                let (y, m) = (total.div_euclid(12) as i32, (total.rem_euclid(12) + 1) as u32);
                let Some(first) = NaiveDate::from_ymd_opt(y, m, 1) else { break };
                if first > limit {
                    break;
                }
                for d in month_days(y, m, rule, start) {
                    if d < start {
                        continue;
                    }
                    if d > limit || out.len() >= max {
                        break 'months;
                    }
                    out.push(d);
                }
                k += 1;
            }
        }
        Freq::Yearly => {
            let mut months: Vec<u32> =
                if rule.by_month.is_empty() { vec![start.month()] } else { rule.by_month.clone() };
            months.sort_unstable();
            months.dedup();
            let mut k = 0i64;
            'years: while (k as usize) < MAX_RECUR_ITER {
                let y = i64::from(start.year()) + k * interval;
                let Ok(y) = i32::try_from(y) else { break };
                let Some(first) = NaiveDate::from_ymd_opt(y, 1, 1) else { break };
                if first > limit {
                    break;
                }
                for &m in &months {
                    for d in month_days(y, m, rule, start) {
                        if d < start {
                            continue;
                        }
                        if d > limit || out.len() >= max {
                            break 'years;
                        }
                        out.push(d);
                    }
                }
                k += 1;
            }
        }
    }
    out
}

/// The "now .. now + lookahead" window, in both frames an event can live in.
struct Window {
    now: DateTime<Utc>,
    end: DateTime<Utc>,
    today: NaiveDate,
    last_date: NaiveDate,
}

impl Window {
    fn at(now: DateTime<Local>, days: i64) -> Self {
        let now_utc = now.with_timezone(&Utc);
        let today = now.date_naive();
        Self {
            now: now_utc,
            end: now_utc + ChronoDuration::days(days),
            today,
            last_date: today + ChronoDuration::days(days),
        }
    }
}

struct Occurrence {
    sort_key: i64,
    event: CalendarEvent,
}

fn after_until(start: &When, until: &When) -> bool {
    match (start, until) {
        (When::Date(d), When::Date(u)) => d > u,
        (When::Date(d), When::Time(n, _)) => *d > n.date(),
        (When::Time(n, _), When::Date(u)) => n.date() > *u,
        (s, u) => matches!((when_key(s), when_key(u)), (Some(a), Some(b)) if a > b),
    }
}

fn expand_event(
    ev: &RawEvent,
    excluded: &HashSet<i64>,
    w: &Window,
    source_name: &str,
    kind: &str,
    out: &mut Vec<Occurrence>,
) {
    // A RECURRENCE-ID override is a standalone instance, never a rule itself.
    let rule = if ev.recurrence_id.is_some() { None } else { ev.rule.as_ref() };
    // Generous bound: covers any timezone offset between the event's frame
    // and the viewer's.
    let limit = w.last_date + ChronoDuration::days(2);

    let start_date = match ev.start {
        When::Date(d) => d,
        When::Time(n, _) => n.date(),
    };
    let dates = match rule {
        Some(r) => recur_dates(start_date, r, limit),
        None => vec![start_date],
    };

    for date in dates {
        let (when, all_day) = match ev.start {
            When::Date(_) => (When::Date(date), true),
            When::Time(n, z) => (When::Time(date.and_time(n.time()), z), false),
        };
        if when_key(&when).is_some_and(|k| excluded.contains(&k)) {
            continue;
        }
        if let Some(until) = rule.and_then(|r| r.until.as_ref()) {
            if after_until(&when, until) {
                continue;
            }
        }

        let (sort_key, start_s, end_s) = match when {
            When::Date(d) => {
                let len_days = match ev.end {
                    Some(When::Date(e)) => (e - start_date).num_days(),
                    _ => ev.duration.map(|d| d.num_days()).unwrap_or(1),
                }
                .max(1);
                let last_day = d + ChronoDuration::days(len_days - 1);
                if last_day < w.today || d > w.last_date {
                    continue;
                }
                let key_day = d.max(w.today);
                let key = Local
                    .from_local_datetime(&key_day.and_hms_opt(0, 0, 0).unwrap_or_default())
                    .earliest()
                    .map(|t| t.timestamp())
                    .unwrap_or(0);
                (key, d.format("%Y-%m-%d").to_string(), last_day.format("%Y-%m-%d").to_string())
            }
            When::Time(n, z) => {
                let Some(start_utc) = to_utc(n, z) else { continue };
                let len = match (ev.end, ev.start) {
                    (Some(When::Time(en, ez)), When::Time(sn, sz)) => match (to_utc(en, ez), to_utc(sn, sz)) {
                        (Some(e), Some(s)) => e - s,
                        _ => ChronoDuration::zero(),
                    },
                    _ => ev.duration.unwrap_or_else(ChronoDuration::zero),
                };
                let len = len.max(ChronoDuration::zero());
                let end_utc = start_utc + len;
                if end_utc.max(start_utc) < w.now || start_utc > w.end {
                    continue;
                }
                (
                    start_utc.timestamp(),
                    start_utc.to_rfc3339_opts(SecondsFormat::Secs, true),
                    end_utc.to_rfc3339_opts(SecondsFormat::Secs, true),
                )
            }
        };

        out.push(Occurrence {
            sort_key,
            event: CalendarEvent {
                title: if ev.title.is_empty() { "(No title)".to_string() } else { ev.title.clone() },
                start: start_s,
                end: end_s,
                all_day,
                source_name: source_name.to_string(),
                kind: kind.to_string(),
                location: ev.location.clone(),
            },
        });
    }
}

fn expand_calendar(
    cal: &ParsedCalendar,
    w: &Window,
    source_name: &str,
    kind: &str,
    public_only: bool,
    out: &mut Vec<Occurrence>,
) {
    // Instances a RECURRENCE-ID override replaces (or cancels) on its master.
    let mut overridden: HashMap<&str, HashSet<i64>> = HashMap::new();
    for ev in &cal.events {
        if let Some(key) = ev.recurrence_id.as_ref().and_then(when_key) {
            overridden.entry(ev.uid.as_str()).or_default().insert(key);
        }
    }

    for ev in &cal.events {
        if ev.cancelled || (public_only && ev.observance) {
            continue;
        }
        let mut excluded: HashSet<i64> = ev.exdates.iter().filter_map(when_key).collect();
        if let Some(o) = overridden.get(ev.uid.as_str()) {
            if ev.recurrence_id.is_none() {
                excluded.extend(o.iter().copied());
            }
        }
        expand_event(ev, &excluded, w, source_name, kind, out);
    }
}

// --- Fetching ---------------------------------------------------------------

/// Accepts `https://` and the `webcal://` scheme calendar apps hand out;
/// plain `http://` is refused since the URL itself is a credential.
fn normalize_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if let Some(rest) = url.strip_prefix("webcal://") {
        return Ok(format!("https://{rest}"));
    }
    if url.starts_with("https://") {
        return Ok(url.to_string());
    }
    Err("calendar URL must start with https:// (or webcal://)".to_string())
}

async fn fetch_ics(raw_url: &str) -> Result<String, String> {
    let url = normalize_url(raw_url)?;
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(|e| e.to_string())?;

    let resp = client.get(&url).send().await.map_err(|e| {
        // Deliberately not the full error: reqwest's Display can embed the URL.
        log::warn!("calendar: request failed (timeout={}, connect={})", e.is_timeout(), e.is_connect());
        "couldn't reach the calendar".to_string()
    })?;
    if !resp.status().is_success() {
        let status = resp.status();
        log::warn!("calendar: feed returned status {status}");
        return Err(format!("calendar returned status {status}"));
    }
    if resp.content_length().is_some_and(|n| n as usize > MAX_ICS_BYTES) {
        return Err("calendar is too large".to_string());
    }
    let bytes = resp.bytes().await.map_err(|_| "failed reading the calendar".to_string())?;
    if bytes.len() > MAX_ICS_BYTES {
        return Err("calendar is too large".to_string());
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    if !body.trim_start_matches('\u{feff}').trim_start().starts_with("BEGIN:VCALENDAR") {
        return Err("that URL isn't an iCalendar feed".to_string());
    }
    Ok(body)
}

struct CacheEntry {
    url: String,
    fetched: Instant,
    parsed: Arc<ParsedCalendar>,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the parsed feed (a stale cached copy if the refresh failed) plus
/// the refresh error, if any.
async fn load_source(src: &CalendarSource, force: bool) -> (Option<Arc<ParsedCalendar>>, Option<String>) {
    let cached = cache()
        .lock()
        .ok()
        .and_then(|c| c.get(&src.id).filter(|e| e.url == src.url).map(|e| (e.fetched, e.parsed.clone())));
    if let Some((at, parsed)) = &cached {
        if !force && at.elapsed() < CACHE_TTL {
            return (Some(parsed.clone()), None);
        }
    }
    match fetch_ics(&src.url).await {
        Ok(body) => {
            let parsed = Arc::new(parse_ics(&body));
            log::info!("calendar: source {} refreshed, {} events in feed", src.id, parsed.events.len());
            if let Ok(mut c) = cache().lock() {
                c.insert(
                    src.id.clone(),
                    CacheEntry { url: src.url.clone(), fetched: Instant::now(), parsed: parsed.clone() },
                );
            }
            (Some(parsed), None)
        }
        Err(e) => (cached.map(|(_, p)| p), Some(e)),
    }
}

// --- Settings ---------------------------------------------------------------

struct Config {
    sources: Vec<CalendarSource>,
    lookahead_days: i64,
    max_items: usize,
    holiday_public_only: bool,
}

async fn load_config(app: &AppHandle) -> Result<Config, String> {
    let pool = crate::db::open_direct_pool(app).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM app_setting WHERE key IN ($1, $2, $3, $4)",
    )
    .bind(KEY_SOURCES)
    .bind(KEY_LOOKAHEAD)
    .bind(KEY_MAX_ITEMS)
    .bind(KEY_PUBLIC_ONLY)
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("couldn't read calendar settings: {e}"))?;
    pool.close().await;

    let map: HashMap<String, String> = rows.into_iter().collect();

    let sources = match map.get(KEY_SOURCES).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Vec::new(),
        Some(stored) => {
            // May fail with `KEY_LOCKED:` -- the frontend treats any error as
            // "hide the card", same as the due-habits panel.
            let json = crate::crypto::FieldCipher::resolve(app).await?.decrypt(stored).await?;
            serde_json::from_str::<Vec<CalendarSource>>(&json)
                .map_err(|_| "calendar sources are unreadable".to_string())?
        }
    };

    Ok(Config {
        sources,
        lookahead_days: map
            .get(KEY_LOOKAHEAD)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(DEFAULT_LOOKAHEAD_DAYS)
            .clamp(1, 90),
        max_items: map
            .get(KEY_MAX_ITEMS)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_ITEMS)
            .clamp(1, 30),
        holiday_public_only: map.get(KEY_PUBLIC_ONLY).map(|v| v.trim() != "false").unwrap_or(true),
    })
}

// --- Commands ---------------------------------------------------------------

#[tauri::command]
pub async fn get_upcoming_events(app: AppHandle, force: bool) -> Result<UpcomingEvents, String> {
    let cfg = load_config(&app).await?;
    let enabled: Vec<CalendarSource> = cfg.sources.into_iter().filter(|s| s.enabled && !s.url.trim().is_empty()).collect();
    if enabled.is_empty() {
        return Ok(UpcomingEvents { events: Vec::new(), errors: Vec::new(), configured: false });
    }

    // Fetch the sources concurrently; each task owns its source.
    let handles: Vec<_> = enabled
        .iter()
        .cloned()
        .map(|src| tauri::async_runtime::spawn(async move { load_source(&src, force).await }))
        .collect();

    let window = Window::at(Local::now(), cfg.lookahead_days);
    let mut occurrences: Vec<Occurrence> = Vec::new();
    let mut errors: Vec<SourceError> = Vec::new();

    for (src, handle) in enabled.iter().zip(handles) {
        let (parsed, error) = match handle.await {
            Ok(r) => r,
            Err(_) => (None, Some("calendar refresh was interrupted".to_string())),
        };
        if let Some(message) = error {
            errors.push(SourceError { source_id: src.id.clone(), name: src.name.clone(), message });
        }
        if let Some(cal) = parsed {
            let public_only = src.kind == "holiday" && cfg.holiday_public_only;
            expand_calendar(&cal, &window, &src.name, &src.kind, public_only, &mut occurrences);
        }
    }

    occurrences.sort_by(|a, b| a.sort_key.cmp(&b.sort_key).then_with(|| a.event.title.cmp(&b.event.title)));
    let events = occurrences.into_iter().take(cfg.max_items).map(|o| o.event).collect();
    Ok(UpcomingEvents { events, errors, configured: true })
}

/// Settings' "Test" button: fetches the URL once and reports what it found.
#[tauri::command]
pub async fn validate_calendar_url(url: String) -> Result<CalendarCheck, String> {
    let body = fetch_ics(&url).await?;
    let parsed = parse_ics(&body);
    Ok(CalendarCheck { name: parsed.name, event_count: parsed.events.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-06 12:00 UTC, a 14-day window.
    fn window() -> Window {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        Window {
            now,
            end: now + ChronoDuration::days(14),
            today: NaiveDate::from_ymd_opt(2026, 10, 6).unwrap(),
            last_date: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        }
    }

    fn run(ics: &str, public_only: bool) -> Vec<CalendarEvent> {
        let cal = parse_ics(ics);
        let mut out = Vec::new();
        expand_calendar(&cal, &window(), "Test", "personal", public_only, &mut out);
        out.sort_by_key(|o| o.sort_key);
        out.into_iter().map(|o| o.event).collect()
    }

    fn wrap(events: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Work\r\n{events}END:VCALENDAR\r\n")
    }

    #[test]
    fn all_day_event_in_window_and_out_of_window() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261010\r\nDTEND;VALUE=DATE:20261011\r\nSUMMARY:Diwali\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20261201\r\nDTEND;VALUE=DATE:20261202\r\nSUMMARY:Too far\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:c\r\nDTSTART;VALUE=DATE:20260901\r\nDTEND;VALUE=DATE:20260902\r\nSUMMARY:Past\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].title, "Diwali");
        assert!(ev[0].all_day);
        assert_eq!((ev[0].start.as_str(), ev[0].end.as_str()), ("2026-10-10", "2026-10-10"));
    }

    #[test]
    fn multi_day_all_day_event_already_underway_is_kept() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261004\r\nDTEND;VALUE=DATE:20261008\r\nSUMMARY:Trip\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].start.as_str(), ev[0].end.as_str()), ("2026-10-04", "2026-10-07"));
    }

    #[test]
    fn tzid_event_converts_to_utc_with_dst() {
        // 2026-10-10 is EDT (UTC-4).
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=America/New_York:20261010T090000\r\nDTEND;TZID=America/New_York:20261010T100000\r\nSUMMARY:Standup\r\nLOCATION:Room 4\\, 2nd floor\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].start, "2026-10-10T13:00:00Z");
        assert_eq!(ev[0].end, "2026-10-10T14:00:00Z");
        assert_eq!(ev[0].location, "Room 4, 2nd floor");
        assert!(!ev[0].all_day);
    }

    #[test]
    fn event_that_already_ended_today_is_dropped() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261006T090000Z\r\nDTEND:20261006T100000Z\r\nSUMMARY:Earlier\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:b\r\nDTSTART:20261006T113000Z\r\nDTEND:20261006T130000Z\r\nSUMMARY:In progress\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].title, "In progress");
    }

    #[test]
    fn weekly_rule_with_exdate_and_count() {
        // Mon+Wed 09:00Z from Mon 2026-10-05, 6 instances: Oct 5,7,12,14,19,21.
        // Oct 14 is excluded, Oct 5 is already past, and Oct 21 falls after the
        // window ends (Oct 20 12:00Z).
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261005T090000Z\r\nDTEND:20261005T093000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=6\r\nEXDATE:20261014T090000Z\r\nSUMMARY:Sync\r\nEND:VEVENT\r\n",
        );
        let starts: Vec<String> = run(&ics, false).into_iter().map(|e| e.start).collect();
        assert_eq!(starts, vec!["2026-10-07T09:00:00Z", "2026-10-12T09:00:00Z", "2026-10-19T09:00:00Z"]);
    }

    #[test]
    fn until_stops_a_daily_rule() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261001T080000Z\r\nDTEND:20261001T081500Z\r\nRRULE:FREQ=DAILY;UNTIL=20261008T235959Z\r\nSUMMARY:Pills\r\nEND:VEVENT\r\n",
        );
        let starts: Vec<String> = run(&ics, false).into_iter().map(|e| e.start).collect();
        assert_eq!(starts, vec!["2026-10-07T08:00:00Z", "2026-10-08T08:00:00Z"]);
    }

    #[test]
    fn monthly_second_tuesday() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260101\r\nRRULE:FREQ=MONTHLY;BYDAY=2TU\r\nSUMMARY:Second Tue\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].start, "2026-10-13");
    }

    #[test]
    fn monthly_negative_ordinal_resolves_to_the_last_weekday() {
        assert_eq!(
            nth_weekday(2026, 10, -1, Weekday::Fri),
            NaiveDate::from_ymd_opt(2026, 10, 30)
        );
        assert_eq!(
            nth_weekday(2026, 10, -2, Weekday::Fri),
            NaiveDate::from_ymd_opt(2026, 10, 23)
        );
        // Oct 30 is past the Oct 20 window end, so the rule shows nothing yet.
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20260101\r\nRRULE:FREQ=MONTHLY;BYDAY=-1FR\r\nSUMMARY:Last Fri\r\nEND:VEVENT\r\n",
        );
        assert!(run(&ics, false).is_empty());
    }

    #[test]
    fn yearly_rule_repeats_a_birthday() {
        // The first event has an unparseable DTSTART and is dropped, not fatal.
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:1990-1010\r\nSUMMARY:bad date\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:19901010\r\nRRULE:FREQ=YEARLY\r\nSUMMARY:Birthday\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].start, "2026-10-10");
    }

    #[test]
    fn recurrence_id_override_replaces_one_instance() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:s\r\nDTSTART:20261007T090000Z\r\nDTEND:20261007T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nSUMMARY:Review\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID:20261014T090000Z\r\nDTSTART:20261014T150000Z\r\nDTEND:20261014T160000Z\r\nSUMMARY:Review (moved)\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        let got: Vec<(&str, &str)> = ev.iter().map(|e| (e.title.as_str(), e.start.as_str())).collect();
        assert_eq!(
            got,
            // The third weekly instance (Oct 21) is past the window end.
            vec![("Review", "2026-10-07T09:00:00Z"), ("Review (moved)", "2026-10-14T15:00:00Z")]
        );
    }

    #[test]
    fn cancelled_instance_override_removes_it() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:s\r\nDTSTART:20261007T090000Z\r\nDTEND:20261007T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=2\r\nSUMMARY:Review\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID:20261014T090000Z\r\nDTSTART:20261014T090000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:Review\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].start, "2026-10-07T09:00:00Z");
    }

    #[test]
    fn cancelled_event_is_skipped() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261010\r\nSTATUS:CANCELLED\r\nSUMMARY:Nope\r\nEND:VEVENT\r\n",
        );
        assert!(run(&ics, false).is_empty());
    }

    #[test]
    fn observances_are_filtered_only_when_public_only() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261010\r\nDESCRIPTION:Observance\\nTo hide observances\\, go to Settings\r\nSUMMARY:Minor day\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20261011\r\nDESCRIPTION:Public holiday\r\nSUMMARY:Big day\r\nEND:VEVENT\r\n",
        );
        assert_eq!(run(&ics, true).len(), 1);
        assert_eq!(run(&ics, true)[0].title, "Big day");
        assert_eq!(run(&ics, false).len(), 2);
    }

    #[test]
    fn unfolds_lines_and_unescapes_text_and_ignores_alarms() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261010\r\nSUMMARY:Lunch with\r\n  Sam\\; bring\\, \\\\notes\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nSUMMARY:alarm text\r\nEND:VALARM\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].title, "Lunch with Sam; bring, \\notes");
    }

    #[test]
    fn unsupported_rule_degrades_to_first_instance() {
        let ics = wrap(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261008\r\nRRULE:FREQ=MONTHLY;BYSETPOS=1;BYDAY=MO,TU\r\nSUMMARY:Odd\r\nEND:VEVENT\r\n",
        );
        let ev = run(&ics, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].start, "2026-10-08");
    }

    #[test]
    fn calendar_name_and_event_count_come_from_the_feed() {
        let cal = parse_ics(&wrap("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261010\r\nEND:VEVENT\r\n"));
        assert_eq!(cal.name, "Work");
        assert_eq!(cal.events.len(), 1);
    }

    #[test]
    fn url_normalisation_accepts_https_and_webcal_only() {
        assert_eq!(normalize_url(" https://x.test/a.ics ").unwrap(), "https://x.test/a.ics");
        assert_eq!(normalize_url("webcal://x.test/a.ics").unwrap(), "https://x.test/a.ics");
        assert!(normalize_url("http://x.test/a.ics").is_err());
        assert!(normalize_url("file:///etc/passwd").is_err());
        assert!(normalize_url("").is_err());
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("PT1H30M"), Some(ChronoDuration::minutes(90)));
        assert_eq!(parse_duration("P1D"), Some(ChronoDuration::days(1)));
        assert_eq!(parse_duration("P1W"), Some(ChronoDuration::days(7)));
        assert_eq!(parse_duration("1H"), None);
    }

    /// Hits the network, so it's opt-in: `cargo test --lib live_ -- --ignored`.
    /// Checks every country in the table still resolves to a parseable feed,
    /// which is the thing most likely to rot (Google can drop a calendar id).
    #[test]
    #[ignore]
    fn live_every_holiday_feed_fetches_and_parses() {
        tauri::async_runtime::block_on(async {
            for c in list_holiday_countries() {
                let body = fetch_ics(&c.url).await.unwrap_or_else(|e| panic!("{}: {e}", c.name));
                let cal = parse_ics(&body);
                assert!(cal.events.len() > 50, "{}: only {} events", c.name, cal.events.len());
                assert!(cal.name.starts_with("Holidays in"), "{}: name {:?}", c.name, cal.name);
            }
        });
    }

    #[test]
    fn every_holiday_country_has_a_unique_code_and_https_url() {
        let list = list_holiday_countries();
        let codes: HashSet<_> = list.iter().map(|c| c.code.clone()).collect();
        assert_eq!(codes.len(), list.len());
        assert!(list.iter().all(|c| c.url.starts_with("https://calendar.google.com/calendar/ical/en.")));
    }
}
