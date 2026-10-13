use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

macro_rules! str_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = anyhow::Error;
            fn from_str(s: &str) -> Result<Self> {
                let s = s.trim().to_lowercase();
                $name::ALL
                    .iter()
                    .copied()
                    .find(|v| v.as_str() == s)
                    .ok_or_else(|| {
                        let valid: Vec<&str> = $name::ALL.iter().map(|v| v.as_str()).collect();
                        anyhow!("invalid {} '{}' (valid: {})", stringify!($name).to_lowercase(), s, valid.join(", "))
                    })
            }
        }
    };
}

str_enum!(Kind {
    Paper => "paper",
    Software => "software",
    Book => "book",
    Course => "course",
    Grant => "grant",
    Consulting => "consulting",
    Website => "website",
    Dataset => "dataset",
    Collab => "collab",
    Talk => "talk",
    Other => "other",
});

str_enum!(Status {
    Idea => "idea",
    Planned => "planned",
    Active => "active",
    Paused => "paused",
    Submitted => "submitted",
    Revision => "revision",
    Accepted => "accepted",
    Published => "published",
    Archived => "archived",
});

impl Status {
    /// Finished or dead projects, hidden from default listings.
    pub fn is_closed(&self) -> bool {
        matches!(self, Status::Published | Status::Archived)
    }
}

str_enum!(Priority {
    High => "high",
    Normal => "normal",
    Low => "low",
});

str_enum!(Outcome {
    Submitted => "submitted",
    UnderReview => "under-review",
    Revision => "revision",
    Rejected => "rejected",
    Accepted => "accepted",
    Published => "published",
    Withdrawn => "withdrawn",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deadline {
    pub date: NaiveDate,
    pub what: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub venue: String,
    pub date: NaiveDate,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub rel: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub kind: Kind,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Short portfolio or programme code, e.g. AMRPRED
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaborators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created: NaiveDate,
    pub updated: NaiveDate,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deadlines: Vec<Deadline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submissions: Vec<Submission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

/// Lowercase ASCII slug: runs of non-alphanumerics collapse to one '-'.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

pub fn parse_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| anyhow!("invalid date '{}' (expected YYYY-MM-DD)", s))
}

/// Parses "30d", "2w", "3m" (30-day months) or a bare number of days.
pub fn parse_span_days(s: &str) -> Result<i64> {
    let s = s.trim().to_lowercase();
    let (num, mult) = match s.chars().last() {
        Some('d') => (&s[..s.len() - 1], 1),
        Some('w') => (&s[..s.len() - 1], 7),
        Some('m') => (&s[..s.len() - 1], 30),
        _ => (s.as_str(), 1),
    };
    let n: i64 = num
        .parse()
        .map_err(|_| anyhow!("invalid span '{}' (e.g. 30d, 2w, 3m)", s))?;
    Ok(n * mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_collapses_punctuation() {
        assert_eq!(slugify("Reply letter: J. Imag. Stat!"), "reply-letter-j-imag-stat");
        assert_eq!(slugify("  aurora  "), "aurora");
    }

    #[test]
    fn spans() {
        assert_eq!(parse_span_days("30d").unwrap(), 30);
        assert_eq!(parse_span_days("2w").unwrap(), 14);
        assert_eq!(parse_span_days("3m").unwrap(), 90);
        assert_eq!(parse_span_days("10").unwrap(), 10);
        assert!(parse_span_days("abc").is_err());
    }

    #[test]
    fn enums_round_trip() {
        assert_eq!("Under-Review".parse::<Outcome>().unwrap(), Outcome::UnderReview);
        assert!("bogus".parse::<Kind>().is_err());
        for k in Kind::ALL {
            assert_eq!(k.as_str().parse::<Kind>().unwrap(), *k);
        }
    }
}
