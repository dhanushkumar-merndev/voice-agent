//! Appointment availability and booking for the `check_availability` and
//! `book_appointment` tools.
//!
//! Slots come from `config/booking.toml` (opening hours, slot size, open
//! weekdays); taken slots come from `appointments`. Replies are short plain
//! sentences the model can repeat on the call, always naming the weekday so
//! it never has to work out dates itself.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use common::{assets::BookingRules, ids::TenantId};
use sqlx::PgPool;

/// Nearest free times offered when the asked-for time is taken.
const NEAREST: usize = 2;

/// Errors from the booking service.
#[derive(Debug, thiserror::Error)]
pub enum BookingError {
    /// Bad `config/booking.toml`.
    #[error("booking config: {0}")]
    Config(String),
    /// Database failure.
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Parsed booking rules for one tenant.
#[derive(Debug, Clone)]
pub struct Booking {
    tz: Tz,
    open: NaiveTime,
    close: NaiveTime,
    slot: Duration,
    days: Vec<Weekday>,
    max_days_ahead: i64,
    min_notice: Duration,
}

/// Result of a booking attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookOutcome {
    /// Booked; spoken confirmation.
    Booked(String),
    /// Not booked; reason plus alternatives.
    Rejected(String),
}

impl Booking {
    /// Validates the rules.
    pub fn new(rules: &BookingRules) -> Result<Self, BookingError> {
        let tz: Tz = rules
            .timezone
            .parse()
            .map_err(|_| BookingError::Config(format!("unknown timezone {}", rules.timezone)))?;
        let time = |s: &str| {
            NaiveTime::parse_from_str(s, "%H:%M")
                .map_err(|_| BookingError::Config(format!("bad time {s}")))
        };
        let (open, close) = (time(&rules.open)?, time(&rules.close)?);
        if open >= close || rules.slot_mins == 0 {
            return Err(BookingError::Config("open must be before close".into()));
        }
        let days = rules
            .days
            .iter()
            .map(|d| {
                d.parse::<Weekday>()
                    .map_err(|_| BookingError::Config(format!("bad weekday {d}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            tz,
            open,
            close,
            slot: Duration::minutes(i64::from(rules.slot_mins)),
            days,
            max_days_ahead: i64::from(rules.max_days_ahead),
            min_notice: Duration::minutes(i64::from(rules.min_notice_mins)),
        })
    }

    /// Slot length in minutes.
    pub fn slot_mins(&self) -> i64 {
        self.slot.num_minutes()
    }

    /// Today in the business timezone.
    pub fn today(&self, now: DateTime<Utc>) -> NaiveDate {
        now.with_timezone(&self.tz).date_naive()
    }

    /// Prompt line naming today and the next week's dates, so the model
    /// can turn "tomorrow" or "Monday" into a `YYYY-MM-DD` without
    /// calculating.
    pub fn calendar_line(&self, now: DateTime<Utc>) -> String {
        let today = self.today(now);
        let mut s = format!("{} {}", today.weekday_name(), today);
        s.push_str(". Next days: ");
        let next: Vec<String> = (1..=7)
            .filter_map(|i| today.checked_add_signed(Duration::days(i)))
            .map(|d| {
                let label = if d == today.succ_opt().unwrap_or(d) {
                    " (tomorrow)"
                } else {
                    ""
                };
                format!("{} {}{label}", d.weekday_name(), d)
            })
            .collect();
        s.push_str(&next.join(", "));
        s
    }

    /// All slot start times on an open day.
    fn day_slots(&self) -> Vec<NaiveTime> {
        let mut out = Vec::new();
        let mut t = self.open;
        while t < self.close {
            out.push(t);
            let (next, wrapped) = t.overflowing_add_signed(self.slot);
            if wrapped != 0 || next <= t {
                break;
            }
            t = next;
        }
        out
    }

    fn is_open(&self, date: NaiveDate) -> bool {
        self.days.contains(&date.weekday())
    }

    fn at(&self, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
        self.tz
            .from_local_datetime(&date.and_time(time))
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    }

    /// Free slots on `date`, ignoring past and too-soon times.
    async fn free_slots(
        &self,
        db: &PgPool,
        tenant: TenantId,
        date: NaiveDate,
        now: DateTime<Utc>,
    ) -> Result<Vec<NaiveTime>, BookingError> {
        if !self.is_open(date) {
            return Ok(Vec::new());
        }
        let (Some(start), Some(end)) = (
            self.at(date, NaiveTime::MIN),
            date.succ_opt().and_then(|d| self.at(d, NaiveTime::MIN)),
        ) else {
            return Ok(Vec::new());
        };
        let taken: Vec<(DateTime<Utc>,)> = sqlx::query_as(
            "SELECT starts_at FROM appointments WHERE tenant_id = $1 AND status = 'booked' \
             AND starts_at >= $2 AND starts_at < $3",
        )
        .bind(tenant.0)
        .bind(start)
        .bind(end)
        .fetch_all(db)
        .await?;
        let earliest = now + self.min_notice;
        Ok(self
            .day_slots()
            .into_iter()
            .filter(|t| {
                self.at(date, *t)
                    .is_some_and(|at| at >= earliest && !taken.iter().any(|(x,)| *x == at))
            })
            .collect())
    }

    /// First open day after `after` (within the booking window) with free
    /// slots.
    async fn next_free_day(
        &self,
        db: &PgPool,
        tenant: TenantId,
        after: NaiveDate,
        now: DateTime<Utc>,
    ) -> Result<Option<(NaiveDate, Vec<NaiveTime>)>, BookingError> {
        let last = self.today(now) + Duration::days(self.max_days_ahead);
        let mut d = after;
        while let Some(next) = d.succ_opt() {
            d = next;
            if d > last {
                break;
            }
            let free = self.free_slots(db, tenant, d, now).await?;
            if !free.is_empty() {
                return Ok(Some((d, free)));
            }
        }
        Ok(None)
    }

    /// Spoken summary of free times on `date` (`YYYY-MM-DD`). With `time`
    /// (`HH:MM`), says whether that time is free, else the nearest free ones.
    pub async fn check(
        &self,
        db: &PgPool,
        tenant: TenantId,
        date: &str,
        time: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<String, BookingError> {
        let today = self.today(now);
        let Some(date) = parse_date(date) else {
            return Ok(format!(
                "Could not read that date. Use YYYY-MM-DD. Today is {} {today}.",
                today.weekday_name()
            ));
        };
        if date < today {
            return Ok(format!(
                "{} has already passed. Today is {} {today}.",
                day_label(date),
                today.weekday_name()
            ));
        }
        if date > today + Duration::days(self.max_days_ahead) {
            return Ok(format!(
                "Bookings open only {} days ahead. Ask for a nearer date.",
                self.max_days_ahead
            ));
        }
        let free = self.free_slots(db, tenant, date, now).await?;
        if !free.is_empty() {
            if let Some(t) = time.and_then(parse_time) {
                if free.contains(&t) {
                    return Ok(format!(
                        "{} on {} is free.",
                        spoken_time(t),
                        day_label(date)
                    ));
                }
                return Ok(format!(
                    "{} on {} is not available. Nearest free times: {}.",
                    spoken_time(t),
                    day_label(date),
                    nearest(&free, t)
                        .iter()
                        .map(|x| spoken_time(*x))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ));
            }
            return Ok(format!(
                "{} is available. Free start times: {}.",
                day_label(date),
                self.ranges(&free)
            ));
        }
        let why = if self.is_open(date) {
            format!("{} is fully booked", day_label(date))
        } else {
            format!("The clinic is closed on {}", day_label(date))
        };
        Ok(match self.next_free_day(db, tenant, date, now).await? {
            Some((next, slots)) => format!(
                "{why}. Next free day is {}: {}.",
                day_label(next),
                self.ranges(&slots)
            ),
            None => format!(
                "{why}. No free slots in the next {} days.",
                self.max_days_ahead
            ),
        })
    }

    /// Free times as ranges of start times: "10 AM to 1:30 PM, 3 PM,
    /// 4 PM to 5:30 PM (every 30 minutes)", so the model sees the whole day.
    fn ranges(&self, times: &[NaiveTime]) -> String {
        let mut parts = Vec::new();
        let mut i = 0;
        while i < times.len() {
            let mut j = i;
            while j + 1 < times.len() && times[j + 1] - times[j] == self.slot {
                j += 1;
            }
            parts.push(if i == j {
                spoken_time(times[i])
            } else {
                format!("{} to {}", spoken_time(times[i]), spoken_time(times[j]))
            });
            i = j + 1;
        }
        let mut s = parts.join(", ");
        if times.len() > 1 {
            s.push_str(&format!(" (every {} minutes)", self.slot.num_minutes()));
        }
        s
    }

    /// Books `time` on `date` for `name`. Race-safe: the unique slot index
    /// decides between two callers.
    pub async fn book(
        &self,
        db: &PgPool,
        tenant: TenantId,
        call: Option<uuid::Uuid>,
        (date, time, name): (&str, &str, &str),
        now: DateTime<Utc>,
    ) -> Result<BookOutcome, BookingError> {
        let (Some(d), Some(t)) = (parse_date(date), parse_time(time)) else {
            return Ok(BookOutcome::Rejected(
                "Could not read the date or time. Use YYYY-MM-DD and HH:MM.".into(),
            ));
        };
        let name = clean_spoken_name(name);
        let name = name.as_str();
        if name.is_empty() {
            return Ok(BookOutcome::Rejected(
                "Ask for the caller's name first.".into(),
            ));
        }
        if has_indian_script(name) {
            // Staff read the bookings in English; the model can romanize the name itself.
            return Ok(BookOutcome::Rejected(
                "Not booked: write the name in English letters (Lakshmi, not லட்சுமி) and call book_appointment again. Don't mention this to the caller.".into(),
            ));
        }
        let free = self.free_slots(db, tenant, d, now).await?;
        if !free.contains(&t) {
            return Ok(BookOutcome::Rejected(format!(
                "Not booked. {}",
                self.check(db, tenant, date, Some(time), now).await?
            )));
        }
        let Some(starts_at) = self.at(d, t) else {
            return Ok(BookOutcome::Rejected("That time does not exist.".into()));
        };
        let inserted = sqlx::query(
            "INSERT INTO appointments (id, tenant_id, call_id, starts_at, duration_mins, name) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (tenant_id, starts_at) WHERE status = 'booked' DO NOTHING",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(tenant.0)
        .bind(call)
        .bind(starts_at)
        .bind(self.slot.num_minutes() as i32)
        .bind(name.chars().take(100).collect::<String>())
        .execute(db)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(BookOutcome::Rejected(format!(
                "Someone just took that slot. {}",
                self.check(db, tenant, date, Some(time), now).await?
            )));
        }
        metrics::counter!("voice_appointments_booked_total").increment(1);
        Ok(BookOutcome::Booked(format!(
            "Booked for {name} on {} at {}.",
            day_label(d),
            spoken_time(t)
        )))
    }
}

trait WeekdayName {
    fn weekday_name(&self) -> &'static str;
}

impl WeekdayName for NaiveDate {
    fn weekday_name(&self) -> &'static str {
        match self.weekday() {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        }
    }
}

/// "Thursday 29 October".
fn day_label(d: NaiveDate) -> String {
    format!("{} {}", d.weekday_name(), d.format("%-d %B"))
}

/// "10 AM", "10:30 AM", "12 PM", "4:30 PM".
fn spoken_time(t: NaiveTime) -> String {
    if t.format("%M").to_string() == "00" {
        t.format("%-I %p").to_string()
    } else {
        t.format("%-I:%M %p").to_string()
    }
}

/// The free times closest to `wanted`, in time order.
fn nearest(free: &[NaiveTime], wanted: NaiveTime) -> Vec<NaiveTime> {
    let mut by_distance = free.to_vec();
    by_distance.sort_by_key(|t| (*t - wanted).num_minutes().abs());
    by_distance.truncate(NEAREST);
    by_distance.sort();
    by_distance
}

/// Whether `text` has Devanagari, Bengali, Gurmukhi, Gujarati, Odia, Tamil,
/// Telugu, Kannada or Malayalam letters.
pub fn has_indian_script(text: &str) -> bool {
    text.chars().any(|c| ('\u{0900}'..='\u{0D7F}').contains(&c))
}

/// Drops letter-by-letter spelling that speech-to-text writes out:
/// "Dhanush D H A N U S H" → "Dhanush", "D H A N U S H" → "Dhanush".
/// Initials that spell nothing nearby ("A R Rahman") are kept.
pub fn clean_spoken_name(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let single = |w: &str| {
        let w = w.trim_matches(|c: char| c == '.' || c == ',');
        w.chars().count() == 1 && w.chars().all(char::is_alphabetic)
    };
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let mut j = i;
        while j < words.len() && single(words[j]) {
            j += 1;
        }
        if j - i >= 2 {
            let spelled: String = words[i..j]
                .iter()
                .flat_map(|w| w.chars().filter(|c| c.is_alphabetic()))
                .collect::<String>()
                .to_lowercase();
            let near = |w: Option<&&str>| w.is_some_and(|w| w.to_lowercase() == spelled);
            if near(i.checked_sub(1).and_then(|k| words.get(k))) || near(words.get(j)) {
                // Spelling of a neighbouring word: drop it.
            } else if i == 0 && j == words.len() {
                let mut c = spelled.chars();
                out.push(
                    c.next()
                        .map_or(String::new(), |f| f.to_uppercase().chain(c).collect()),
                );
            } else {
                out.extend(words[i..j].iter().map(|w| (*w).to_owned()));
            }
            i = j;
        } else {
            out.push(words[i].to_owned());
            i += 1;
        }
    }
    out.join(" ")
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Accepts "14:30", "9:00", "14", "2 PM", "2:30pm".
fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_ascii_uppercase().replace('.', "");
    let (body, pm) = if let Some(b) = s.strip_suffix("PM") {
        (b.trim().to_owned(), Some(true))
    } else if let Some(b) = s.strip_suffix("AM") {
        (b.trim().to_owned(), Some(false))
    } else {
        (s, None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn booking() -> Booking {
        Booking::new(&BookingRules::default()).unwrap()
    }

    #[test]
    fn slots_cover_opening_hours() {
        let slots = booking().day_slots();
        assert_eq!(slots.len(), 16);
        assert_eq!(spoken_time(slots[0]), "10 AM");
        assert_eq!(spoken_time(slots[15]), "5:30 PM");
    }

    #[test]
    fn times_parse_in_common_forms() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0);
        assert_eq!(parse_time("14:30"), t(14, 30));
        assert_eq!(parse_time("2:30 pm"), t(14, 30));
        assert_eq!(parse_time("11 AM"), t(11, 0));
        assert_eq!(parse_time("12 PM"), t(12, 0));
        assert_eq!(parse_time("10"), t(10, 0));
        assert_eq!(parse_time("soon"), None);
    }

    #[test]
    fn calendar_line_marks_tomorrow() {
        // 2026-10-28 12:00 IST.
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 6, 30, 0).unwrap();
        let line = booking().calendar_line(now);
        assert!(line.starts_with("Wednesday 2026-10-28"));
        assert!(line.contains("Thursday 2026-10-29 (tomorrow)"));
        assert!(line.contains("Wednesday 2026-11-04"));
    }

    #[test]
    fn free_times_are_given_as_ranges() {
        let b = booking();
        let mut slots = b.day_slots();
        assert_eq!(b.ranges(&slots), "10 AM to 5:30 PM (every 30 minutes)");
        // 2 PM and 3:30 PM taken.
        slots.retain(|t| *t != NaiveTime::from_hms_opt(14, 0, 0).unwrap());
        slots.retain(|t| *t != NaiveTime::from_hms_opt(15, 30, 0).unwrap());
        assert_eq!(
            b.ranges(&slots),
            "10 AM to 1:30 PM, 2:30 PM to 3 PM, 4 PM to 5:30 PM (every 30 minutes)"
        );
    }

    #[test]
    fn nearest_free_times_surround_the_wanted_one() {
        let slots = booking().day_slots();
        let free: Vec<_> = slots
            .into_iter()
            .filter(|t| t.format("%H").to_string() != "14")
            .collect();
        let got = nearest(&free, NaiveTime::from_hms_opt(14, 30, 0).unwrap());
        let got: Vec<_> = got.into_iter().map(spoken_time).collect();
        assert_eq!(got, ["1:30 PM", "3 PM"]);
    }

    #[test]
    fn names_in_indian_scripts_are_detected() {
        assert!(has_indian_script("லட்சுமி"));
        assert!(has_indian_script("Ravi ಕುಮಾರ್"));
        assert!(!has_indian_script("Lakshmi"));
        assert!(!has_indian_script("José"));
    }

    #[test]
    fn spelled_names_are_cleaned() {
        assert_eq!(clean_spoken_name("Dhanush D H A N U S H"), "Dhanush");
        assert_eq!(clean_spoken_name("D H A N U S H"), "Dhanush");
        assert_eq!(clean_spoken_name("A R Rahman"), "A R Rahman");
        assert_eq!(clean_spoken_name("  Gokul   Nath "), "Gokul Nath");
    }
}
