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

//! Cron-expression schedule helpers for automations.
//! Pure: parse a cron expression, compute the next occurrence, and expand a
//! window for the calendar view. Cron is parsed by the `cron` crate (chrono
//! datetimes); results are converted to `time::OffsetDateTime` to match the rest
//! of the codebase.

use std::str::FromStr;

use cron::Schedule;
use time::OffsetDateTime;

use crate::error::{AppError, Result};

fn parse(schedule: &str) -> Result<Schedule> {
    Schedule::from_str(schedule)
        .map_err(|e| AppError::Validation(format!("invalid cron schedule: {e}")))
}

fn to_chrono(t: OffsetDateTime) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(t.unix_timestamp(), 0).unwrap_or_else(chrono::Utc::now)
}

fn from_chrono(dt: chrono::DateTime<chrono::Utc>) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(dt.timestamp()).ok()
}

/// Validate a cron expression (for create/update).
pub fn validate(schedule: &str) -> Result<()> {
    parse(schedule).map(|_| ())
}

/// Next occurrence strictly after `after`, or `None` if the schedule has no
/// future occurrence.
pub fn next_after(schedule: &str, after: OffsetDateTime) -> Result<Option<OffsetDateTime>> {
    let s = parse(schedule)?;
    Ok(s.after(&to_chrono(after)).next().and_then(from_chrono))
}

/// Occurrences in `(from, to]`, capped at `cap` — the calendar expansion.
pub fn occurrences_between(
    schedule: &str,
    from: OffsetDateTime,
    to: OffsetDateTime,
    cap: usize,
) -> Result<Vec<OffsetDateTime>> {
    let s = parse(schedule)?;
    let to_ts = to.unix_timestamp();
    let out = s
        .after(&to_chrono(from))
        .take(cap)
        .take_while(|dt| dt.timestamp() <= to_ts)
        .filter_map(from_chrono)
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, Time, Weekday};

    fn at(y: i32, m: Month, d: u8) -> OffsetDateTime {
        OffsetDateTime::new_utc(Date::from_calendar_date(y, m, d).unwrap(), Time::MIDNIGHT)
    }

    #[test]
    fn next_after_weekly_monday_0900() {
        // 2026-01-01 is a Thursday; next "Mon 09:00" is 2026-01-05 09:00.
        let next = next_after("0 0 9 * * Mon", at(2026, Month::January, 1)).unwrap().unwrap();
        assert_eq!(next.weekday(), Weekday::Monday);
        assert_eq!(next.hour(), 9);
        assert!(next > at(2026, Month::January, 1));
    }

    #[test]
    fn daily_window_yields_seven() {
        let from = at(2026, Month::January, 1);
        let to = at(2026, Month::January, 8);
        let occ = occurrences_between("0 0 9 * * *", from, to, 100).unwrap();
        assert_eq!(occ.len(), 7, "one 09:00 per day across 7 days");
        assert!(occ.iter().all(|t| t.hour() == 9));
    }

    #[test]
    fn invalid_cron_rejected() {
        assert!(validate("not a cron").is_err());
        assert!(validate("0 0 9 * * Mon").is_ok());
    }
}
