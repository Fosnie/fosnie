// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! When a practice is open, and therefore what a caller can be offered.
//!
//! Everything about time lives here, and nothing here touches a database. Opening hours
//! are minutes from **local** midnight in the practice's own zone, and turning those into
//! instants is the one genuinely hard thing in the diary, because twice a year the local
//! clock is not a function of the local calendar.
//!
//! Two decisions rather than two panics:
//!
//! - the hour the clocks go **forward** does not exist, so a slot inside it is skipped
//!   entirely rather than nudged to a time nobody asked for;
//! - the hour they go **back** happens twice, so a slot inside it is offered **once**, at
//!   the earlier of the two, because a caller offered the same wall-clock time twice in one
//!   list cannot tell them apart and neither can whoever reads the diary afterwards.
//!
//! The other rule worth stating: the model never supplies an instant. Availability hands
//! out tokens and booking re-derives the instant from these same rules, so an appointment
//! outside the opening hours cannot be created even by asking for one.

use chrono::{Datelike, TimeZone, Timelike};
use chrono_tz::Tz;
use time::OffsetDateTime;

/// One opening period on one weekday, in minutes from local midnight.
///
/// Several per weekday, so a lunch break is two of these rather than a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opening {
    /// 0 is Monday.
    pub weekday: u8,
    pub opens_minute: i32,
    pub closes_minute: i32,
}

/// Everything about a practice's diary that decides what can be offered.
#[derive(Debug, Clone)]
pub struct Diary {
    pub timezone: String,
    pub slot_minutes: i32,
    pub lead_minutes: i32,
    pub horizon_days: i32,
    pub hours: Vec<Opening>,
    /// Local calendar dates the practice is shut, whatever the hours say.
    pub closures: Vec<time::Date>,
}

/// A time that could be offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The instant it starts. Also, in RFC 3339, the token the agent hands back to book it.
    pub starts_at: OffsetDateTime,
    /// What to say to the caller, in the practice's own local time.
    pub spoken: String,
}

/// How many slots one answer offers.
///
/// Six, because this is read aloud. A caller cannot hold a list, so the point is to give
/// them a real choice and then stop; the agent asks again if none of them suit.
pub const OFFER_LIMIT: usize = 6;

/// Is this a zone the time-zone database knows?
///
/// Rejecting an unknown name on the way in is what makes every later conversion safe:
/// there is no sensible fallback for a diary whose zone cannot be resolved, and quietly
/// using UTC instead would offer appointments an hour or five out with no sign of trouble.
pub fn zone(name: &str) -> Option<Tz> {
    name.parse::<Tz>().ok()
}

/// The instant at which a given local wall-clock time occurs, if it occurs at all.
///
/// `None` when the local clock skips that time, which is the hour the clocks go forward.
/// When the local clock repeats it, the earlier of the two is returned: see the note at
/// the top of this module for why one rather than both.
fn instant_at(tz: Tz, date: time::Date, minute_of_day: i32) -> Option<OffsetDateTime> {
    let (h, m) = ((minute_of_day / 60) as u32, (minute_of_day % 60) as u32);
    let local = tz.with_ymd_and_hms(date.year(), date.month() as u32, date.day() as u32, h, m, 0);
    let dt = match local {
        chrono::LocalResult::Single(dt) => dt,
        // Twice over. `earliest` is the one before the clocks changed.
        chrono::LocalResult::Ambiguous(earlier, _later) => earlier,
        // The local clock never reads this. There is no instant to offer.
        chrono::LocalResult::None => return None,
    };
    OffsetDateTime::from_unix_timestamp(dt.timestamp()).ok()
}

/// The local calendar date, in the practice's zone, of an instant.
pub fn local_date(tz: Tz, at: OffsetDateTime) -> Option<time::Date> {
    let dt = chrono::DateTime::from_timestamp(at.unix_timestamp(), 0)?.with_timezone(&tz);
    let month = time::Month::try_from(dt.month() as u8).ok()?;
    time::Date::from_calendar_date(dt.year(), month, dt.day() as u8).ok()
}

/// 0 for Monday, matching how the opening hours are written down.
fn weekday_index(date: time::Date) -> u8 {
    date.weekday().number_days_from_monday()
}

/// Every time this practice could see somebody, between now and its horizon.
///
/// `booked` is the instants already taken. `now` is passed in rather than read, so the
/// whole of this is a function of its arguments and can be tested against a real day.
///
/// The horizon is counted in **local days**, not in hours from now: walking the calendar
/// is what makes it so. Cutting it at exactly this time of day some number of days hence
/// would leave the last day half offered, at a boundary nobody chose and which moves with
/// the time the caller happens to ring.
pub fn open_slots(
    diary: &Diary,
    now: OffsetDateTime,
    booked: &[OffsetDateTime],
    limit: usize,
) -> Vec<Slot> {
    let Some(tz) = zone(&diary.timezone) else { return Vec::new() };
    let earliest = now + time::Duration::minutes(diary.lead_minutes as i64);
    let Some(first_day) = local_date(tz, now) else { return Vec::new() };

    let mut out = Vec::new();
    for day_offset in 0..=diary.horizon_days {
        let Some(date) = first_day.checked_add(time::Duration::days(day_offset as i64)) else {
            break;
        };
        if diary.closures.contains(&date) {
            continue;
        }
        let weekday = weekday_index(date);
        for opening in diary.hours.iter().filter(|o| o.weekday == weekday) {
            let mut minute = opening.opens_minute;
            // While a whole appointment fits before closing. A slot that would run past
            // the end of the day is not a slot: the practice shuts at the time it says.
            while minute + diary.slot_minutes <= opening.closes_minute {
                if let Some(starts_at) = instant_at(tz, date, minute) {
                    if starts_at >= earliest
                        && !booked.contains(&starts_at)
                        && !out.iter().any(|s: &Slot| s.starts_at == starts_at)
                    {
                        out.push(Slot { starts_at, spoken: spoken_at(tz, starts_at) });
                    }
                }
                minute += diary.slot_minutes;
            }
        }
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by_key(|s| s.starts_at);
    out.truncate(limit);
    out
}

/// Is this instant a slot this diary would offer, ignoring what is already booked?
///
/// What makes a booking safe. The agent hands back a token it was given, and this decides
/// whether that token names a real opening: a stale one, an invented one, one outside the
/// hours or one on a day the practice is shut all fail here rather than becoming an
/// appointment nobody can keep.
pub fn is_open_slot(diary: &Diary, at: OffsetDateTime) -> bool {
    let Some(tz) = zone(&diary.timezone) else { return false };
    let Some(date) = local_date(tz, at) else { return false };
    if diary.closures.contains(&date) {
        return false;
    }
    let weekday = weekday_index(date);
    diary.hours.iter().filter(|o| o.weekday == weekday).any(|opening| {
        let mut minute = opening.opens_minute;
        while minute + diary.slot_minutes <= opening.closes_minute {
            if instant_at(tz, date, minute) == Some(at) {
                return true;
            }
            minute += diary.slot_minutes;
        }
        false
    })
}

/// Names of the days and months, so a time can be said rather than printed.
const DAYS: [&str; 7] =
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];

/// A time as the practice would say it, in the practice's own zone.
///
/// Never an offset and never a "Z". The agent reads this aloud to somebody holding a
/// telephone, and a caller who hears "fourteen ten UTC" has been told the time by a
/// computer rather than by a receptionist.
pub fn spoken_at(tz: Tz, at: OffsetDateTime) -> String {
    let Some(dt) = chrono::DateTime::from_timestamp(at.unix_timestamp(), 0) else {
        return "an unknown time".into();
    };
    let dt = dt.with_timezone(&tz);
    let day = DAYS[dt.weekday().num_days_from_monday() as usize];
    let month = MONTHS[(dt.month() as usize).saturating_sub(1).min(11)];
    let (hour12, suffix) = match dt.hour() {
        0 => (12, "in the morning"),
        h @ 1..=11 => (h, "in the morning"),
        12 => (12, "midday"),
        h @ 13..=17 => (h - 12, "in the afternoon"),
        h => (h - 12, "in the evening"),
    };
    let minute = dt.minute();
    let clock = if minute == 0 {
        format!("{hour12} o'clock")
    } else {
        format!("{hour12}:{minute:02}")
    };
    format!("{day} the {} of {month}, {clock} {suffix}", ordinal(dt.day()))
}

/// "1st", "2nd", "23rd" and the rest, because a date said aloud has an ending.
fn ordinal(day: u32) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

/// Which part of the day a caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartOfDay {
    Morning,
    Afternoon,
    Any,
}

impl PartOfDay {
    pub fn parse(raw: Option<&str>) -> PartOfDay {
        match raw {
            Some("morning") => PartOfDay::Morning,
            Some("afternoon") => PartOfDay::Afternoon,
            _ => PartOfDay::Any,
        }
    }

    /// Does a slot fall in this part of the practice's own day?
    pub fn covers(self, tz: Tz, at: OffsetDateTime) -> bool {
        if self == PartOfDay::Any {
            return true;
        }
        let Some(dt) = chrono::DateTime::from_timestamp(at.unix_timestamp(), 0) else {
            return true;
        };
        let hour = dt.with_timezone(&tz).hour();
        match self {
            PartOfDay::Morning => hour < 12,
            PartOfDay::Afternoon => hour >= 12,
            PartOfDay::Any => true,
        }
    }
}

/// Is the caller the person who booked this appointment?
///
/// Two independent things have to agree, and neither on its own is enough. The reference
/// is something only somebody who was told it should have; matching the number they are
/// ringing from, or the name recorded against the booking, is the second. A reference
/// alone would let anybody who overheard one act on it, and a name alone would let anybody
/// who guessed a common one.
///
/// The reference having already been matched is the caller's side of this function: it
/// takes what was found by reference and asks whether the rest agrees.
pub fn caller_matches(
    recorded_name: &str,
    recorded_e164: &str,
    given_name: &str,
    calling_from: &str,
) -> bool {
    // The number is the stronger of the two, when there is one to compare.
    if !recorded_e164.is_empty() && recorded_e164 == calling_from {
        return true;
    }
    // Otherwise the name, compared the same way two writings of a name are decided to be
    // one party anywhere else in this feature.
    let (a, b) = (super::conflict::normalise(recorded_name), super::conflict::normalise(given_name));
    !a.is_empty() && !b.is_empty() && a == b
}

/// The most times one call may try to name an appointment it did not book.
pub const MAX_IDENTIFY_ATTEMPTS: i16 = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn london() -> Tz {
        zone("Europe/London").expect("Europe/London is a real zone")
    }

    /// Nine to five on a Tuesday, with an hour for lunch.
    fn weekday_diary(timezone: &str) -> Diary {
        Diary {
            timezone: timezone.into(),
            slot_minutes: 30,
            lead_minutes: 0,
            horizon_days: 14,
            hours: vec![
                Opening { weekday: 1, opens_minute: 9 * 60, closes_minute: 12 * 60 },
                Opening { weekday: 1, opens_minute: 13 * 60, closes_minute: 17 * 60 },
            ],
            closures: Vec::new(),
        }
    }

    #[test]
    fn a_zone_has_to_be_a_real_one() {
        assert!(zone("Europe/London").is_some());
        assert!(zone("America/New_York").is_some());
        assert!(zone("UTC").is_some());
        // No fallback: a diary whose zone cannot be resolved offers nothing, rather than
        // quietly offering times in the wrong one.
        assert!(zone("Europe/Brigadoon").is_none());
        assert!(zone("").is_none());
        assert!(zone("+01:00").is_none());
    }

    #[test]
    fn opening_hours_become_slots_with_the_break_left_out() {
        let diary = weekday_diary("Europe/London");
        // A Monday, so the first Tuesday is the next day.
        let now = datetime!(2026-08-03 00:00 UTC);
        let slots = open_slots(&diary, now, &[], 100);
        let tuesday: Vec<&Slot> = slots
            .iter()
            .filter(|s| local_date(london(), s.starts_at) == time::Date::from_calendar_date(2026, time::Month::August, 4).ok())
            .collect();
        // Six in the morning session, eight in the afternoon one.
        assert_eq!(tuesday.len(), 14, "{:#?}", tuesday.iter().map(|s| &s.spoken).collect::<Vec<_>>());
        // Nothing during the hour the practice is shut for lunch.
        assert!(!tuesday.iter().any(|s| s.spoken.contains("12:")));
        // And nothing that would still be going after closing.
        assert!(tuesday.iter().any(|s| s.spoken.contains("4:30")));
        assert!(!tuesday.iter().any(|s| s.spoken.contains("5:00")));
    }

    #[test]
    fn a_taken_slot_is_not_offered() {
        let diary = weekday_diary("Europe/London");
        let now = datetime!(2026-08-03 00:00 UTC);
        let all = open_slots(&diary, now, &[], 100);
        let taken = all[0].starts_at;
        let left = open_slots(&diary, now, &[taken], 100);
        assert_eq!(left.len(), all.len() - 1);
        assert!(!left.iter().any(|s| s.starts_at == taken));
    }

    #[test]
    fn nothing_is_offered_on_a_day_the_practice_is_shut() {
        let mut diary = weekday_diary("Europe/London");
        diary.closures = vec![time::Date::from_calendar_date(2026, time::Month::August, 4).unwrap()];
        let now = datetime!(2026-08-03 00:00 UTC);
        let slots = open_slots(&diary, now, &[], 100);
        assert!(!slots.iter().any(|s| s.spoken.contains("4th of August")));
        // The following Tuesday is still offered, so a closure closes one day and not the
        // whole diary.
        assert!(slots.iter().any(|s| s.spoken.contains("11th of August")));
    }

    #[test]
    fn nothing_is_offered_sooner_than_the_lead_time_or_past_the_horizon() {
        let mut diary = weekday_diary("Europe/London");
        diary.lead_minutes = 24 * 60;
        diary.horizon_days = 7;
        // Tuesday morning, before the practice opens.
        let now = datetime!(2026-08-04 07:00 UTC);
        let slots = open_slots(&diary, now, &[], 100);
        // Today is inside the lead time, so the first offer is next week.
        assert!(!slots.iter().any(|s| s.spoken.contains("4th of August")));
        assert!(slots.iter().any(|s| s.spoken.contains("11th of August")));
        // And a horizon of a week means nothing the week after.
        assert!(!slots.iter().any(|s| s.spoken.contains("18th of August")));
    }

    /// The hour the clocks go forward does not exist, so nothing in it can be offered.
    ///
    /// In 2026 the United Kingdom goes forward on 29 March: 01:00 becomes 02:00, so no
    /// local clock reads 01:00 to 01:59 that day.
    #[test]
    fn the_hour_the_clocks_go_forward_offers_nothing() {
        let diary = Diary {
            timezone: "Europe/London".into(),
            slot_minutes: 30,
            lead_minutes: 0,
            horizon_days: 1,
            // A Sunday, opening through the missing hour.
            hours: vec![Opening { weekday: 6, opens_minute: 0, closes_minute: 4 * 60 }],
            closures: Vec::new(),
        };
        let now = datetime!(2026-03-29 00:00 UTC) - time::Duration::hours(2);
        let slots = open_slots(&diary, now, &[], 100);
        let spoken: Vec<&str> = slots.iter().map(|s| s.spoken.as_str()).collect();
        // Midnight and half past exist; one o'clock and half past one do not.
        assert!(spoken.iter().any(|s| s.contains("12 o'clock")), "{spoken:?}");
        assert!(!spoken.iter().any(|s| s.contains("1 o'clock")), "{spoken:?}");
        assert!(!spoken.iter().any(|s| s.contains("1:30")), "{spoken:?}");
        // And the ones after the change are there.
        assert!(spoken.iter().any(|s| s.contains("2 o'clock")), "{spoken:?}");
        // Every instant offered is distinct, which is the property the whole rule protects.
        let mut instants: Vec<i64> = slots.iter().map(|s| s.starts_at.unix_timestamp()).collect();
        let before = instants.len();
        instants.sort_unstable();
        instants.dedup();
        assert_eq!(instants.len(), before);
    }

    /// The hour they go back happens twice, and is offered once.
    ///
    /// In 2026 the United Kingdom goes back on 25 October: 02:00 becomes 01:00, so the
    /// local clock reads 01:00 to 01:59 twice. A caller cannot tell the two apart, so the
    /// diary offers only the first.
    #[test]
    fn the_hour_the_clocks_go_back_is_offered_once() {
        let diary = Diary {
            timezone: "Europe/London".into(),
            slot_minutes: 30,
            lead_minutes: 0,
            horizon_days: 1,
            hours: vec![Opening { weekday: 6, opens_minute: 0, closes_minute: 4 * 60 }],
            closures: Vec::new(),
        };
        let now = datetime!(2026-10-24 22:00 UTC);
        let slots = open_slots(&diary, now, &[], 100);
        let ones = slots.iter().filter(|s| s.spoken.contains("1 o'clock")).count();
        let half_ones = slots.iter().filter(|s| s.spoken.contains("1:30")).count();
        assert_eq!(ones, 1, "{:#?}", slots.iter().map(|s| &s.spoken).collect::<Vec<_>>());
        assert_eq!(half_ones, 1);
        // The earlier of the two, so an hour before the same wall clock the second time.
        let one = slots.iter().find(|s| s.spoken.contains("1 o'clock")).unwrap();
        assert_eq!(one.starts_at, datetime!(2026-10-25 00:00 UTC));
    }

    #[test]
    fn a_practice_keeps_its_own_local_hours_wherever_the_host_is() {
        // Nine in the morning in New York is not nine in the morning in London, and the
        // whole point of storing a zone is that the diary knows which nine is meant.
        let london = weekday_diary("Europe/London");
        let new_york = weekday_diary("America/New_York");
        let now = datetime!(2026-08-03 00:00 UTC);
        let first_london = open_slots(&london, now, &[], 1)[0].starts_at;
        let first_new_york = open_slots(&new_york, now, &[], 1)[0].starts_at;
        // Both say "9 o'clock in the morning" to their own caller.
        assert!(open_slots(&london, now, &[], 1)[0].spoken.contains("9 o'clock in the morning"));
        assert!(open_slots(&new_york, now, &[], 1)[0].spoken.contains("9 o'clock in the morning"));
        // And they are five hours apart in August, which is the difference the zone
        // database knows and a fixed offset would not.
        assert_eq!(first_new_york - first_london, time::Duration::hours(5));
    }

    #[test]
    fn a_time_is_said_rather_than_printed() {
        let at = datetime!(2026-08-04 13:10 UTC); // 14:10 in London in August
        let spoken = spoken_at(london(), at);
        assert_eq!(spoken, "Tuesday the 4th of August, 2:10 in the afternoon");
        // Nothing a receptionist would not say aloud.
        for bad in ["Z", "+01", "UTC", "T13", "2026"] {
            assert!(!spoken.contains(bad), "{spoken} contains {bad}");
        }
        // Midday and midnight read as words rather than as twelves.
        assert!(spoken_at(london(), datetime!(2026-08-04 11:00 UTC)).contains("12 o'clock midday"));
        assert!(spoken_at(london(), datetime!(2026-01-04 00:00 UTC)).contains("12 o'clock in the morning"));
        // Ordinals.
        for (day, want) in [(1u8, "1st"), (2, "2nd"), (3, "3rd"), (4, "4th"), (11, "11th"), (21, "21st"), (23, "23rd")] {
            let d = time::Date::from_calendar_date(2026, time::Month::December, day).unwrap();
            let at = d.midnight().assume_utc() + time::Duration::hours(10);
            assert!(spoken_at(london(), at).contains(want), "{day} should read {want}");
        }
    }

    #[test]
    fn only_a_time_the_diary_would_offer_can_be_booked() {
        let diary = weekday_diary("Europe/London");
        // 09:00 local on Tuesday the 4th, which is 08:00 UTC in August.
        let good = datetime!(2026-08-04 08:00 UTC);
        assert!(is_open_slot(&diary, good));
        // Off the grid by ten minutes.
        assert!(!is_open_slot(&diary, good + time::Duration::minutes(10)));
        // During lunch.
        assert!(!is_open_slot(&diary, datetime!(2026-08-04 11:30 UTC)));
        // After closing.
        assert!(!is_open_slot(&diary, datetime!(2026-08-04 16:00 UTC)));
        // On a Wednesday, when the practice does not open at all.
        assert!(!is_open_slot(&diary, datetime!(2026-08-05 08:00 UTC)));
        // On a day it is shut.
        let mut shut = diary.clone();
        shut.closures = vec![time::Date::from_calendar_date(2026, time::Month::August, 4).unwrap()];
        assert!(!is_open_slot(&shut, good));
    }

    #[test]
    fn a_part_of_the_day_is_the_practices_own() {
        let morning = datetime!(2026-08-04 08:00 UTC); // 09:00 in London
        let afternoon = datetime!(2026-08-04 13:00 UTC); // 14:00 in London
        assert!(PartOfDay::Morning.covers(london(), morning));
        assert!(!PartOfDay::Morning.covers(london(), afternoon));
        assert!(PartOfDay::Afternoon.covers(london(), afternoon));
        assert!(PartOfDay::Any.covers(london(), afternoon));
        assert_eq!(PartOfDay::parse(Some("morning")), PartOfDay::Morning);
        assert_eq!(PartOfDay::parse(Some("whenever")), PartOfDay::Any);
        assert_eq!(PartOfDay::parse(None), PartOfDay::Any);
    }

    /// Two things have to agree, and each on its own has to fail.
    #[test]
    fn a_caller_is_identified_by_two_things_or_not_at_all() {
        let recorded = ("Jane Alice Fraser", "+447700900123");
        // The number they are ringing from.
        assert!(caller_matches(recorded.0, recorded.1, "", "+447700900123"));
        // Or the name, however it is written.
        assert!(caller_matches(recorded.0, recorded.1, "Fraser, Jane Alice", "+447700900999"));
        assert!(caller_matches(recorded.0, recorded.1, "Mrs Jane Alice Fraser", ""));
        // Neither: a different person ringing from a different telephone.
        assert!(!caller_matches(recorded.0, recorded.1, "Peter Bell", "+447700900999"));
        // A partial name is not the name. Containment is right for deciding two records
        // are one party; it is not right for letting somebody move an appointment.
        assert!(!caller_matches(recorded.0, recorded.1, "Jane Fraser", "+447700900999"));
        // Nothing offered at all.
        assert!(!caller_matches(recorded.0, recorded.1, "", ""));
        // And a booking with no number recorded cannot be identified by an empty one.
        assert!(!caller_matches(recorded.0, "", "", ""));
    }

    #[test]
    fn an_answer_offers_a_handful_and_stops() {
        let diary = weekday_diary("Europe/London");
        let now = datetime!(2026-08-03 00:00 UTC);
        let slots = open_slots(&diary, now, &[], OFFER_LIMIT);
        assert_eq!(slots.len(), OFFER_LIMIT);
        // In order, because they are read out in order.
        let mut sorted = slots.clone();
        sorted.sort_by_key(|s| s.starts_at);
        assert_eq!(slots, sorted);
    }
}
