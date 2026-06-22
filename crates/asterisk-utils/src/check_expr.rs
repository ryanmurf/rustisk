//! check_expr - Validate Asterisk dialplan expressions
//!
//! Scans extensions.conf files for $[...] expressions and checks them for
//! common problems such as operators not separated by spaces. This is a
//! static analysis tool that helps dialplan authors find subtle bugs.
//!
//! Port of asterisk/utils/check_expr.c

use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::process;

/// Check Asterisk dialplan expressions in extensions.conf files
#[derive(Parser, Debug)]
#[command(
    name = "check_expr",
    about = "Validate $[...] expressions in Asterisk extensions.conf files"
)]
struct Args {
    /// Path to extensions.conf file to check
    file: String,

    /// Variable assignments in the form VAR=VALUE (used for expression evaluation)
    #[arg(trailing_var_arg = true)]
    vars: Vec<String>,
}

/// Statistics collected during parsing
#[derive(Debug, Default)]
struct ExprStats {
    expr_count: usize,
    ok_count: usize,
    warn_count: usize,
    max_size: usize,
    total_size: usize,
}

/// Check a single expression for common problems.
///
/// Returns a list of warning messages. Checks for operators that are
/// not properly separated by spaces, which can lead to unexpected
/// evaluation behavior.
fn check_expr(buffer: &str, lineno: usize) -> Vec<String> {
    let mut warnings = Vec::new();
    let chars: Vec<char> = buffer.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            '"' => {
                // Skip quoted strings
                i += 1;
                while i < len && chars[i] != '"' {
                    i += 1;
                }
                if i >= len {
                    warnings.push(format!(
                        "WARNING: line {lineno}: Unterminated double quote found"
                    ));
                }
            }
            '>' | '<' | '!' => {
                // Check for >=, <=, != operators not separated by spaces
                if i + 1 < len && chars[i + 1] == '=' {
                    let prev_no_space = i > 0 && chars[i - 1] != ' ';
                    let next_no_space = i + 2 < len && chars[i + 2] != ' ';
                    if prev_no_space || next_no_space {
                        warnings.push(format!(
                            "WARNING: line {lineno}: '{}{}' operator not separated by spaces. \
                             This may lead to confusion. You may wish to use double quotes \
                             to quote the grouping it is in. Please check!",
                            chars[i],
                            chars[i + 1]
                        ));
                    }
                }
            }
            '|' | '&' | '=' | '+' | '-' | '*' | '/' | '%' | '?' | ':' => {
                let prev_no_space = i > 0 && chars[i - 1] != ' ';
                let next_no_space = i + 1 < len && chars[i + 1] != ' ';
                if prev_no_space || next_no_space {
                    warnings.push(format!(
                        "WARNING: line {lineno}: '{}' operator not separated by spaces. \
                         This may lead to confusion. You may wish to use double quotes \
                         to quote the grouping it is in. Please check!",
                        chars[i]
                    ));
                }
            }
            _ => {}
        }
        i += 1;
    }

    warnings
}

/// Evaluate an expression by substituting variables.
///
/// Replaces ${varname} references with their values from the variable map.
/// Unknown variables are replaced with "555" (matching the C original).
fn eval_expr(buffer: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::new();
    let chars: Vec<char> = buffer.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len && chars[i] == '$' && chars[i + 1] == '{' {
            // Variable reference
            let start = i + 2;
            let mut brace_level = 1;
            let mut j = start;
            while j < len {
                if chars[j] == '{' {
                    brace_level += 1;
                } else if chars[j] == '}' {
                    brace_level -= 1;
                    if brace_level == 0 {
                        break;
                    }
                }
                j += 1;
            }
            if j < len && chars[j] == '}' {
                let varname: String = chars[start..j].iter().collect();
                if let Some(val) = vars.get(&varname) {
                    result.push_str(val);
                } else {
                    result.push_str("555"); // default substitution
                }
                i = j + 1;
            } else {
                result.push(chars[i]);
                i += 1;
            }
        } else if chars[i] == '\\' && i + 1 < len {
            // Escaped character
            i += 1;
            result.push(chars[i]);
            i += 1;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Parse an extensions.conf file and check all $[...] expressions.
fn parse_file(
    filename: &str,
    vars: &HashMap<String, String>,
) -> Result<ExprStats, String> {
    let content = fs::read_to_string(filename)
        .map_err(|e| format!("Couldn't open {filename} for reading: {e}"))?;

    let mut stats = ExprStats::default();
    let mut lineno = 1usize;
    let chars: Vec<char> = content.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut last_char = '\0';

    while i < len {
        let c = chars[i];
        if c == '\n' {
            lineno += 1;
        } else if c == '[' && last_char == '$' {
            // Found $[ - start of expression
            let mut bracket_level = 1;
            let mut buffer = String::new();
            i += 1;

            while i < len {
                let ec = chars[i];
                if ec == '[' {
                    bracket_level += 1;
                } else if ec == ']' {
                    bracket_level -= 1;
                    if bracket_level == 0 {
                        break;
                    }
                }
                if ec == '\n' {
                    eprintln!(
                        "--- ERROR --- A newline in the middle of an expression at line {lineno}!"
                    );
                    lineno += 1;
                }
                buffer.push(ec);
                i += 1;
            }

            if i >= len {
                eprintln!(
                    "--- ERROR --- EOF reached in middle of an expression at line {lineno}!"
                );
                return Ok(stats);
            }

            // Update statistics
            stats.total_size += buffer.len();
            stats.expr_count += 1;
            if buffer.len() > stats.max_size {
                stats.max_size = buffer.len();
            }

            // Check for warnings
            let warnings = check_expr(&buffer, lineno);
            if warnings.is_empty() {
                println!("OK -- $[{buffer}] at line {lineno}");
                stats.ok_count += 1;
            } else {
                println!(
                    "Warning(s) at line {lineno}, expression: $[{buffer}]"
                );
                for w in &warnings {
                    eprintln!("  {w}");
                }
                stats.warn_count += warnings.len();
            }

            // Evaluate expression (for logging)
            let evaluated = eval_expr(&buffer, vars);
            let _ = evaluated; // Result logged in the C version to expr2_log
        }
        last_char = c;
        i += 1;
    }

    Ok(stats)
}

fn main() {
    let args = Args::parse();

    // Parse variable assignments from command line
    let mut vars = HashMap::new();
    for var_decl in &args.vars {
        if let Some(eq_pos) = var_decl.find('=') {
            let name = &var_decl[..eq_pos];
            let value = &var_decl[eq_pos + 1..];
            vars.insert(name.to_string(), value.to_string());
        }
    }

    match parse_file(&args.file, &vars) {
        Ok(stats) => {
            let avg = stats.total_size.checked_div(stats.expr_count).unwrap_or(0);
            println!("Summary:");
            println!("  Expressions detected: {}", stats.expr_count);
            println!("  Expressions OK:  {}", stats.ok_count);
            println!("  Total # Warnings:   {}", stats.warn_count);
            println!("  Longest Expr:   {} chars", stats.max_size);
            println!("  Ave expr len:  {avg} chars");
        }
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_expr_no_warnings() {
        let warnings = check_expr("1 + 2", 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_check_expr_missing_spaces() {
        let warnings = check_expr("1+2", 1);
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("operator not separated by spaces"));
    }

    #[test]
    fn test_check_expr_comparison_operator() {
        let warnings = check_expr("a>=b", 1);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn test_check_expr_quoted_string() {
        // Operators inside quotes should not trigger warnings
        let warnings = check_expr("\"a+b\" = \"c\"", 1);
        // The = should have space issues but the + inside quotes should not
        // Actually the = does have spaces around it, so no warning
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_eval_expr_variable_substitution() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "42".to_string());

        let result = eval_expr("${FOO} + 1", &vars);
        assert_eq!(result, "42 + 1");
    }

    #[test]
    fn test_eval_expr_unknown_variable() {
        let vars = HashMap::new();
        let result = eval_expr("${UNKNOWN}", &vars);
        assert_eq!(result, "555");
    }

    #[test]
    fn test_eval_expr_escape() {
        let vars = HashMap::new();
        let result = eval_expr("a\\+b", &vars);
        assert_eq!(result, "a+b");
    }
}
