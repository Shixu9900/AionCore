//! Record-value normalization: flight hours, cycles, dates.

use chrono::NaiveDate;

/// Parse a flight-hours reading. `HH:MM` (with optional thousands commas) is
/// converted to decimal hours; anything else is read as a decimal number.
pub fn parse_hours(raw: &str) -> Option<f64> {
    let cleaned = strip_number(raw);
    if let Some((h, m)) = cleaned.split_once(':') {
        let hours: f64 = h.parse().ok()?;
        let minutes: f64 = m.parse().ok()?;
        if !(0.0..60.0).contains(&minutes) {
            return None;
        }
        return Some(hours + minutes / 60.0);
    }
    cleaned.parse().ok()
}

pub fn parse_cycles(raw: &str) -> Option<f64> {
    strip_number(raw).parse().ok()
}

fn strip_number(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ':' || *c == '-')
        .collect()
}

pub fn format_hours(hours: f64) -> String {
    let total_minutes = (hours * 60.0).round() as i64;
    let h = total_minutes / 60;
    let m = (total_minutes % 60).abs();
    format!("{}:{:02}", group_thousands(h), m)
}

pub fn format_cycles(cycles: f64) -> String {
    group_thousands(cycles.round() as i64)
}

fn group_thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

pub fn parse_date(raw: &str) -> Option<NaiveDate> {
    let trimmed = raw.trim();
    let head = trimmed.get(..10).unwrap_or(trimmed);
    for fmt in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(head, fmt) {
            return Some(d);
        }
    }
    for fmt in ["%m/%d/%Y", "%d %b %Y", "%d-%b-%Y", "%d %B %Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(trimmed, fmt) {
            return Some(d);
        }
    }
    None
}

/// ISO date when parseable, the trimmed input otherwise.
pub fn normalize_date(raw: &str) -> String {
    parse_date(raw)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| raw.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_accept_hhmm_and_decimal() {
        assert!((parse_hours("4,812:50").unwrap() - 4812.8333).abs() < 1e-3);
        assert_eq!(parse_hours("13,986.7"), Some(13986.7));
        assert_eq!(parse_hours("12:75"), None);
        assert_eq!(format_hours(29189.2667), "29,189:16");
    }

    #[test]
    fn dates_normalize() {
        assert_eq!(normalize_date("3/18/2010"), "2010-03-18");
        assert_eq!(normalize_date("2015-07-11"), "2015-07-11");
        assert_eq!(normalize_date("11 Jul 2015"), "2015-07-11");
    }
}
