//! How old a generation is, and whether it is due for rotation (`max_age`, and the expiry a
//! provider reports)

use chrono::{DateTime, SubsecRound, Utc};

/// The time now, to the second (what generations record)
pub fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(0)
}

/// A length of time, written as a number and a unit: `45s`, `30m`, `12h`, `90d`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    seconds: u64,
}

const UNITS: [(char, u64); 4] = [('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)];

impl Span {
    pub const fn from_secs(seconds: u64) -> Self {
        Self { seconds }
    }

    pub const fn from_days(days: u64) -> Self {
        Self {
            seconds: days * 86_400,
        }
    }

    pub fn as_secs(self) -> u64 {
        self.seconds
    }

    pub fn is_zero(self) -> bool {
        self.seconds == 0
    }

    fn delta(self) -> chrono::TimeDelta {
        chrono::TimeDelta::seconds(i64::try_from(self.seconds).unwrap_or(i64::MAX / 1_000))
    }
}

impl std::str::FromStr for Span {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let bad = || {
            anyhow::anyhow!("{s:?} is not a length of time (a number and s, m, h or d, like 90d)")
        };
        let unit = s.chars().last().ok_or_else(bad)?;
        let (_, per) = UNITS.iter().find(|(u, _)| *u == unit).ok_or_else(bad)?;
        let number = &s[..s.len() - 1];
        anyhow::ensure!(
            !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
            bad()
        );
        let n: u64 = number.parse().map_err(|_| bad())?;
        let seconds = n
            .checked_mul(*per)
            .filter(|s| *s <= 100 * 365 * 86_400)
            .ok_or_else(|| anyhow::anyhow!("{s:?} is too long"))?;
        Ok(Self { seconds })
    }
}

impl std::fmt::Display for Span {
    /// In the largest unit that divides it (`90d`, `36h`)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (unit, per) = UNITS
            .iter()
            .find(|(_, per)| self.seconds.is_multiple_of(*per) && self.seconds >= *per)
            .copied()
            .unwrap_or(('s', 1));
        write!(f, "{}{unit}", self.seconds / per)
    }
}

impl serde::Serialize for Span {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a generation stands. The order is how bad it is
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Ok,
    /// Past `max_age` minus `warn_before`, or expiring within `warn_before`
    Due,
    /// Past `max_age`
    RotateNow,
    /// Past the expiry the provider reported
    Expired,
}

impl Standing {
    /// What `status` puts after the line (empty when nothing is due)
    pub fn mark(self) -> &'static str {
        match self {
            Standing::Ok => "",
            Standing::Due => ": DUE",
            Standing::RotateNow => ": ROTATE NOW",
            Standing::Expired => ": EXPIRED",
        }
    }

    /// Makes `check --stale` fail
    pub fn is_problem(self) -> bool {
        self >= Standing::RotateNow
    }
}

/// The latest generation of a resource, as far as age goes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Age {
    pub created: DateTime<Utc>,
    pub expires: Option<DateTime<Utc>>,
    pub max_age: Option<Span>,
}

impl Age {
    pub fn standing(&self, now: DateTime<Utc>, warn_before: Span) -> Standing {
        let age = now - self.created;
        let warn = warn_before.delta();
        if self.expires.is_some_and(|e| now >= e) {
            return Standing::Expired;
        }
        if self.max_age.is_some_and(|m| age >= m.delta()) {
            return Standing::RotateNow;
        }
        let due_by_age = self.max_age.is_some_and(|m| age >= m.delta() - warn);
        let due_by_expiry = self.expires.is_some_and(|e| e - now <= warn);
        if due_by_age || due_by_expiry {
            Standing::Due
        } else {
            Standing::Ok
        }
    }

    /// `97 days old (max 90d), expires in 4 days`
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let mut s = format!("{} old", length(now - self.created));
        if let Some(m) = self.max_age {
            s.push_str(&format!(" (max {m})"));
        }
        match self.expires {
            Some(e) if e > now => s.push_str(&format!(", expires in {}", length(e - now))),
            Some(e) => s.push_str(&format!(", expired {} ago", length(now - e))),
            None => {}
        }
        s
    }
}

/// A length of time for people: `97 days`, `1 day`, `5 hours`, `less than a minute`
pub fn length(d: chrono::TimeDelta) -> String {
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit}")
        } else {
            format!("{n} {unit}s")
        }
    };
    if d.num_days() >= 1 {
        plural(d.num_days(), "day")
    } else if d.num_hours() >= 1 {
        plural(d.num_hours(), "hour")
    } else if d.num_minutes() >= 1 {
        plural(d.num_minutes(), "minute")
    } else {
        "less than a minute".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_lengths() {
        let span = |s: &str| s.parse::<Span>();
        assert_eq!(span("90d").unwrap().as_secs(), 90 * 86_400);
        assert_eq!(span("12h").unwrap().as_secs(), 12 * 3_600);
        assert_eq!(span("30m").unwrap().as_secs(), 1_800);
        assert_eq!(span("45s").unwrap().as_secs(), 45);
        assert_eq!(span("0d").unwrap().as_secs(), 0);
        for bad in ["", "d", "90", "90 d", "-1d", "1.5d", "90w", "1e3d"] {
            assert!(span(bad).is_err(), "{bad:?}");
        }
        assert!(span("99999999999d").is_err());
        assert_eq!(span("90d").unwrap().to_string(), "90d");
        assert_eq!(span("36h").unwrap().to_string(), "36h");
        assert_eq!(span("48h").unwrap().to_string(), "2d");
        assert_eq!(span("90s").unwrap().to_string(), "90s");
        assert_eq!(Span::from_secs(0).to_string(), "0s");
    }

    #[test]
    fn decides_what_is_due() {
        let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let warn = Span::from_days(14);
        let age = Age {
            created: t("2026-01-01T00:00:00Z"),
            expires: None,
            max_age: Some(Span::from_days(90)),
        };
        assert_eq!(age.standing(t("2026-03-01T00:00:00Z"), warn), Standing::Ok);
        // 76 days: within 14 days of 90
        assert_eq!(age.standing(t("2026-03-18T00:00:00Z"), warn), Standing::Due);
        assert_eq!(
            age.standing(t("2026-04-01T00:00:00Z"), warn),
            Standing::RotateNow
        );
        assert_eq!(
            age.describe(t("2026-04-08T00:00:00Z")),
            "97 days old (max 90d)"
        );

        let expiring = Age {
            created: t("2026-01-01T00:00:00Z"),
            expires: Some(t("2026-01-08T00:00:00Z")),
            max_age: None,
        };
        assert_eq!(
            expiring.standing(t("2026-01-04T00:00:00Z"), warn),
            Standing::Due
        );
        assert_eq!(
            expiring.standing(t("2026-01-04T00:00:00Z"), Span::from_days(1)),
            Standing::Ok
        );
        assert_eq!(
            expiring.standing(t("2026-01-08T00:00:00Z"), warn),
            Standing::Expired
        );
        assert_eq!(
            expiring.describe(t("2026-01-04T00:00:00Z")),
            "3 days old, expires in 4 days"
        );
        assert_eq!(
            expiring.describe(t("2026-01-10T01:00:00Z")),
            "9 days old, expired 2 days ago"
        );

        let none = Age {
            created: t("2026-01-01T00:00:00Z"),
            expires: None,
            max_age: None,
        };
        assert_eq!(none.standing(t("2036-01-01T00:00:00Z"), warn), Standing::Ok);
        assert_eq!(
            none.describe(t("2026-01-01T00:00:30Z")),
            "less than a minute old"
        );
        assert_eq!(none.describe(t("2026-01-02T00:00:00Z")), "1 day old");
    }
}
