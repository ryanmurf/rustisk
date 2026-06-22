//! Static dialplan loader from extensions.conf.
//!
//! Port of pbx/pbx_config.c from Asterisk C.
//!
//! Reads extensions.conf and populates the Dialplan with contexts,
//! extensions, priorities, includes, and other directives.
//!
//! Format:
//!   [context_name]
//!   exten => pattern,priority,Application(args)
//!   exten => pattern,priority(label),Application(args)
//!   include => other_context
//!   ignorepat => pattern

use super::{Context, Dialplan, Extension, Priority};
use std::collections::HashMap;
use tracing::{debug, info};

/// Result of loading extensions.conf.
#[derive(Debug)]
pub struct LoadResult {
    /// Number of contexts loaded
    pub contexts: usize,
    /// Number of extensions loaded
    pub extensions: usize,
    /// Number of priorities loaded
    pub priorities: usize,
    /// Number of includes loaded
    pub includes: usize,
    /// Warnings encountered during loading
    pub warnings: Vec<String>,
}

/// Parse a single `exten =>` line.
///
/// Format: `pattern,priority[(label)],Application(args)`
///
/// Returns (pattern, priority_num, label, app, app_data) or None if invalid.
pub fn parse_exten_line(line: &str) -> Option<(String, i32, Option<String>, String, String)> {
    // Split on first comma to get pattern
    let parts: Vec<&str> = line.splitn(2, ',').collect();
    if parts.len() < 2 {
        return None;
    }
    let pattern = parts[0].trim().to_string();

    // Split remainder on first comma to get priority and app
    let rest = parts[1];
    let parts2: Vec<&str> = rest.splitn(2, ',').collect();
    if parts2.len() < 2 {
        return None;
    }

    let prio_str = parts2[0].trim();
    let app_str = parts2[1].trim();

    // Parse priority - may have label: "1(start)"
    let (priority, label) = parse_priority(prio_str)?;

    // Parse application - "App(args)" or "App"
    let (app, app_data) = parse_application(app_str);

    Some((pattern, priority, label, app, app_data))
}

/// Parse a priority string that may include a label.
/// "1" -> (1, None)
/// "2(my_label)" -> (2, Some("my_label"))
/// "n" -> (-1, None) -- special "next" priority
/// "n(label)" -> (-1, Some("label")) -- next priority with label
fn parse_priority(s: &str) -> Option<(i32, Option<String>)> {
    if s.eq_ignore_ascii_case("n") {
        return Some((-1, None)); // "next" priority placeholder
    }
    if s.eq_ignore_ascii_case("hint") {
        return Some((-2, None)); // hint priority
    }

    if let Some(paren_pos) = s.find('(') {
        let prio_part = &s[..paren_pos];
        let label_part = &s[paren_pos + 1..];
        let label = label_part.trim_end_matches(')').to_string();

        // Handle "n(label)" -- next priority with a label
        if prio_part.eq_ignore_ascii_case("n") {
            return Some((-1, Some(label)));
        }

        let prio: i32 = prio_part.parse().ok()?;
        Some((prio, Some(label)))
    } else {
        let prio: i32 = s.parse().ok()?;
        Some((prio, None))
    }
}

/// Parse "Application(args)" into (app_name, args).
fn parse_application(s: &str) -> (String, String) {
    if let Some(paren_pos) = s.find('(') {
        let app = s[..paren_pos].trim().to_string();
        let data = s[paren_pos + 1..]
            .trim_end_matches(')')
            .to_string();
        (app, data)
    } else {
        (s.trim().to_string(), String::new())
    }
}

/// Parse a `same =>` line (continues previous extension).
///
/// Format: `priority[(label)],Application(args)`
///
/// Returns (priority_num, label, app, app_data) or None if invalid.
pub fn parse_same_line(line: &str) -> Option<(i32, Option<String>, String, String)> {
    // Split on first comma to get priority and app
    let parts: Vec<&str> = line.splitn(2, ',').collect();
    if parts.len() < 2 {
        return None;
    }

    let prio_str = parts[0].trim();
    let app_str = parts[1].trim();

    // Parse priority - may have label: "1(start)" or "n" or "n(label)"
    let (priority, label) = parse_priority(prio_str)?;

    // Parse application - "App(args)" or "App"
    let (app, app_data) = parse_application(app_str);

    Some((priority, label, app, app_data))
}

/// Load a dialplan from extensions.conf content.
///
/// This parses the INI-like format of extensions.conf and builds a Dialplan.
pub fn load_extensions_conf(content: &str) -> (Dialplan, LoadResult) {
    let mut dialplan = Dialplan::new();
    let mut result = LoadResult {
        contexts: 0,
        extensions: 0,
        priorities: 0,
        includes: 0,
        warnings: Vec::new(),
    };

    let mut current_context: Option<String> = None;
    // Track "next" priority per extension per context
    let mut next_prio: HashMap<(String, String), i32> = HashMap::new();
    // Track the last extension pattern for `same =>` support
    let mut last_exten: Option<String> = None;

    for (line_num, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        // Strip inline comments
        let line = if let Some(pos) = line.find(';') {
            line[..pos].trim()
        } else {
            line
        };

        // Context header: [context_name]
        if line.starts_with('[') && line.ends_with(']') {
            let ctx_name = &line[1..line.len() - 1];
            current_context = Some(ctx_name.to_string());
            last_exten = None;
            if !dialplan.contexts.contains_key(ctx_name) {
                dialplan.add_context(Context::new(ctx_name));
                result.contexts += 1;
            }
            debug!("pbx_config: entering context [{}]", ctx_name);
            continue;
        }

        let Some(ref ctx_name) = current_context else {
            result.warnings.push(format!(
                "Line {}: directive outside of any context", line_num + 1
            ));
            continue;
        };

        // exten => pattern,priority,application(args)
        if let Some(exten_data) = line
            .strip_prefix("exten")
            .and_then(|s| s.trim_start().strip_prefix("=>"))
            .or_else(|| line.strip_prefix("exten").and_then(|s| s.trim_start().strip_prefix("=")))
        {
            let exten_data = exten_data.trim();
            match parse_exten_line(exten_data) {
                Some((pattern, mut priority, label, app, app_data)) => {
                    // Handle "n" (next) priority
                    if priority == -1 {
                        let key = (ctx_name.clone(), pattern.clone());
                        let last = next_prio.get(&key).copied().unwrap_or(0);
                        priority = last + 1;
                        next_prio.insert(key, priority);
                    } else if priority > 0 {
                        let key = (ctx_name.clone(), pattern.clone());
                        next_prio.insert(key, priority);
                    }

                    let prio = Priority {
                        priority,
                        app,
                        app_data,
                        label,
                    };

                    let ctx = dialplan
                        .get_context_mut(ctx_name)
                        .unwrap();

                    if let Some(ext) = ctx.extensions.get_mut(&pattern) {
                        ext.add_priority(prio);
                    } else {
                        let mut ext = Extension::new(&pattern);
                        ext.add_priority(prio);
                        ctx.add_extension(ext);
                        result.extensions += 1;
                    }
                    result.priorities += 1;
                    last_exten = Some(pattern);
                }
                None => {
                    result.warnings.push(format!(
                        "Line {}: invalid exten => line: {}",
                        line_num + 1, exten_data
                    ));
                }
            }
            continue;
        }

        // same => priority,application(args) -- continues previous exten
        if let Some(same_data) = line
            .strip_prefix("same")
            .and_then(|s| s.trim_start().strip_prefix("=>"))
            .or_else(|| line.strip_prefix("same").and_then(|s| s.trim_start().strip_prefix("=")))
        {
            let same_data = same_data.trim();
            let Some(ref pattern) = last_exten else {
                result.warnings.push(format!(
                    "Line {}: same => without preceding exten =>",
                    line_num + 1
                ));
                continue;
            };

            match parse_same_line(same_data) {
                Some((mut priority, label, app, app_data)) => {
                    // Handle "n" (next) priority
                    if priority == -1 {
                        let key = (ctx_name.clone(), pattern.clone());
                        let last = next_prio.get(&key).copied().unwrap_or(0);
                        priority = last + 1;
                        next_prio.insert(key, priority);
                    } else if priority > 0 {
                        let key = (ctx_name.clone(), pattern.clone());
                        next_prio.insert(key, priority);
                    }

                    let prio = Priority {
                        priority,
                        app,
                        app_data,
                        label,
                    };

                    let ctx = dialplan
                        .get_context_mut(ctx_name)
                        .unwrap();

                    if let Some(ext) = ctx.extensions.get_mut(pattern) {
                        ext.add_priority(prio);
                    } else {
                        let mut ext = Extension::new(pattern);
                        ext.add_priority(prio);
                        ctx.add_extension(ext);
                        result.extensions += 1;
                    }
                    result.priorities += 1;
                }
                None => {
                    result.warnings.push(format!(
                        "Line {}: invalid same => line: {}",
                        line_num + 1, same_data
                    ));
                }
            }
            continue;
        }

        // include => context_name
        if let Some(include_data) = line
            .strip_prefix("include")
            .and_then(|s| s.trim_start().strip_prefix("=>"))
            .or_else(|| line.strip_prefix("include").and_then(|s| s.trim_start().strip_prefix("=")))
        {
            let include_ctx = include_data.trim();
            if let Some(ctx) = dialplan.get_context_mut(ctx_name) {
                ctx.add_include(include_ctx);
                result.includes += 1;
            }
            continue;
        }

        // ignorepat => pattern (we acknowledge but don't need to store)
        if line.starts_with("ignorepat") {
            continue;
        }

        // Unknown directive
        debug!("pbx_config: unknown directive at line {}: {}", line_num + 1, line);
    }

    info!(
        "pbx_config: loaded {} contexts, {} extensions, {} priorities, {} includes",
        result.contexts, result.extensions, result.priorities, result.includes,
    );

    (dialplan, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_exten_line_basic() {
        let (pat, prio, label, app, data) =
            parse_exten_line("100,1,Answer()").unwrap();
        assert_eq!(pat, "100");
        assert_eq!(prio, 1);
        assert!(label.is_none());
        assert_eq!(app, "Answer");
        assert!(data.is_empty());
    }

    #[test]
    fn test_parse_exten_line_with_args() {
        let (pat, prio, label, app, data) =
            parse_exten_line("_1XX,1,Dial(SIP/${EXTEN},30)").unwrap();
        assert_eq!(pat, "_1XX");
        assert_eq!(prio, 1);
        assert!(label.is_none());
        assert_eq!(app, "Dial");
        assert_eq!(data, "SIP/${EXTEN},30");
    }

    #[test]
    fn test_parse_exten_line_with_label() {
        let (_pat, prio, label, _app, _) =
            parse_exten_line("100,1(start),Answer()").unwrap();
        assert_eq!(prio, 1);
        assert_eq!(label.unwrap(), "start");
    }

    #[test]
    fn test_parse_priority_next() {
        let (prio, label) = parse_priority("n").unwrap();
        assert_eq!(prio, -1);
        assert!(label.is_none());
    }

    #[test]
    fn test_parse_priority_hint() {
        let (prio, _) = parse_priority("hint").unwrap();
        assert_eq!(prio, -2);
    }

    #[test]
    fn test_load_simple_config() {
        let config = r#"
[default]
exten => 100,1,Answer()
exten => 100,n,Playback(hello-world)
exten => 100,n,Hangup()

exten => 200,1,Dial(SIP/bob,30)
exten => 200,n,Hangup()

[internal]
include => default
exten => 300,1,Answer()
"#;
        let (dialplan, result) = load_extensions_conf(config);
        assert_eq!(result.contexts, 2);
        assert_eq!(result.extensions, 3); // 100, 200, 300
        assert_eq!(result.priorities, 6); // 3 + 2 + 1
        assert_eq!(result.includes, 1);
        assert!(result.warnings.is_empty());

        // Verify contexts
        assert!(dialplan.get_context("default").is_some());
        assert!(dialplan.get_context("internal").is_some());

        // Verify extension 100 priorities
        let (_, ext) = dialplan.find_extension("default", "100").unwrap();
        assert!(ext.get_priority(1).is_some());
        assert!(ext.get_priority(2).is_some()); // "n" -> 2
        assert!(ext.get_priority(3).is_some()); // "n" -> 3

        // Verify include resolution
        let result = dialplan.find_extension("internal", "200");
        assert!(result.is_some()); // found via include
    }

    #[test]
    fn test_load_with_comments() {
        let config = r#"
; This is a comment
[test]
; extension 100
exten => 100,1,Answer()  ; answer the call
exten => 100,n,Hangup()
"#;
        let (_dialplan, result) = load_extensions_conf(config);
        assert_eq!(result.contexts, 1);
        assert_eq!(result.priorities, 2);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_parse_application() {
        let (app, data) = parse_application("Dial(SIP/alice,30,tT)");
        assert_eq!(app, "Dial");
        assert_eq!(data, "SIP/alice,30,tT");
    }

    #[test]
    fn test_parse_application_no_args() {
        let (app, data) = parse_application("Answer");
        assert_eq!(app, "Answer");
        assert!(data.is_empty());
    }

    #[test]
    fn test_parse_same_line_basic() {
        let (prio, label, app, data) = parse_same_line("n,Playback(hello-world)").unwrap();
        assert_eq!(prio, -1); // "n" placeholder
        assert!(label.is_none());
        assert_eq!(app, "Playback");
        assert_eq!(data, "hello-world");
    }

    #[test]
    fn test_parse_same_line_with_label() {
        let (prio, label, app, _) = parse_same_line("n(start),Answer()").unwrap();
        assert_eq!(prio, -1);
        assert_eq!(label.unwrap(), "start");
        assert_eq!(app, "Answer");
    }

    #[test]
    fn test_parse_same_line_explicit_priority() {
        let (prio, label, app, _) = parse_same_line("2,Hangup()").unwrap();
        assert_eq!(prio, 2);
        assert!(label.is_none());
        assert_eq!(app, "Hangup");
    }

    #[test]
    fn test_load_with_same() {
        let config = r#"
[default]
exten => 100,1,Answer()
 same => n,Playback(hello-world)
 same => n,Hangup()

exten => 200,1,Answer()
 same => n(greet),Playback(welcome)
 same => n,Dial(SIP/bob,30)
 same => n,Hangup()
"#;
        let (dialplan, result) = load_extensions_conf(config);
        assert_eq!(result.contexts, 1);
        assert_eq!(result.extensions, 2); // 100 and 200
        assert_eq!(result.priorities, 7); // 3 + 4
        assert!(result.warnings.is_empty());

        // Verify extension 100 priorities
        let (_, ext) = dialplan.find_extension("default", "100").unwrap();
        assert!(ext.get_priority(1).is_some()); // Answer
        assert!(ext.get_priority(2).is_some()); // Playback (same => n)
        assert!(ext.get_priority(3).is_some()); // Hangup (same => n)
        assert_eq!(ext.get_priority(2).unwrap().app, "Playback");
        assert_eq!(ext.get_priority(3).unwrap().app, "Hangup");

        // Verify extension 200 priorities and label
        let (_, ext200) = dialplan.find_extension("default", "200").unwrap();
        assert!(ext200.get_priority(1).is_some());
        assert!(ext200.get_priority(2).is_some());
        assert_eq!(ext200.get_priority(2).unwrap().label.as_deref(), Some("greet"));
        assert_eq!(ext200.get_priority(3).unwrap().app, "Dial");
        assert_eq!(ext200.get_priority(4).unwrap().app, "Hangup");
    }
}
