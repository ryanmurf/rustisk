//! check_expr2 - Extended Asterisk expression checker
//!
//! An enhanced version of check_expr that provides deeper analysis of
//! Asterisk dialplan expressions. In addition to spacing checks, this
//! version performs:
//!
//! - Type inference on expression operands
//! - Detection of potential division by zero
//! - Nested expression validation
//! - String vs numeric context verification
//! - Ternary operator balance checking
//!
//! Port of the check_expr2 concept from Asterisk utilities.

use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::process;

/// Extended Asterisk expression checker
#[derive(Parser, Debug)]
#[command(
    name = "check_expr2",
    about = "Extended validation of $[...] expressions in Asterisk dialplan"
)]
struct Args {
    /// Path to extensions.conf file to check
    file: String,

    /// Variable assignments in the form VAR=VALUE
    #[arg(trailing_var_arg = true)]
    vars: Vec<String>,

    /// Enable verbose output with expression evaluation results
    #[arg(short, long)]
    verbose: bool,
}

/// Types that an expression operand can have
#[derive(Debug, Clone, PartialEq)]
enum ExprType {
    Integer,
    Float,
    String,
    Variable,
    Unknown,
}

/// A token in an expression
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum Token {
    Number(String),
    StringLiteral(String),
    Variable(String),
    Operator(String),
    OpenParen,
    CloseParen,
    Ternary,
    Colon,
}

/// Diagnostic severity
#[derive(Debug, Clone, PartialEq)]
enum Severity {
    Error,
    Warning,
    Info,
}

/// A diagnostic message from the checker
#[derive(Debug, Clone)]
struct Diagnostic {
    severity: Severity,
    line: usize,
    message: String,
}

/// Extended statistics
#[derive(Debug, Default)]
struct ExtendedStats {
    expr_count: usize,
    ok_count: usize,
    warning_count: usize,
    error_count: usize,
    info_count: usize,
    max_depth: usize,
    max_size: usize,
    total_size: usize,
}

/// Tokenize an expression string into tokens for analysis.
fn tokenize(expr: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = expr.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            ' ' | '\t' => {
                i += 1;
            }
            '(' => {
                tokens.push(Token::OpenParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::CloseParen);
                i += 1;
            }
            '?' => {
                tokens.push(Token::Ternary);
                i += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                i += 1;
            }
            '"' => {
                // Quoted string
                let mut s = String::new();
                i += 1;
                while i < len && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    i += 1; // skip closing quote
                }
                tokens.push(Token::StringLiteral(s));
            }
            '$' if i + 1 < len && chars[i + 1] == '{' => {
                // Variable reference
                let mut varname = String::new();
                i += 2;
                let mut brace_depth = 1;
                while i < len && brace_depth > 0 {
                    if chars[i] == '{' {
                        brace_depth += 1;
                    } else if chars[i] == '}' {
                        brace_depth -= 1;
                        if brace_depth == 0 {
                            break;
                        }
                    }
                    varname.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    i += 1; // skip }
                }
                tokens.push(Token::Variable(varname));
            }
            '0'..='9' | '.' => {
                let mut num = String::new();
                while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    num.push(chars[i]);
                    i += 1;
                }
                tokens.push(Token::Number(num));
            }
            '>' | '<' | '!' | '=' | '|' | '&' | '+' | '-' | '*' | '/' | '%' | '~' => {
                let mut op = String::new();
                op.push(chars[i]);
                i += 1;
                // Check for two-character operators
                if i < len && (chars[i] == '=' || chars[i] == chars[i - 1]) {
                    // >=, <=, !=, ==, ||, &&
                    if matches!(
                        (op.as_str(), chars[i]),
                        (">" | "<" | "!" | "=", '=') | ("|", '|') | ("&", '&')
                    ) {
                        op.push(chars[i]);
                        i += 1;
                    }
                }
                tokens.push(Token::Operator(op));
            }
            _ => {
                // Identifier or string character
                let mut s = String::new();
                while i < len
                    && !chars[i].is_whitespace()
                    && !matches!(
                        chars[i],
                        '(' | ')' | '?' | ':' | '"' | '>' | '<' | '!'
                            | '=' | '|' | '&' | '+' | '-' | '*' | '/' | '%'
                    )
                {
                    s.push(chars[i]);
                    i += 1;
                }
                if !s.is_empty() {
                    tokens.push(Token::StringLiteral(s));
                }
            }
        }
    }

    tokens
}

/// Infer the type of a token.
fn infer_type(token: &Token) -> ExprType {
    match token {
        Token::Number(n) => {
            if n.contains('.') {
                ExprType::Float
            } else {
                ExprType::Integer
            }
        }
        Token::StringLiteral(_) => ExprType::String,
        Token::Variable(_) => ExprType::Variable,
        _ => ExprType::Unknown,
    }
}

/// Perform extended checks on a single expression.
fn check_expr_extended(expr: &str, lineno: usize) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let tokens = tokenize(expr);

    // Check 1: Spacing around operators (from check_expr)
    let chars: Vec<char> = expr.chars().collect();
    let len = chars.len();
    let mut ci = 0;
    while ci < len {
        if chars[ci] == '"' {
            ci += 1;
            while ci < len && chars[ci] != '"' {
                ci += 1;
            }
        } else if matches!(chars[ci], '|' | '&' | '=' | '+' | '-' | '*' | '/' | '%' | '?' | ':')
        {
            let prev_no_space = ci > 0 && chars[ci - 1] != ' ';
            let next_no_space = ci + 1 < len && chars[ci + 1] != ' ';
            if prev_no_space || next_no_space {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    line: lineno,
                    message: format!(
                        "'{}' operator not separated by spaces",
                        chars[ci]
                    ),
                });
            }
        } else if matches!(chars[ci], '>' | '<' | '!') && ci + 1 < len && chars[ci + 1] == '=' {
            let prev_no_space = ci > 0 && chars[ci - 1] != ' ';
            let next_no_space = ci + 2 < len && chars[ci + 2] != ' ';
            if prev_no_space || next_no_space {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    line: lineno,
                    message: format!(
                        "'{}=' operator not separated by spaces",
                        chars[ci]
                    ),
                });
            }
            ci += 1; // skip the =
        }
        ci += 1;
    }

    // Check 2: Ternary balance
    let ternary_count = tokens
        .iter()
        .filter(|t| matches!(t, Token::Ternary))
        .count();
    let colon_count = tokens
        .iter()
        .filter(|t| matches!(t, Token::Colon))
        .count();
    if ternary_count != colon_count {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            line: lineno,
            message: format!(
                "Unbalanced ternary operator: {} '?' but {} ':'",
                ternary_count, colon_count
            ),
        });
    }

    // Check 3: Parenthesis balance
    let open_parens = tokens
        .iter()
        .filter(|t| matches!(t, Token::OpenParen))
        .count();
    let close_parens = tokens
        .iter()
        .filter(|t| matches!(t, Token::CloseParen))
        .count();
    if open_parens != close_parens {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            line: lineno,
            message: format!(
                "Unbalanced parentheses: {} '(' but {} ')'",
                open_parens, close_parens
            ),
        });
    }

    // Check 4: Division by zero possibility
    for window in tokens.windows(2) {
        if let [Token::Operator(op), Token::Number(n)] = window {
            if (op == "/" || op == "%") && n.parse::<f64>().is_ok_and(|v| v == 0.0) {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    line: lineno,
                    message: "Possible division by zero detected".to_string(),
                });
            }
        }
    }

    // Check 5: Type mixing hints
    for window in tokens.windows(3) {
        if let [ref left, Token::Operator(ref op), ref right] = window {
            let left_type = infer_type(left);
            let right_type = infer_type(right);
            if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%")
                && left_type == ExprType::String
                && right_type == ExprType::String
            {
                diagnostics.push(Diagnostic {
                    severity: Severity::Info,
                    line: lineno,
                    message: format!(
                        "Arithmetic operator '{op}' used with string operands; \
                         Asterisk will attempt numeric conversion"
                    ),
                });
            }
        }
    }

    diagnostics
}

/// Parse file and check all expressions.
fn parse_file(
    filename: &str,
    _vars: &HashMap<String, String>,
    verbose: bool,
) -> Result<ExtendedStats, String> {
    let content = fs::read_to_string(filename)
        .map_err(|e| format!("Cannot open {filename}: {e}"))?;

    let mut stats = ExtendedStats::default();
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
            let mut bracket_level = 1;
            let mut depth = 1usize;
            let mut buffer = String::new();
            i += 1;

            while i < len {
                let ec = chars[i];
                if ec == '[' {
                    bracket_level += 1;
                    depth += 1;
                    if depth > stats.max_depth {
                        stats.max_depth = depth;
                    }
                } else if ec == ']' {
                    bracket_level -= 1;
                    if bracket_level == 0 {
                        break;
                    }
                }
                if ec == '\n' {
                    lineno += 1;
                }
                buffer.push(ec);
                i += 1;
            }

            if i >= len {
                eprintln!("ERROR: EOF in middle of expression at line {lineno}");
                return Ok(stats);
            }

            stats.total_size += buffer.len();
            stats.expr_count += 1;
            if buffer.len() > stats.max_size {
                stats.max_size = buffer.len();
            }

            let diagnostics = check_expr_extended(&buffer, lineno);

            let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);
            let has_warnings = diagnostics.iter().any(|d| d.severity == Severity::Warning);

            if diagnostics.is_empty() {
                if verbose {
                    println!("OK -- $[{buffer}] at line {lineno}");
                }
                stats.ok_count += 1;
            } else {
                for diag in &diagnostics {
                    let prefix = match diag.severity {
                        Severity::Error => {
                            stats.error_count += 1;
                            "ERROR"
                        }
                        Severity::Warning => {
                            stats.warning_count += 1;
                            "WARNING"
                        }
                        Severity::Info => {
                            stats.info_count += 1;
                            "INFO"
                        }
                    };
                    println!("{prefix}: line {}: {}", diag.line, diag.message);
                }
                if !has_errors && !has_warnings {
                    stats.ok_count += 1;
                }
            }
        }
        last_char = c;
        i += 1;
    }

    Ok(stats)
}

fn main() {
    let args = Args::parse();

    let mut vars = HashMap::new();
    for var_decl in &args.vars {
        if let Some(eq_pos) = var_decl.find('=') {
            let name = &var_decl[..eq_pos];
            let value = &var_decl[eq_pos + 1..];
            vars.insert(name.to_string(), value.to_string());
        }
    }

    match parse_file(&args.file, &vars, args.verbose) {
        Ok(stats) => {
            let avg = stats.total_size.checked_div(stats.expr_count).unwrap_or(0);
            println!("\nExtended Expression Check Summary:");
            println!("  Expressions detected: {}", stats.expr_count);
            println!("  Expressions OK:       {}", stats.ok_count);
            println!("  Errors:               {}", stats.error_count);
            println!("  Warnings:             {}", stats.warning_count);
            println!("  Info messages:        {}", stats.info_count);
            println!("  Max nesting depth:    {}", stats.max_depth);
            println!("  Longest expr:         {} chars", stats.max_size);
            println!("  Average expr len:     {avg} chars");
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
    fn test_tokenize_simple() {
        let tokens = tokenize("1 + 2");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(&tokens[0], Token::Number(n) if n == "1"));
        assert!(matches!(&tokens[1], Token::Operator(op) if op == "+"));
        assert!(matches!(&tokens[2], Token::Number(n) if n == "2"));
    }

    #[test]
    fn test_tokenize_variable() {
        let tokens = tokenize("${FOO} + 1");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(&tokens[0], Token::Variable(v) if v == "FOO"));
    }

    #[test]
    fn test_tokenize_ternary() {
        let tokens = tokenize("1 ? 2 : 3");
        assert_eq!(tokens.len(), 5);
        assert!(matches!(&tokens[1], Token::Ternary));
        assert!(matches!(&tokens[3], Token::Colon));
    }

    #[test]
    fn test_check_division_by_zero() {
        let diags = check_expr_extended("x / 0", 1);
        assert!(diags.iter().any(|d| d.message.contains("division by zero")));
    }

    #[test]
    fn test_check_unbalanced_ternary() {
        let diags = check_expr_extended("1 ? 2", 1);
        assert!(diags
            .iter()
            .any(|d| d.message.contains("Unbalanced ternary")));
    }

    #[test]
    fn test_check_unbalanced_parens() {
        let diags = check_expr_extended("(1 + 2", 1);
        assert!(diags
            .iter()
            .any(|d| d.message.contains("Unbalanced parentheses")));
    }

    #[test]
    fn test_infer_type() {
        assert_eq!(infer_type(&Token::Number("42".to_string())), ExprType::Integer);
        assert_eq!(
            infer_type(&Token::Number("3.14".to_string())),
            ExprType::Float
        );
        assert_eq!(
            infer_type(&Token::StringLiteral("hello".to_string())),
            ExprType::String
        );
        assert_eq!(
            infer_type(&Token::Variable("FOO".to_string())),
            ExprType::Variable
        );
    }
}
