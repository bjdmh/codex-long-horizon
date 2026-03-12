use chrono::DateTime;
use chrono::Duration;
use chrono::Local;
use chrono::LocalResult;
use chrono::NaiveDateTime;
use chrono::NaiveTime;
use chrono::TimeZone;
use chrono::Utc;

pub(super) fn resolve_future_time(
    run_at: Option<&str>,
    delay_seconds: Option<u64>,
) -> Result<DateTime<Utc>, String> {
    match (run_at, delay_seconds) {
        (Some(run_at), None) => {
            let resolved = parse_run_at(run_at)?;
            if resolved <= Utc::now() {
                return Err("scheduled time must be in the future".to_string());
            }
            Ok(resolved)
        }
        (None, Some(delay_seconds)) => {
            if delay_seconds == 0 {
                return Err("delay_seconds must be greater than zero".to_string());
            }
            Ok(Utc::now()
                + Duration::seconds(
                    i64::try_from(delay_seconds)
                        .map_err(|_| "delay_seconds is too large".to_string())?,
                ))
        }
        (Some(_), Some(_)) => Err("provide either run_at or delay_seconds, not both".to_string()),
        (None, None) => Err("provide either run_at or delay_seconds".to_string()),
    }
}

fn parse_run_at(value: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("run_at must not be empty".to_string());
    }

    if let Ok(epoch_seconds) = trimmed.parse::<i64>() {
        return DateTime::from_timestamp(epoch_seconds, 0)
            .map(|value| value.with_timezone(&Utc))
            .ok_or_else(|| format!("invalid unix timestamp: {epoch_seconds}"));
    }

    if let Ok(rfc3339) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(rfc3339.with_timezone(&Utc));
    }

    if let Some(time) = trimmed.strip_prefix("today ") {
        let now = Local::now();
        let naive_time = parse_clock_time(time)?;
        let naive = now.date_naive().and_time(naive_time);
        return local_naive_to_utc(naive);
    }

    if let Some(time) = trimmed.strip_prefix("tomorrow ") {
        let now = Local::now();
        let naive_time = parse_clock_time(time)?;
        let tomorrow = now.date_naive() + Duration::days(1);
        let naive = tomorrow.and_time(naive_time);
        return local_naive_to_utc(naive);
    }

    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M") {
        return local_naive_to_utc(naive);
    }

    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M") {
        return local_naive_to_utc(naive);
    }

    if let Ok(naive_time) = parse_clock_time(trimmed) {
        let now = Local::now();
        let mut date = now.date_naive();
        if naive_time <= now.time() {
            date += Duration::days(1);
        }
        return local_naive_to_utc(date.and_time(naive_time));
    }

    Err(
        "unsupported run_at format; use RFC3339, unix seconds, YYYY-MM-DD HH:MM, today HH:MM, tomorrow HH:MM, or HH:MM"
            .to_string(),
    )
}

fn parse_clock_time(value: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M")
        .map_err(|_| format!("invalid local clock time: {}", value.trim()))
}

fn local_naive_to_utc(value: NaiveDateTime) -> Result<DateTime<Utc>, String> {
    match Local.from_local_datetime(&value) {
        LocalResult::Single(datetime) => Ok(datetime.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, _) => Ok(first.with_timezone(&Utc)),
        LocalResult::None => Err(format!(
            "local time {value} does not exist in the current timezone"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_timestamp() {
        let parsed =
            resolve_future_time(Some("2099-01-02T03:04:05Z"), None).expect("parse rfc3339");
        assert_eq!(parsed.to_rfc3339(), "2099-01-02T03:04:05+00:00");
    }

    #[test]
    fn rejects_missing_time_inputs() {
        let error = resolve_future_time(None, None).expect_err("missing input should fail");
        assert_eq!(error, "provide either run_at or delay_seconds");
    }
}
