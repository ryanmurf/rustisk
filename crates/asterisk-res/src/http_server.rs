//! HTTP server for AMI/ARI web interfaces.
//!
//! Port of `res/res_http.c`. Provides a basic HTTP server using tokio for
//! serving the Asterisk Manager Interface (AMI) and Asterisk REST Interface
//! (ARI) web endpoints, plus WebSocket upgrade support.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::RwLock;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum HttpError {
    #[error("HTTP I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP parse error: {0}")]
    Parse(String),
    #[error("HTTP method not allowed: {0}")]
    MethodNotAllowed(String),
    #[error("HTTP not found: {0}")]
    NotFound(String),
    #[error("HTTP authentication required")]
    Unauthorized,
    #[error("HTTP server error: {0}")]
    Internal(String),
}

pub type HttpResult<T> = Result<T, HttpError>;

// ---------------------------------------------------------------------------
// HTTP request/response types
// ---------------------------------------------------------------------------

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Options,
    Head,
    Patch,
}

impl HttpMethod {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "DELETE" => Some(Self::Delete),
            "OPTIONS" => Some(Self::Options),
            "HEAD" => Some(Self::Head),
            "PATCH" => Some(Self::Patch),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Head => "HEAD",
            Self::Patch => "PATCH",
        }
    }
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A parsed HTTP request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: HttpMethod,
    /// Request URI/path.
    pub uri: String,
    /// HTTP version string.
    pub version: String,
    /// Request headers (lowercase keys).
    pub headers: HashMap<String, String>,
    /// Request body.
    pub body: Vec<u8>,
    /// Query string parameters.
    pub query_params: HashMap<String, String>,
    /// Remote address.
    pub remote_addr: SocketAddr,
}

impl HttpRequest {
    /// Parse an HTTP request from a TCP stream.
    pub async fn parse(stream: &mut TcpStream, remote_addr: SocketAddr) -> HttpResult<Self> {
        let mut reader = BufReader::new(stream);

        // Read request line.
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await?;
        let request_line = request_line.trim_end();

        let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
        if parts.len() < 3 {
            return Err(HttpError::Parse(format!(
                "Invalid request line: {}",
                request_line
            )));
        }

        let method = HttpMethod::from_str(parts[0]).ok_or_else(|| {
            HttpError::Parse(format!("Unknown HTTP method: {}", parts[0]))
        })?;
        let full_uri = parts[1].to_string();
        let version = parts[2].to_string();

        // Parse query string.
        let (uri, query_params) = if let Some((path, query)) = full_uri.split_once('?') {
            let params = parse_query_string(query);
            (path.to_string(), params)
        } else {
            (full_uri, HashMap::new())
        };

        // Read headers.
        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                headers.insert(
                    key.trim().to_lowercase(),
                    value.trim().to_string(),
                );
            }
        }

        // Read body based on Content-Length.
        let content_length: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body).await?;
        }

        Ok(Self {
            method,
            uri,
            version,
            headers,
            body,
            query_params,
            remote_addr,
        })
    }

    /// Check if this is a WebSocket upgrade request.
    pub fn is_websocket_upgrade(&self) -> bool {
        let upgrade = self
            .headers
            .get("upgrade")
            .map(|v| v.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
        let connection = self
            .headers
            .get("connection")
            .map(|v| v.to_lowercase().contains("upgrade"))
            .unwrap_or(false);
        upgrade && connection
    }

    /// Get the Sec-WebSocket-Key header.
    pub fn websocket_key(&self) -> Option<&str> {
        self.headers.get("sec-websocket-key").map(|s| s.as_str())
    }

    /// Get the Sec-WebSocket-Protocol header.
    pub fn websocket_protocol(&self) -> Option<&str> {
        self.headers
            .get("sec-websocket-protocol")
            .map(|s| s.as_str())
    }

    /// Get Basic auth credentials if present.
    pub fn basic_auth(&self) -> Option<(String, String)> {
        let auth = self.headers.get("authorization")?;
        let encoded = auth.strip_prefix("Basic ")?;
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let decoded_str = String::from_utf8(decoded).ok()?;
        let (user, pass) = decoded_str.split_once(':')?;
        Some((user.to_string(), pass.to_string()))
    }
}

/// Parse a query string into key-value pairs.
fn parse_query_string(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            params.insert(key.to_string(), value.to_string());
        }
    }
    params
}

/// HTTP response status codes.
#[derive(Debug, Clone, Copy)]
pub struct HttpStatus(pub u16);

impl HttpStatus {
    pub const OK: Self = Self(200);
    pub const NO_CONTENT: Self = Self(204);
    pub const BAD_REQUEST: Self = Self(400);
    pub const UNAUTHORIZED: Self = Self(401);
    pub const FORBIDDEN: Self = Self(403);
    pub const NOT_FOUND: Self = Self(404);
    pub const METHOD_NOT_ALLOWED: Self = Self(405);
    pub const INTERNAL_SERVER_ERROR: Self = Self(500);

    pub fn reason(&self) -> &'static str {
        match self.0 {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            500 => "Internal Server Error",
            _ => "Unknown",
        }
    }
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: HttpStatus,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: HttpStatus) -> Self {
        let mut headers = HashMap::new();
        headers.insert("Server".to_string(), "Rustisk/0.1.0".to_string());
        headers.insert("Connection".to_string(), "close".to_string());
        Self {
            status,
            headers,
            body: Vec::new(),
        }
    }

    pub fn ok() -> Self {
        Self::new(HttpStatus::OK)
    }

    pub fn not_found() -> Self {
        Self::new(HttpStatus::NOT_FOUND)
    }

    pub fn unauthorized() -> Self {
        let mut resp = Self::new(HttpStatus::UNAUTHORIZED);
        resp.headers.insert(
            "WWW-Authenticate".to_string(),
            "Basic realm=\"Asterisk\"".to_string(),
        );
        resp
    }

    pub fn with_body(mut self, content_type: &str, body: Vec<u8>) -> Self {
        self.headers
            .insert("Content-Type".to_string(), content_type.to_string());
        self.headers
            .insert("Content-Length".to_string(), body.len().to_string());
        self.body = body;
        self
    }

    pub fn with_json(self, json: &str) -> Self {
        self.with_body("application/json", json.as_bytes().to_vec())
    }

    pub fn with_text(self, text: &str) -> Self {
        self.with_body("text/plain", text.as_bytes().to_vec())
    }

    pub fn with_html(self, html: &str) -> Self {
        self.with_body("text/html", html.as_bytes().to_vec())
    }

    /// Serialize the response into bytes for transmission.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = format!(
            "HTTP/1.1 {} {}\r\n",
            self.status.0,
            self.status.reason()
        );

        for (key, value) in &self.headers {
            buf.push_str(&format!("{}: {}\r\n", key, value));
        }
        buf.push_str("\r\n");

        let mut bytes = buf.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

// ---------------------------------------------------------------------------
// Route handler
// ---------------------------------------------------------------------------

/// Type alias for HTTP route handler functions.
pub type RouteHandler = Arc<
    dyn Fn(HttpRequest) -> Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        + Send
        + Sync,
>;

/// A registered HTTP route.
#[derive(Clone)]
pub struct Route {
    /// HTTP method.
    pub method: HttpMethod,
    /// Path prefix.
    pub path: String,
    /// Handler function.
    pub handler: RouteHandler,
}

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Route")
            .field("method", &self.method)
            .field("path", &self.path)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// Authentication credentials for HTTP access.
#[derive(Debug, Clone)]
pub struct HttpCredentials {
    pub username: String,
    pub password: String,
}

/// Authentication configuration.
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum AuthConfig {
    /// No authentication required.
    #[default]
    None,
    /// Basic authentication.
    Basic(Vec<HttpCredentials>),
}


impl AuthConfig {
    /// Check if a request is authenticated.
    pub fn check(&self, request: &HttpRequest) -> bool {
        match self {
            Self::None => true,
            Self::Basic(credentials) => {
                if let Some((user, pass)) = request.basic_auth() {
                    credentials
                        .iter()
                        .any(|c| c.username == user && c.password == pass)
                } else {
                    false
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------------------

/// HTTP server for AMI/ARI web interfaces.
///
/// Port of `res/res_http.c`. Uses tokio for async I/O.
pub struct HttpServer {
    /// Listen address.
    listen_addr: SocketAddr,
    /// Registered routes.
    routes: RwLock<Vec<Route>>,
    /// Authentication configuration.
    auth: RwLock<AuthConfig>,
    /// Whether TLS is enabled (stub).
    tls_enabled: bool,
    /// Static file root directory.
    static_root: Option<String>,
}

impl fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpServer")
            .field("listen_addr", &self.listen_addr)
            .field("routes", &self.routes.read().len())
            .field("tls_enabled", &self.tls_enabled)
            .finish()
    }
}

impl HttpServer {
    /// Create a new HTTP server.
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            routes: RwLock::new(Vec::new()),
            auth: RwLock::new(AuthConfig::None),
            tls_enabled: false,
            static_root: None,
        }
    }

    /// Set the authentication configuration.
    pub fn set_auth(&self, auth: AuthConfig) {
        *self.auth.write() = auth;
    }

    /// Set the static file root directory.
    pub fn set_static_root(&mut self, path: &str) {
        self.static_root = Some(path.to_string());
    }

    /// Enable TLS (stub -- just sets a flag).
    pub fn enable_tls(&mut self) {
        self.tls_enabled = true;
        warn!("TLS support is stubbed -- not actually enabled");
    }

    /// Register a route.
    pub fn add_route<F, Fut>(&self, method: HttpMethod, path: &str, handler: F)
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        let handler = Arc::new(move |req: HttpRequest| {
            let fut = handler(req);
            Box::pin(fut) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.write().push(Route {
            method,
            path: path.to_string(),
            handler,
        });

        debug!(method = %method, path, "HTTP route registered");
    }

    /// Find a matching route for a request.
    fn find_route(&self, method: HttpMethod, path: &str) -> Option<Route> {
        let routes = self.routes.read();
        routes
            .iter()
            .find(|r| r.method == method && path.starts_with(&r.path))
            .cloned()
    }

    /// Handle a single HTTP connection.
    async fn handle_connection(self: Arc<Self>, mut stream: TcpStream, remote_addr: SocketAddr) {
        let request = match HttpRequest::parse(&mut stream, remote_addr).await {
            Ok(req) => req,
            Err(e) => {
                debug!(error = %e, "Failed to parse HTTP request");
                let response = HttpResponse::new(HttpStatus::BAD_REQUEST)
                    .with_text(&format!("Bad Request: {}", e));
                let _ = stream.write_all(&response.to_bytes()).await;
                return;
            }
        };

        debug!(
            method = %request.method,
            uri = %request.uri,
            remote = %remote_addr,
            "HTTP request"
        );

        // Check authentication.
        let auth = self.auth.read().clone();
        if !auth.check(&request) {
            let response = HttpResponse::unauthorized();
            let _ = stream.write_all(&response.to_bytes()).await;
            return;
        }

        // Check for WebSocket upgrade.
        if request.is_websocket_upgrade() {
            debug!(uri = %request.uri, "WebSocket upgrade request");
            // In a full implementation, this would upgrade the connection.
            let response = HttpResponse::new(HttpStatus::BAD_REQUEST)
                .with_text("WebSocket upgrade not yet implemented");
            let _ = stream.write_all(&response.to_bytes()).await;
            return;
        }

        // Route the request.
        let response = if let Some(route) = self.find_route(request.method, &request.uri) {
            (route.handler)(request).await
        } else {
            HttpResponse::not_found().with_text("Not Found")
        };

        let _ = stream.write_all(&response.to_bytes()).await;
    }

    /// Start the HTTP server. This method runs indefinitely.
    pub async fn run(self: Arc<Self>) -> HttpResult<()> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        info!(addr = %self.listen_addr, "HTTP server started");

        loop {
            let (stream, remote_addr) = listener.accept().await?;
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                server.handle_connection(stream, remote_addr).await;
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Utility: MIME type detection
// ---------------------------------------------------------------------------

/// Guess a MIME type from a file extension.
pub fn mime_type_for_extension(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "txt" => "text/plain",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_parse() {
        assert_eq!(HttpMethod::from_str("GET"), Some(HttpMethod::Get));
        assert_eq!(HttpMethod::from_str("post"), Some(HttpMethod::Post));
        assert_eq!(HttpMethod::from_str("DELETE"), Some(HttpMethod::Delete));
        assert_eq!(HttpMethod::from_str("FOOBAR"), None);
    }

    #[test]
    fn test_http_status_reason() {
        assert_eq!(HttpStatus::OK.reason(), "OK");
        assert_eq!(HttpStatus::NOT_FOUND.reason(), "Not Found");
        assert_eq!(HttpStatus::UNAUTHORIZED.reason(), "Unauthorized");
    }

    #[test]
    fn test_http_response_serialization() {
        let resp = HttpResponse::ok().with_json(r#"{"status":"ok"}"#);
        let bytes = resp.to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("HTTP/1.1 200 OK"));
        assert!(text.contains("Content-Type: application/json"));
        assert!(text.contains(r#"{"status":"ok"}"#));
    }

    #[test]
    fn test_query_string_parsing() {
        let params = parse_query_string("foo=bar&baz=qux&num=42");
        assert_eq!(params.get("foo").map(|s| s.as_str()), Some("bar"));
        assert_eq!(params.get("baz").map(|s| s.as_str()), Some("qux"));
        assert_eq!(params.get("num").map(|s| s.as_str()), Some("42"));
    }

    #[test]
    fn test_mime_type() {
        assert_eq!(mime_type_for_extension("html"), "text/html");
        assert_eq!(mime_type_for_extension("JSON"), "application/json");
        assert_eq!(mime_type_for_extension("png"), "image/png");
        assert_eq!(mime_type_for_extension("xyz"), "application/octet-stream");
    }

    #[test]
    fn test_auth_config_none() {
        let auth = AuthConfig::None;
        let request = HttpRequest {
            method: HttpMethod::Get,
            uri: "/".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            query_params: HashMap::new(),
            remote_addr: "127.0.0.1:1234".parse().unwrap(),
        };
        assert!(auth.check(&request));
    }

    #[test]
    fn test_auth_config_basic() {
        let auth = AuthConfig::Basic(vec![HttpCredentials {
            username: "admin".to_string(),
            password: "secret".to_string(),
        }]);

        // No auth header -> fail.
        let request = HttpRequest {
            method: HttpMethod::Get,
            uri: "/".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            query_params: HashMap::new(),
            remote_addr: "127.0.0.1:1234".parse().unwrap(),
        };
        assert!(!auth.check(&request));

        // Correct auth -> pass.
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:secret");
        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), format!("Basic {}", encoded));
        let request = HttpRequest {
            method: HttpMethod::Get,
            uri: "/".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            body: Vec::new(),
            query_params: HashMap::new(),
            remote_addr: "127.0.0.1:1234".parse().unwrap(),
        };
        assert!(auth.check(&request));
    }

    #[test]
    fn test_http_response_unauthorized() {
        let resp = HttpResponse::unauthorized();
        let bytes = resp.to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("401 Unauthorized"));
        assert!(text.contains("WWW-Authenticate"));
    }

    #[test]
    fn test_route_matching() {
        let server = HttpServer::new("127.0.0.1:8088".parse().unwrap());
        server.add_route(HttpMethod::Get, "/api/v1", |_req| async {
            HttpResponse::ok().with_json(r#"{"hello":"world"}"#)
        });
        server.add_route(HttpMethod::Post, "/api/v1/channels", |_req| async {
            HttpResponse::ok()
        });

        assert!(server.find_route(HttpMethod::Get, "/api/v1/test").is_some());
        assert!(server
            .find_route(HttpMethod::Post, "/api/v1/channels")
            .is_some());
        assert!(server.find_route(HttpMethod::Get, "/other").is_none());
        assert!(server
            .find_route(HttpMethod::Delete, "/api/v1/test")
            .is_none());
    }
}
