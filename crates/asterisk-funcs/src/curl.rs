//! CURL() function - HTTP client for dialplan.
//!
//! Port of func_curl.c from Asterisk C.
//!
//! Provides:
//! - CURL(url[,post-data]) - fetch URL via HTTP GET/POST
//!
//! Note: This implementation uses a simple blocking approach.
//! In production Asterisk this uses libcurl. Here we provide the
//! interface and options framework, with a stub HTTP client that
//! can be replaced with a real implementation (e.g., reqwest).

use crate::{DialplanFunc, FuncContext, FuncError, FuncResult};

/// CURL options configurable via CURLOPT().
///
/// These mirror the options available in Asterisk's func_curl.c.
#[derive(Debug, Clone)]
pub struct CurlOptions {
    /// Connection timeout in seconds.
    pub connect_timeout: u32,
    /// Maximum total time in seconds.
    pub max_time: u32,
    /// User-Agent header value.
    pub user_agent: String,
    /// HTTP proxy URL.
    pub proxy: String,
    /// Whether to verify SSL certificates.
    pub ssl_verify: bool,
    /// Custom HTTP headers (name: value).
    pub headers: Vec<(String, String)>,
    /// HTTP basic auth username.
    pub username: String,
    /// HTTP basic auth password.
    pub password: String,
    /// Whether to follow redirects.
    pub follow_location: bool,
    /// Maximum number of redirects to follow.
    pub max_redirects: u32,
    /// Cookie string to send.
    pub cookie: String,
    /// Hash algorithm for response hashing (empty = no hashing).
    pub hash_type: String,
    /// Whether to fail silently on HTTP errors.
    pub fail_on_error: bool,
}

impl Default for CurlOptions {
    fn default() -> Self {
        Self {
            connect_timeout: 30,
            max_time: 120,
            user_agent: "rustisk-curl/1.0".to_string(),
            proxy: String::new(),
            ssl_verify: true,
            headers: Vec::new(),
            username: String::new(),
            password: String::new(),
            follow_location: true,
            max_redirects: 10,
            cookie: String::new(),
            hash_type: String::new(),
            fail_on_error: false,
        }
    }
}

impl CurlOptions {
    /// Parse a CURLOPT setting.
    pub fn set_option(&mut self, name: &str, value: &str) -> Result<(), FuncError> {
        match name.to_lowercase().as_str() {
            "conntimeout" | "connect_timeout" => {
                self.connect_timeout = value.parse().map_err(|_| {
                    FuncError::InvalidArgument(format!(
                        "CURLOPT: invalid connect_timeout '{}'",
                        value
                    ))
                })?;
            }
            "maxtime" | "max_time" => {
                self.max_time = value.parse().map_err(|_| {
                    FuncError::InvalidArgument(format!(
                        "CURLOPT: invalid max_time '{}'",
                        value
                    ))
                })?;
            }
            "useragent" | "user_agent" => {
                self.user_agent = value.to_string();
            }
            "proxy" => {
                self.proxy = value.to_string();
            }
            "ssl_verifypeer" | "ssl_verify" => {
                self.ssl_verify = value != "0" && value.to_lowercase() != "false";
            }
            "httpheader" | "header" => {
                if let Some(colon) = value.find(':') {
                    let name = value[..colon].trim().to_string();
                    let val = value[colon + 1..].trim().to_string();
                    self.headers.push((name, val));
                }
            }
            "userpwd" => {
                if let Some(colon) = value.find(':') {
                    self.username = value[..colon].to_string();
                    self.password = value[colon + 1..].to_string();
                } else {
                    self.username = value.to_string();
                }
            }
            "followlocation" | "follow_location" => {
                self.follow_location = value != "0" && value.to_lowercase() != "false";
            }
            "maxredirs" | "max_redirects" => {
                self.max_redirects = value.parse().map_err(|_| {
                    FuncError::InvalidArgument(format!(
                        "CURLOPT: invalid max_redirects '{}'",
                        value
                    ))
                })?;
            }
            "cookie" => {
                self.cookie = value.to_string();
            }
            "hashtype" | "hash" => {
                self.hash_type = value.to_string();
            }
            "failonerror" | "fail_on_error" => {
                self.fail_on_error = value != "0" && value.to_lowercase() != "false";
            }
            other => {
                return Err(FuncError::InvalidArgument(format!(
                    "CURLOPT: unknown option '{}'",
                    other
                )));
            }
        }
        Ok(())
    }
}

/// CURL() function.
///
/// Fetches a URL via HTTP GET or POST.
///
/// Usage:
///   CURL(url)          - HTTP GET
///   CURL(url,postdata) - HTTP POST with body
///
/// Returns the response body as a string.
///
/// This is a stub implementation. In production, this would use a real HTTP
/// client library (reqwest, hyper, or libcurl FFI).
pub struct FuncCurl {
    /// Default options for CURL requests.
    pub options: CurlOptions,
}

impl FuncCurl {
    pub fn new() -> Self {
        Self {
            options: CurlOptions::default(),
        }
    }

    /// Perform an HTTP request (stub implementation).
    ///
    /// In a real implementation, this would make an actual HTTP request.
    /// Currently returns a placeholder indicating the request that would be made.
    fn perform_request(&self, url: &str, post_data: Option<&str>) -> Result<String, FuncError> {
        // Validate URL
        if url.is_empty() {
            return Err(FuncError::InvalidArgument(
                "CURL: URL is required".to_string(),
            ));
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(FuncError::InvalidArgument(format!(
                "CURL: invalid URL '{}', must start with http:// or https://",
                url
            )));
        }

        // Stub: in production this would use reqwest or similar.
        // Return a diagnostic string describing the request.
        let method = if post_data.is_some() { "POST" } else { "GET" };
        let body_info = post_data
            .map(|d| format!(", body={} bytes", d.len()))
            .unwrap_or_default();

        Ok(format!(
            "[CURL stub: {} {} timeout={}s{}]",
            method, url, self.options.max_time, body_info
        ))
    }
}

impl Default for FuncCurl {
    fn default() -> Self {
        Self::new()
    }
}

impl DialplanFunc for FuncCurl {
    fn name(&self) -> &str {
        "CURL"
    }

    fn read(&self, _ctx: &FuncContext, args: &str) -> FuncResult {
        let parts: Vec<&str> = args.splitn(2, ',').collect();
        let url = parts[0].trim();
        let post_data = parts.get(1).map(|s| s.trim());

        self.perform_request(url, post_data)
    }
}

/// CURLOPT() function.
///
/// Reads or writes CURL options for the channel.
///
/// Usage:
///   CURLOPT(option) -> current value
///   Set(CURLOPT(option)=value) -> set option
pub struct FuncCurlOpt;

impl DialplanFunc for FuncCurlOpt {
    fn name(&self) -> &str {
        "CURLOPT"
    }

    fn read(&self, ctx: &FuncContext, args: &str) -> FuncResult {
        let option = args.trim();
        let key = format!("__CURLOPT_{}", option.to_lowercase());
        Ok(ctx.get_variable(&key).cloned().unwrap_or_default())
    }

    fn write(&self, ctx: &mut FuncContext, args: &str, value: &str) -> Result<(), FuncError> {
        let option = args.trim();
        // Validate option name
        let mut opts = CurlOptions::default();
        opts.set_option(option, value)?;

        let key = format!("__CURLOPT_{}", option.to_lowercase());
        ctx.set_variable(&key, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_curl_get() {
        let ctx = FuncContext::new();
        let func = FuncCurl::new();
        let result = func.read(&ctx, "http://example.com/api").unwrap();
        assert!(result.contains("GET"));
        assert!(result.contains("example.com"));
    }

    #[test]
    fn test_curl_post() {
        let ctx = FuncContext::new();
        let func = FuncCurl::new();
        let result = func
            .read(&ctx, "http://example.com/api,key=value&foo=bar")
            .unwrap();
        assert!(result.contains("POST"));
    }

    #[test]
    fn test_curl_invalid_url() {
        let ctx = FuncContext::new();
        let func = FuncCurl::new();
        assert!(func.read(&ctx, "not-a-url").is_err());
    }

    #[test]
    fn test_curl_options() {
        let mut opts = CurlOptions::default();
        opts.set_option("conntimeout", "10").unwrap();
        assert_eq!(opts.connect_timeout, 10);
        opts.set_option("useragent", "MyAgent/1.0").unwrap();
        assert_eq!(opts.user_agent, "MyAgent/1.0");
        opts.set_option("ssl_verifypeer", "0").unwrap();
        assert!(!opts.ssl_verify);
        opts.set_option("httpheader", "X-Custom: test").unwrap();
        assert_eq!(opts.headers.len(), 1);
        assert_eq!(opts.headers[0].0, "X-Custom");
    }

    #[test]
    fn test_curlopt_function() {
        let mut ctx = FuncContext::new();
        let func = FuncCurlOpt;
        func.write(&mut ctx, "conntimeout", "15").unwrap();
        assert_eq!(func.read(&ctx, "conntimeout").unwrap(), "15");
    }
}
