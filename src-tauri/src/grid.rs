use chrono::{DateTime, Duration as ChronoDuration, Local, LocalResult, NaiveDateTime, TimeZone, Timelike};
// `Mode` below is the schedule selector (Normal / Concentration).

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

/// Given the ISO start of a *break* slot (`Slot::start_iso()` when `phase ==
/// Break`), returns the ISO start of the *work* slot it follows -- i.e. the
/// pomodoro the reflection is actually about. Both break-slot gaps (`:00`-
/// `:25` work -> `:25` break, `:30`-`:55` work -> `:55` break) are exactly 25
/// minutes, so this is a fixed offset, not another `slot_for` recomputation.
/// `reflection.slot_start_at` should always be this value, not the raw break
/// start -- otherwise entries display as starting at `:25`/`:55` instead of
/// the `:00`/`:30` the work slot actually began.
pub fn preceding_work_slot_start_iso(break_slot_start_iso: &str) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(break_slot_start_iso).ok()?;
    Some((dt - ChronoDuration::minutes(25)).to_rfc3339())
}

/// Given the ISO start of a *break* slot, returns the ISO start of the *next*
/// work slot -- the one that begins the moment this break ends. Every break
/// is a fixed 5 minutes (`:25`-`:30`, `:55`-`:00`), so like
/// `preceding_work_slot_start_iso` this is a fixed offset, not another
/// `slot_for` recomputation. Used for the overlay's "Coming next" preview
/// (whatever's already saved for the upcoming slot, e.g. via a bulk edit
/// ahead of time) -- see native_overlay.rs's Android mirror and db.ts's
/// `nextWorkSlotStartIso` for the desktop/frontend equivalent.
pub fn next_work_slot_start_iso(break_slot_start_iso: &str) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(break_slot_start_iso).ok()?;
    Some((dt + ChronoDuration::minutes(5)).to_rfc3339())
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
/// :00-:25 work, :25-:30 break, :30-:55 work, :55-:00 break. (Normal mode.)
pub fn slot_for(now: DateTime<Local>) -> Slot {
    slot_for_mode(now, Mode::Normal)
}

/// Which wall-clock schedule is in effect. `Normal` is the original
/// 25/5/25/5 grid; `Concentration` is work :00-:25, break :25:00-:26:00 (1min),
/// work :26:00-:50, break :50-:00 (10min).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Normal,
    Concentration,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Concentration => "concentration",
        }
    }

    /// Unknown/blank values fall back to Normal so a bad row can't break scheduling.
    pub fn parse(s: &str) -> Mode {
        if s.trim().eq_ignore_ascii_case("concentration") {
            Mode::Concentration
        } else {
            Mode::Normal
        }
    }
}

/// Builds "today's wall-clock HH:mm:ss" for `now`'s hour; same DST handling as `at_minute`.
fn at_minute_sec(now: DateTime<Local>, minute: u32, sec: u32) -> DateTime<Local> {
    let naive = now
        .naive_local()
        .date()
        .and_hms_opt(now.hour(), minute, sec)
        .expect("hour/minute/second in valid range");
    resolve_local(&Local, naive)
}

pub fn slot_for_mode(now: DateTime<Local>, mode: Mode) -> Slot {
    // (phase, start (min, sec), end (min, sec)); end minute 60 = top of next hour.
    let (phase, start, end): (Phase, (u32, u32), (u32, u32)) = match mode {
        Mode::Normal => {
            let m = now.minute();
            if m < 25 {
                (Phase::Work, (0, 0), (25, 0))
            } else if m < 30 {
                (Phase::Break, (25, 0), (30, 0))
            } else if m < 55 {
                (Phase::Work, (30, 0), (55, 0))
            } else {
                (Phase::Break, (55, 0), (60, 0))
            }
        }
        Mode::Concentration => {
            let s = now.minute() * 60 + now.second();
            if s < 25 * 60 {
                (Phase::Work, (0, 0), (25, 0))
            } else if s < 26 * 60 {
                (Phase::Break, (25, 0), (26, 0))
            } else if s < 50 * 60 {
                (Phase::Work, (26, 0), (50, 0))
            } else {
                (Phase::Break, (50, 0), (60, 0))
            }
        }
    };

    let start_dt = at_minute_sec(now, start.0, start.1);
    let end_dt = if end.0 == 60 {
        at_minute(now, 0) + ChronoDuration::hours(1)
    } else {
        at_minute_sec(now, end.0, end.1)
    };

    Slot { phase, start: start_dt, end: end_dt }
}

/// Mode-aware `preceding_work_slot_start_iso`. Reflections always live on the
/// fixed :00/:30 slot grid, in both modes. Concentration breaks start at :25:00
/// and :50:00; a break starting in :00-:29 maps to that hour's :00 slot, one
/// starting in :30-:59 maps to :30 (so the :50 break -> :30, and the :26:00
/// work stretch is still filed under the :00 slot).
pub fn preceding_work_slot_start_iso_for(break_slot_start_iso: &str, mode: Mode) -> Option<String> {
    match mode {
        Mode::Normal => preceding_work_slot_start_iso(break_slot_start_iso),
        Mode::Concentration => {
            let dt = DateTime::parse_from_rfc3339(break_slot_start_iso).ok()?;
            let slot_min = if dt.minute() < 30 { 0 } else { 30 };
            Some(hour_start(dt)?.checked_add_signed(ChronoDuration::minutes(slot_min))?.to_rfc3339())
        }
    }
}

/// Mode-aware `next_work_slot_start_iso`, on the same :00/:30 grid: after a
/// break starting in :00-:29 the next slot is that hour's :30, after one
/// starting in :30-:59 it is the next hour's :00.
pub fn next_work_slot_start_iso_for(break_slot_start_iso: &str, mode: Mode) -> Option<String> {
    match mode {
        Mode::Normal => next_work_slot_start_iso(break_slot_start_iso),
        Mode::Concentration => {
            let dt = DateTime::parse_from_rfc3339(break_slot_start_iso).ok()?;
            let fwd = if dt.minute() < 30 { 30 } else { 60 };
            Some(hour_start(dt)?.checked_add_signed(ChronoDuration::minutes(fwd))?.to_rfc3339())
        }
    }
}

fn hour_start(dt: DateTime<chrono::FixedOffset>) -> Option<DateTime<chrono::FixedOffset>> {
    dt.with_minute(0)?.with_second(0)?.with_nanosecond(0)
}

/// Start instants of the next `n` break slots strictly after `now`, oldest
/// first. iOS schedules its break notifications from this, so the grid rule
/// stays in this one file instead of getting a Swift copy.
///
/// Walks slot to slot with `slot_for`, which is DST-safe but, inside a
/// fall-back hour, resolves ambiguous times to the *earlier* instant -- so a
/// slot's `end` can land at or before the time just examined. Stepping a
/// fixed 5 minutes in that case (and never emitting a start twice) keeps the
/// walk moving forward instead of cycling.
#[cfg(any(test, target_os = "ios"))]
pub fn upcoming_break_starts(now: DateTime<Local>, n: usize) -> Vec<DateTime<Local>> {
    let mut out: Vec<DateTime<Local>> = Vec::with_capacity(n);
    let mut t = now;
    // Generous bound: two breaks per hour, so ~4 slots per break plus DST slack.
    for _ in 0..(n * 8 + 32) {
        if out.len() >= n {
            break;
        }
        let slot = slot_for(t);
        if slot.phase == Phase::Break && slot.start > now && out.last().map_or(true, |last| slot.start > *last) {
            out.push(slot.start);
        }
        t = if slot.end > t { slot.end } else { t + ChronoDuration::minutes(5) };
    }
    out
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

    fn local_hms(h: u32, m: u32, s: u32) -> DateTime<Local> {
        let naive = chrono::NaiveDate::from_ymd_opt(2026, 3, 10)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap();
        resolve_local(&Local, naive)
    }

    #[test]
    fn concentration_bands() {
        let c = Mode::Concentration;
        let s = slot_for_mode(local_hms(10, 24, 59), c);
        assert_eq!((s.phase, s.start, s.end), (Phase::Work, local_hms(10, 0, 0), local_hms(10, 25, 0)));
        let s = slot_for_mode(local_hms(10, 25, 0), c);
        assert_eq!((s.phase, s.start, s.end), (Phase::Break, local_hms(10, 25, 0), local_hms(10, 26, 0)));
        let s = slot_for_mode(local_hms(10, 26, 0), c);
        assert_eq!((s.phase, s.start, s.end), (Phase::Work, local_hms(10, 26, 0), local_hms(10, 50, 0)));
        let s = slot_for_mode(local_hms(10, 50, 0), c);
        assert_eq!((s.phase, s.start, s.end), (Phase::Break, local_hms(10, 50, 0), local_hms(11, 0, 0)));
        let s = slot_for_mode(local_hms(10, 59, 59), c);
        assert_eq!(s.phase, Phase::Break);
    }

    #[test]
    fn concentration_preceding_and_next_work_slots() {
        let c = Mode::Concentration;
        let short = slot_for_mode(local_hms(10, 25, 10), c).start_iso();
        // Reflections stay on the :00/:30 grid: break at :25 -> :00, break at :50 -> :30.
        assert_eq!(preceding_work_slot_start_iso_for(&short, c).unwrap(), local_hms(10, 0, 0).to_rfc3339());
        assert_eq!(next_work_slot_start_iso_for(&short, c).unwrap(), local_hms(10, 30, 0).to_rfc3339());
        let long = slot_for_mode(local_hms(10, 55, 0), c).start_iso();
        assert_eq!(preceding_work_slot_start_iso_for(&long, c).unwrap(), local_hms(10, 30, 0).to_rfc3339());
        assert_eq!(next_work_slot_start_iso_for(&long, c).unwrap(), local_hms(11, 0, 0).to_rfc3339());
    }

    #[test]
    fn mode_parse_defaults_to_normal() {
        assert_eq!(Mode::parse("concentration"), Mode::Concentration);
        assert_eq!(Mode::parse(""), Mode::Normal);
        assert_eq!(Mode::parse("bogus"), Mode::Normal);
    }

    #[test]
    fn preceding_work_slot_start_maps_first_break_to_top_of_hour() {
        let break_start = slot_for(local(10, 27)).start_iso();
        let work_start = preceding_work_slot_start_iso(&break_start).unwrap();
        assert_eq!(work_start, local(10, 0).to_rfc3339());
    }

    #[test]
    fn preceding_work_slot_start_maps_second_break_to_half_past() {
        let break_start = slot_for(local(10, 58)).start_iso();
        let work_start = preceding_work_slot_start_iso(&break_start).unwrap();
        assert_eq!(work_start, local(10, 30).to_rfc3339());
    }

    #[test]
    fn next_work_slot_start_maps_first_break_to_half_past() {
        let break_start = slot_for(local(10, 27)).start_iso();
        let work_start = next_work_slot_start_iso(&break_start).unwrap();
        assert_eq!(work_start, local(10, 30).to_rfc3339());
    }

    #[test]
    fn next_work_slot_start_maps_second_break_to_top_of_next_hour() {
        let break_start = slot_for(local(10, 58)).start_iso();
        let work_start = next_work_slot_start_iso(&break_start).unwrap();
        assert_eq!(work_start, local(11, 0).to_rfc3339());
    }

    #[test]
    fn upcoming_breaks_from_mid_work_slot() {
        let starts = upcoming_break_starts(local(10, 10), 4);
        assert_eq!(starts, vec![local(10, 25), local(10, 55), local(11, 25), local(11, 55)]);
    }

    #[test]
    fn upcoming_breaks_skip_the_break_already_in_progress() {
        let starts = upcoming_break_starts(local(10, 27), 2);
        assert_eq!(starts, vec![local(10, 55), local(11, 25)]);
    }

    #[test]
    fn upcoming_breaks_exclude_a_break_starting_exactly_now() {
        assert_eq!(upcoming_break_starts(local(10, 25), 1), vec![local(10, 55)]);
    }

    #[test]
    fn upcoming_breaks_cover_a_full_day_and_are_strictly_increasing() {
        let starts = upcoming_break_starts(local(9, 0), 48);
        assert_eq!(starts.len(), 48);
        assert!(starts.windows(2).all(|w| w[0] < w[1]));
        assert!(starts.iter().all(|s| s.minute() == 25 || s.minute() == 55));
    }

    #[test]
    fn upcoming_breaks_across_dst_transitions_do_not_hang_or_repeat() {
        for now in [
            Local.with_ymd_and_hms(2026, 11, 1, 0, 40, 0).unwrap(),
            Local.with_ymd_and_hms(2026, 3, 8, 1, 40, 0).unwrap(),
        ] {
            let starts = upcoming_break_starts(now, 48);
            assert_eq!(starts.len(), 48);
            assert!(starts.windows(2).all(|w| w[0] < w[1]));
        }
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
