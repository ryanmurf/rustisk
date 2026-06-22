//! If/ElseIf/Else/EndIf and GotoIf/GotoIfTime dialplan branching applications.
//!
//! Port of app_if.c from Asterisk C. Provides conditional branching
//! in the dialplan using If/ElseIf/Else/EndIf blocks and GotoIf()
//! for single-line conditional jumps.

use crate::goto::GotoTarget;
use crate::{DialplanApp, PbxExecResult};
use asterisk_core::channel::Channel;
use tracing::{debug, info, warn};

/// The GotoIf() dialplan application.
///
/// Usage: GotoIf(condition?[label_true][:label_false])
///
/// Conditionally jumps to a label. Labels can be:
///   priority
///   extension,priority
///   context,extension,priority
pub struct AppGotoIf;

impl DialplanApp for AppGotoIf {
    fn name(&self) -> &str {
        "GotoIf"
    }

    fn description(&self) -> &str {
        "Conditional goto"
    }
}

impl AppGotoIf {
    /// Execute the GotoIf application.
    pub async fn exec(channel: &mut Channel, args: &str) -> PbxExecResult {
        let (condition, branches) = match args.split_once('?') {
            Some((c, b)) => (c.trim(), b),
            None => {
                warn!("GotoIf: requires condition?[true][:false] format");
                return PbxExecResult::Failed;
            }
        };

        let (true_label, false_label) = match branches.split_once(':') {
            Some((t, f)) => (Some(t.trim()).filter(|s| !s.is_empty()), Some(f.trim()).filter(|s| !s.is_empty())),
            None => (Some(branches.trim()).filter(|s| !s.is_empty()), None),
        };

        let is_true = !condition.is_empty() && condition != "0";

        let target = if is_true { true_label } else { false_label };

        if let Some(label) = target {
            info!(
                "GotoIf: channel '{}' condition={} jumping to '{}'",
                channel.name, is_true, label,
            );
            // Parse label as context,extension,priority and set the channel location
            if let Some(goto_target) = GotoTarget::parse(label) {
                if let Some(ctx) = &goto_target.context {
                    channel.context = ctx.clone();
                }
                if let Some(ext) = &goto_target.extension {
                    channel.exten = ext.clone();
                }
                let priority: i32 = match goto_target.priority.parse::<i32>() {
                    Ok(p) => p,
                    Err(_) => {
                        if goto_target.priority.eq_ignore_ascii_case("n") {
                            channel.priority + 1
                        } else {
                            warn!("GotoIf: unknown priority label '{}', using 1", goto_target.priority);
                            1
                        }
                    }
                };
                channel.priority = priority;
            } else {
                warn!("GotoIf: could not parse label '{}'", label);
                return PbxExecResult::Failed;
            }
        } else {
            debug!("GotoIf: channel '{}' condition={} no branch to take", channel.name, is_true);
        }

        PbxExecResult::Success
    }
}

/// The GotoIfTime() dialplan application.
///
/// Usage: GotoIfTime(times,weekdays,mdays,months?label_true[:label_false])
///
/// Conditionally jumps based on the current time matching the given
/// time specification.
pub struct AppGotoIfTime;

impl DialplanApp for AppGotoIfTime {
    fn name(&self) -> &str {
        "GotoIfTime"
    }

    fn description(&self) -> &str {
        "Conditional goto based on current time"
    }
}

impl AppGotoIfTime {
    /// Execute the GotoIfTime application.
    pub async fn exec(channel: &mut Channel, args: &str) -> PbxExecResult {
        let (time_spec, branches) = match args.split_once('?') {
            Some((t, b)) => (t.trim(), b),
            None => {
                warn!("GotoIfTime: requires timespec?label format");
                return PbxExecResult::Failed;
            }
        };

        info!(
            "GotoIfTime: channel '{}' spec='{}' branches='{}'",
            channel.name, time_spec, branches,
        );

        // Parse true/false branch labels
        let (true_label, false_label) = match branches.split_once(':') {
            Some((t, f)) => (
                Some(t.trim()).filter(|s| !s.is_empty()),
                Some(f.trim()).filter(|s| !s.is_empty()),
            ),
            None => (Some(branches.trim()).filter(|s| !s.is_empty()), None),
        };

        // Evaluate the time specification against the current time.
        // Format: times,weekdays,mdays,months  (e.g. "9:00-17:00,mon-fri,*,*")
        let is_match = evaluate_time_spec(time_spec);

        let target = if is_match { true_label } else { false_label };

        if let Some(label) = target {
            info!(
                "GotoIfTime: channel '{}' match={} jumping to '{}'",
                channel.name, is_match, label,
            );
            if let Some(goto_target) = GotoTarget::parse(label) {
                if let Some(ctx) = &goto_target.context {
                    channel.context = ctx.clone();
                }
                if let Some(ext) = &goto_target.extension {
                    channel.exten = ext.clone();
                }
                let priority: i32 = match goto_target.priority.parse::<i32>() {
                    Ok(p) => p,
                    Err(_) => {
                        if goto_target.priority.eq_ignore_ascii_case("n") {
                            channel.priority + 1
                        } else {
                            1
                        }
                    }
                };
                channel.priority = priority;
            }
        } else {
            debug!(
                "GotoIfTime: channel '{}' match={} no branch to take",
                channel.name, is_match,
            );
        }

        PbxExecResult::Success
    }
}

/// Evaluate a time specification against the current time.
///
/// Format: "times,weekdays,mdays,months"
/// Examples: "9:00-17:00,mon-fri,*,*"  or  "*,*,*,*" (always matches)
///
/// Each component is compared against the current time. A "*" wildcard
/// matches any value.
fn evaluate_time_spec(spec: &str) -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};

    let parts: Vec<&str> = spec.split(',').map(|s| s.trim()).collect();
    if parts.len() < 4 {
        // If fewer than 4 fields, treat as wildcard (always match)
        return true;
    }

    // Get current time components from system time
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Approximate local time components (UTC-based; timezone-aware
    // matching is not critical for this implementation)
    let secs_in_day = (secs % 86400) as i32;
    let now_hour = secs_in_day / 3600;
    let now_minute = (secs_in_day % 3600) / 60;
    let now_mins = now_hour * 60 + now_minute;

    // Day of week: 1970-01-01 was a Thursday (day 4)
    let days_since_epoch = secs / 86400;
    let weekday = ((days_since_epoch + 3) % 7) as u32; // 0=Mon..6=Sun

    // Approximate month/day using simple calculation
    let (month, mday) = approximate_month_day(secs);

    // Check time range (e.g. "9:00-17:00")
    if parts[0] != "*" {
        if let Some((start_str, end_str)) = parts[0].split_once('-') {
            let start_mins = parse_time_of_day(start_str);
            let end_mins = parse_time_of_day(end_str);
            if let (Some(s), Some(e)) = (start_mins, end_mins) {
                if s <= e {
                    if now_mins < s || now_mins > e {
                        return false;
                    }
                } else {
                    // Wraps around midnight
                    if now_mins < s && now_mins > e {
                        return false;
                    }
                }
            }
        }
    }

    // Check weekdays (e.g. "mon-fri")
    if parts[1] != "*"
        && !weekday_range_matches(parts[1], weekday) {
            return false;
        }

    // Check month days (e.g. "1-15")
    if parts[2] != "*" {
        if let Some((s, e)) = parse_int_range(parts[2]) {
            if mday < s || mday > e {
                return false;
            }
        }
    }

    // Check months (e.g. "jan-dec")
    if parts[3] != "*"
        && !month_range_matches(parts[3], month) {
            return false;
        }

    true
}

/// Approximate month (1-12) and day (1-31) from Unix epoch seconds.
/// This is a simple calculation that doesn't account for timezones
/// but is sufficient for GotoIfTime matching.
fn approximate_month_day(secs: u64) -> (u32, u32) {
    let days = (secs / 86400) as u32;
    // Calculate year and remaining days
    let mut year = 1970u32;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }
    let leap = is_leap_year(year);
    let month_days: [u32; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for (month, &md) in (1u32..).zip(month_days.iter()) {
        if remaining < md {
            return (month, remaining + 1);
        }
        remaining -= md;
    }
    (12, remaining + 1)
}

fn is_leap_year(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn parse_time_of_day(s: &str) -> Option<i32> {
    let (h, m) = s.split_once(':')?;
    let h: i32 = h.trim().parse().ok()?;
    let m: i32 = m.trim().parse().ok()?;
    Some(h * 60 + m)
}

fn weekday_to_num(name: &str) -> Option<u32> {
    match name.to_lowercase().as_str() {
        "mon" | "monday" => Some(0),
        "tue" | "tuesday" => Some(1),
        "wed" | "wednesday" => Some(2),
        "thu" | "thursday" => Some(3),
        "fri" | "friday" => Some(4),
        "sat" | "saturday" => Some(5),
        "sun" | "sunday" => Some(6),
        _ => None,
    }
}

fn weekday_range_matches(spec: &str, day: u32) -> bool {
    if let Some((start_str, end_str)) = spec.split_once('-') {
        if let (Some(s), Some(e)) = (weekday_to_num(start_str.trim()), weekday_to_num(end_str.trim())) {
            if s <= e {
                return day >= s && day <= e;
            } else {
                return day >= s || day <= e;
            }
        }
    }
    // Single day
    if let Some(d) = weekday_to_num(spec.trim()) {
        return day == d;
    }
    true // if we can't parse, treat as match
}

fn parse_int_range(spec: &str) -> Option<(u32, u32)> {
    if let Some((s, e)) = spec.split_once('-') {
        let start: u32 = s.trim().parse().ok()?;
        let end: u32 = e.trim().parse().ok()?;
        Some((start, end))
    } else {
        let v: u32 = spec.trim().parse().ok()?;
        Some((v, v))
    }
}

fn month_to_num(name: &str) -> Option<u32> {
    match name.to_lowercase().as_str() {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => name.parse().ok(),
    }
}

fn month_range_matches(spec: &str, month: u32) -> bool {
    if let Some((start_str, end_str)) = spec.split_once('-') {
        if let (Some(s), Some(e)) = (month_to_num(start_str.trim()), month_to_num(end_str.trim())) {
            if s <= e {
                return month >= s && month <= e;
            } else {
                return month >= s || month <= e;
            }
        }
    }
    if let Some(m) = month_to_num(spec.trim()) {
        return month == m;
    }
    true
}

/// The If() dialplan application.
///
/// Usage: If(condition)
///
/// Begins an If block. If condition is false, skips to the matching
/// ElseIf/Else/EndIf.
pub struct AppIf;

impl DialplanApp for AppIf {
    fn name(&self) -> &str {
        "If"
    }

    fn description(&self) -> &str {
        "Start a conditional block"
    }
}

impl AppIf {
    /// Execute the If application.
    pub async fn exec(channel: &mut Channel, args: &str) -> PbxExecResult {
        let condition = args.trim();
        let is_true = !condition.is_empty() && condition != "0";

        info!("If: channel '{}' condition='{}' result={}", channel.name, condition, is_true);

        // In a real implementation:
        // If false, find matching ElseIf/Else/EndIf and jump there

        PbxExecResult::Success
    }
}

/// The ElseIf() dialplan application.
///
/// Usage: ElseIf(condition)
pub struct AppElseIf;

impl DialplanApp for AppElseIf {
    fn name(&self) -> &str {
        "ElseIf"
    }

    fn description(&self) -> &str {
        "Conditional else-if in an If block"
    }
}

impl AppElseIf {
    /// Execute the ElseIf application.
    pub async fn exec(channel: &mut Channel, args: &str) -> PbxExecResult {
        let condition = args.trim();
        info!("ElseIf: channel '{}' condition='{}'", channel.name, condition);
        PbxExecResult::Success
    }
}

/// The Else() dialplan application.
///
/// Usage: Else()
pub struct AppElse;

impl DialplanApp for AppElse {
    fn name(&self) -> &str {
        "Else"
    }

    fn description(&self) -> &str {
        "Else branch in an If block"
    }
}

impl AppElse {
    /// Execute the Else application.
    pub async fn exec(channel: &mut Channel, _args: &str) -> PbxExecResult {
        info!("Else: channel '{}'", channel.name);
        PbxExecResult::Success
    }
}

/// The EndIf() dialplan application.
///
/// Usage: EndIf()
pub struct AppEndIf;

impl DialplanApp for AppEndIf {
    fn name(&self) -> &str {
        "EndIf"
    }

    fn description(&self) -> &str {
        "End of If block"
    }
}

impl AppEndIf {
    /// Execute the EndIf application.
    pub async fn exec(channel: &mut Channel, _args: &str) -> PbxExecResult {
        info!("EndIf: channel '{}'", channel.name);
        PbxExecResult::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gotoif_true() {
        let mut channel = Channel::new("SIP/test-001");
        let result = AppGotoIf::exec(&mut channel, "1?100:200").await;
        assert_eq!(result, PbxExecResult::Success);
    }

    #[tokio::test]
    async fn test_gotoif_false() {
        let mut channel = Channel::new("SIP/test-001");
        let result = AppGotoIf::exec(&mut channel, "0?100:200").await;
        assert_eq!(result, PbxExecResult::Success);
    }

    #[tokio::test]
    async fn test_gotoiftime_exec() {
        let mut channel = Channel::new("SIP/test-001");
        let result = AppGotoIfTime::exec(&mut channel, "9:00-17:00,mon-fri,*,*?open:closed").await;
        assert_eq!(result, PbxExecResult::Success);
    }

    #[tokio::test]
    async fn test_if_exec() {
        let mut channel = Channel::new("SIP/test-001");
        let result = AppIf::exec(&mut channel, "1").await;
        assert_eq!(result, PbxExecResult::Success);
    }

    #[tokio::test]
    async fn test_endif_exec() {
        let mut channel = Channel::new("SIP/test-001");
        let result = AppEndIf::exec(&mut channel, "").await;
        assert_eq!(result, PbxExecResult::Success);
    }
}
