use chrono::{DateTime, Duration as ChronoDuration, Local, Timelike};

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

fn at_minute(now: DateTime<Local>, minute: u32) -> DateTime<Local> {
    now.with_minute(minute)
        .and_then(|d| d.with_second(0))
        .and_then(|d| d.with_nanosecond(0))
        .expect("minute/second/nanosecond in valid range")
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

/// Break slots are always 30 minutes apart (:25 and :55 each hour), so the
/// previous break slot's start is just this one minus 30 minutes.
pub fn previous_break_start(current_break_start: DateTime<Local>) -> DateTime<Local> {
    current_break_start - ChronoDuration::minutes(30)
}

/// If `now` falls in a work slot, returns the start of the 5-minute break
/// that ended when this work slot began (every work slot is immediately
/// preceded by a break) -- used at app startup to check for a reflection
/// that was never logged before the app was closed/killed/restarted.
/// Returns `None` if `now` falls in a break, since that's a live break the
/// normal scheduler already owns.
pub fn preceding_break_start(now: DateTime<Local>) -> Option<DateTime<Local>> {
    let slot = slot_for(now);
    match slot.phase {
        Phase::Work => Some(slot.start - ChronoDuration::minutes(5)),
        Phase::Break => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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

    #[test]
    fn previous_break_is_thirty_minutes_earlier() {
        assert_eq!(previous_break_start(local(10, 55)), local(10, 25));
        assert_eq!(previous_break_start(local(11, 25)), local(10, 55));
    }

    #[test]
    fn preceding_break_start_during_work() {
        assert_eq!(preceding_break_start(local(10, 5)), Some(local(9, 55)));
        assert_eq!(preceding_break_start(local(10, 45)), Some(local(10, 25)));
    }

    #[test]
    fn preceding_break_start_is_none_during_break() {
        assert_eq!(preceding_break_start(local(10, 27)), None);
    }
}
