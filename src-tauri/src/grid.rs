use chrono::{DateTime, Duration as ChronoDuration, Local, LocalResult, NaiveDateTime, TimeZone, Timelike};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Work,
    Break,
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub phase: Phase,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
}

impl Slot {
    pub fn start_iso(&self) -> String {
        self.start.to_rfc3339()
    }
}

/// Builds "today's wall-clock HH:mm:00" in the Local timezone, for `now`'s
/// hour and the given `minute`. Deliberately does NOT go through
/// `DateTime::<Local>::with_minute` (which routes through chrono's
/// `map_local` -> `.single()`) -- `.single()` returns `None` for BOTH a
/// nonexistent local time (spring-forward gap) AND an *ambiguous* one
/// (fall-back repeated hour), and an `.expect()` on that used to abort the
/// whole process (`panic = "abort"` in release, see Cargo.toml) for the
/// entire transition hour, every year, in every DST-observing timezone --
/// `run_scheduler` calls this in a loop, so it died within a second of
/// entering the hour and died again on every relaunch until the hour passed.
fn at_minute(now: DateTime<Local>, minute: u32) -> DateTime<Local> {
    let naive = now
        .naive_local()
        .date()
        .and_hms_opt(now.hour(), minute, 0)
        .expect("hour/minute in valid range");
    resolve_local(&Local, naive)
}

/// Resolves a naive local datetime into a real instant in `tz`, handling
/// both DST failure modes `TimeZone::from_local_datetime` can report.
/// Factored out from `at_minute` (generic over `Tz` instead of hardcoded to
/// `Local`) purely so the tests below can exercise it directly against a
/// real IANA timezone via `chrono-tz` -- `Local` is whatever timezone the
/// machine running the tests happens to be in, which can't be pinned to a
/// DST-observing zone from a unit test otherwise.
fn resolve_local<Tz: TimeZone>(tz: &Tz, mut naive: NaiveDateTime) -> DateTime<Tz> {
    // Ambiguous (fall-back, the hour repeats): resolve to the earlier of the
    // two real instants -- an arbitrary but stable choice that keeps this
    // function total instead of panicking.
    //
    // Nonexistent (spring-forward, the hour is skipped): walk forward a
    // minute at a time until landing back in real time. Handles both
    // whole-hour transitions (the common case) and half-hour ones (Lord Howe
    // Island) without hardcoding either gap size. The loop only ever runs
    // during the transition itself, so the bound is generous on purpose.
    for _ in 0..180 {
        match tz.from_local_datetime(&naive) {
            LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => return dt,
            LocalResult::None => naive += ChronoDuration::minutes(1),
        }
    }
    unreachable!("no valid local time found within 3 hours of {naive}")
}

/// Sessions are pinned to fixed wall-clock boundaries, not "N minutes from app start":
/// :00-:25 work, :25-:30 break, :30-:55 work, :55-:00 break.
pub fn slot_for(now: DateTime<Local>) -> Slot {
    let m = now.minute();
    let (phase, start_min, end_min) = if m < 25 {
        (Phase::Work, 0, 25)
    } else if m < 30 {
        (Phase::Break, 25, 30)
    } else if m < 55 {
        (Phase::Work, 30, 55)
    } else {
        (Phase::Break, 55, 60)
    };

    let start = at_minute(now, start_min);
    let end = if end_min == 60 {
        at_minute(now, 0) + ChronoDuration::hours(1)
    } else {
        at_minute(now, end_min)
    };

    Slot { phase, start, end }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 8, 19, h, m, 0).unwrap()
    }

    #[test]
    fn work_slot_start_of_hour() {
        let slot = slot_for(local(10, 0));
        assert_eq!(slot.phase, Phase::Work);
        assert_eq!(slot.start, local(10, 0));
        assert_eq!(slot.end, local(10, 25));
    }

    #[test]
    fn work_slot_mid_range() {
        let slot = slot_for(local(10, 24));
        assert_eq!(slot.phase, Phase::Work);
        assert_eq!(slot.start, local(10, 0));
        assert_eq!(slot.end, local(10, 25));
    }

    #[test]
    fn first_break_slot() {
        let slot = slot_for(local(10, 27));
        assert_eq!(slot.phase, Phase::Break);
        assert_eq!(slot.start, local(10, 25));
        assert_eq!(slot.end, local(10, 30));
    }

    #[test]
    fn second_work_slot() {
        let slot = slot_for(local(10, 45));
        assert_eq!(slot.phase, Phase::Work);
        assert_eq!(slot.start, local(10, 30));
        assert_eq!(slot.end, local(10, 55));
    }

    #[test]
    fn second_break_slot_wraps_hour() {
        let slot = slot_for(local(10, 58));
        assert_eq!(slot.phase, Phase::Break);
        assert_eq!(slot.start, local(10, 55));
        assert_eq!(slot.end, local(11, 0));
    }

    // --- DST regression tests -------------------------------------------
    //
    // `Local` is whatever timezone the machine running the tests happens to
    // be in (this one was written on a UTC+5:30 machine, which never has a
    // DST transition), so these exercise `resolve_local` -- the actual
    // ambiguous/nonexistent-time resolution logic `at_minute` calls with
    // `Local` -- directly against `chrono_tz::America::New_York`, a real
    // IANA zone that does observe DST, rather than against `Local` itself.
    //
    // A couple of `slot_for`/`Local`-only smoke tests are kept alongside to
    // confirm the call site doesn't panic either, even though they can't
    // verify DST resolution on this machine.

    use chrono::{NaiveDate, Offset};
    use chrono_tz::America::New_York;

    fn ny_naive(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    #[test]
    fn resolve_local_picks_earlier_instant_for_ambiguous_fall_back_time() {
        // America/New_York falls back from 02:00 EDT to 01:00 EST on
        // 2026-11-01, so local 01:30 occurs twice: once at UTC-4 (EDT,
        // before the fall-back) and once at UTC-5 (EST, after it).
        let naive = ny_naive(2026, 11, 1, 1, 30);
        let resolved = resolve_local(&New_York, naive);

        // Resolves to a real instant whose wall-clock reading matches what
        // was asked for...
        assert_eq!(resolved.naive_local(), naive);
        // ...specifically the *earlier* of the two occurrences (still EDT,
        // UTC-4), per `resolve_local`'s documented tie-break.
        assert_eq!(resolved.offset().fix().local_minus_utc(), -4 * 3600);
    }

    #[test]
    fn resolve_local_walks_forward_past_nonexistent_spring_forward_time() {
        // America/New_York springs forward from 02:00 EST straight to
        // 03:00 EDT on 2026-03-08, so local 02:00-02:59 does not exist at
        // all on that date.
        let naive = ny_naive(2026, 3, 8, 2, 30);
        let resolved = resolve_local(&New_York, naive);

        // Landed at or after the requested (nonexistent) wall-clock time,
        // not before it, and not still stuck in the gap.
        assert!(resolved.naive_local() >= naive);
        assert!(resolved.naive_local() < ny_naive(2026, 3, 8, 3, 1));
        // The instant it actually resolved to is real: converting back to
        // New_York's own local time reproduces the same reading.
        assert_eq!(resolved.with_timezone(&New_York).naive_local(), resolved.naive_local());
        // And it's now on the DST side (EDT, UTC-4), confirming the walk
        // actually crossed the gap rather than stopping short of it.
        assert_eq!(resolved.offset().fix().local_minus_utc(), -4 * 3600);
    }

    #[test]
    fn dst_fall_back_does_not_panic_at_the_call_site() {
        let now = Local.with_ymd_and_hms(2026, 11, 1, 1, 40, 0).unwrap();
        let _ = slot_for(now); // must not panic
    }

    #[test]
    fn dst_spring_forward_does_not_panic_at_the_call_site() {
        let now = Local.with_ymd_and_hms(2026, 3, 8, 2, 44, 0).unwrap();
        let _ = slot_for(now); // must not panic
    }
}
