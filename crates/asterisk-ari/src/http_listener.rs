//! ARI HTTP Listener.
//!
//! Simple HTTP server using tokio TcpListener that bridges incoming
//! HTTP requests to the ARI server's route handler.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use crate::server::{AriAuth, AriRequest, AriResponse, AriServer, HttpMethod};

/// ARI HTTP listener -- binds to a TCP port and routes requests to the ARI server.
pub struct AriHttpListener {
    /// The ARI server instance that handles requests.
    server: Arc<AriServer>,
    /// Address to listen on.
    listen_addr: SocketAddr,
}

impl AriHttpListener {
    /// Create a new HTTP listener for the given ARI server.
    pub fn new(server: Arc<AriServer>, listen_addr: SocketAddr) -> Self {
        Self {
            server,
            listen_addr,
        }
    }

    /// Create from the ARI server's configured bind address.
    pub fn from_config(server: Arc<AriServer>) -> Result<Self, std::io::Error> {
        let addr: SocketAddr = server
            .config
            .bind_address
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        Ok(Self::new(server, addr))
    }

    /// Start listening for HTTP connections. Runs indefinitely.
    pub async fn run(&self) -> Result<(), std::io::Error> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        info!(addr = %self.listen_addr, "ARI HTTP listener started");

        loop {
            match listener.accept().await {
                Ok((stream, remote_addr)) => {
                    let server = Arc::clone(&self.server);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(server, stream, remote_addr).await {
                            debug!(error = %e, remote = %remote_addr, "ARI connection error");
                        }
                    });
                }
                Err(e) => {
                    warn!(error = %e, "ARI accept error");
                }
            }
        }
    }
}

/// Handle a single HTTP connection.
async fn handle_connection(
    server: Arc<AriServer>,
    mut stream: TcpStream,
    remote_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse the HTTP request
    let (method, path, _version, headers, body) = parse_http_request(&mut stream).await?;

    debug!(method = %method_str(&method), path = %path, remote = %remote_addr, "ARI HTTP request");

    // Parse query string
    let (uri_path, query_params) = parse_path_and_query(&path);

    // Authenticate
    let username = authenticate(&server, &headers, &query_params);

    // Build path segments
    let path_segments: Vec<String> = uri_path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    // Build ARI request
    let ari_request = AriRequest {
        method,
        path: uri_path.to_string(),
        path_segments,
        query_params: query_params
            .iter()
            .map(|(k, v)| (k.clone(), vec![v.clone()]))
            .collect(),
        body: if body.is_empty() {
            None
        } else {
            Some(bytes::Bytes::from(body))
        },
        username,
    };

    // Route to ARI server
    let ari_response = server.handle_request(&ari_request);

    // Send HTTP response
    let http_response = format_http_response(&ari_response);
    stream.write_all(http_response.as_bytes()).await?;
    if let Some(ref body) = ari_response.body {
        stream.write_all(body).await?;
    }
    stream.flush().await?;

    Ok(())
}

/// Parse an HTTP request from a stream.
async fn parse_http_request(
    stream: &mut TcpStream,
) -> Result<
    (
        HttpMethod,
        String,
        String,
        HashMap<String, String>,
        Vec<u8>,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let mut reader = BufReader::new(stream);

    // Read request line
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let request_line = request_line.trim_end();

    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() < 3 {
        return Err(format!("invalid request line: {}", request_line).into());
    }

    let method = match parts[0].to_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "DELETE" => HttpMethod::Delete,
        "OPTIONS" => HttpMethod::Options,
        "PATCH" => HttpMethod::Patch,
        _ => return Err(format!("unknown HTTP method: {}", parts[0]).into()),
    };
    let path = parts[1].to_string();
    let version = parts[2].to_string();

    // Read headers
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }

    // Read body based on Content-Length
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    Ok((method, path, version, headers, body))
}

/// Parse path and query string from a URI.
fn parse_path_and_query(uri: &str) -> (&str, HashMap<String, String>) {
    if let Some((path, query)) = uri.split_once('?') {
        let params = parse_query_string(query);
        (path, params)
    } else {
        (uri, HashMap::new())
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

/// Attempt to authenticate the request using Basic auth or api_key query param.
fn authenticate(
    server: &AriServer,
    headers: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
) -> Option<String> {
    // Try Basic auth from Authorization header
    if let Some(auth_header) = headers.get("authorization") {
        if let Some(encoded) = auth_header.strip_prefix("Basic ") {
            use base64::Engine;
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
                if let Ok(decoded_str) = String::from_utf8(decoded) {
                    if let Some((user, pass)) = decoded_str.split_once(':') {
                        let auth = AriAuth::Basic {
                            username: user.to_string(),
                            password: pass.to_string(),
                        };
                        if let Ok(username) = server.authenticate(&auth) {
                            return Some(username);
                        }
                    }
                }
            }
        }
    }

    // Try api_key query parameter
    if let Some(api_key) = query_params.get("api_key") {
        let auth = AriAuth::ApiKey(api_key.clone());
        if let Ok(username) = server.authenticate(&auth) {
            return Some(username);
        }
    }

    None
}

/// Format an ARI response as HTTP.
fn format_http_response(response: &AriResponse) -> String {
    let reason = match response.status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "Unknown",
    };

    let body_len = response.body.as_ref().map(|b| b.len()).unwrap_or(0);

    let mut resp = format!("HTTP/1.1 {} {}\r\n", response.status, reason);
    resp.push_str("Server: Rustisk/0.1.0\r\n");
    resp.push_str(&format!("Content-Type: {}\r\n", response.content_type));
    resp.push_str(&format!("Content-Length: {}\r\n", body_len));
    resp.push_str("Connection: close\r\n");
    resp.push_str("Access-Control-Allow-Origin: *\r\n");
    resp.push_str("\r\n");

    resp
}

fn method_str(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Patch => "PATCH",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_path_and_query() {
        let (path, params) = parse_path_and_query("/ari/channels?api_key=secret&app=myapp");
        assert_eq!(path, "/ari/channels");
        assert_eq!(params.get("api_key"), Some(&"secret".to_string()));
        assert_eq!(params.get("app"), Some(&"myapp".to_string()));
    }

    #[test]
    fn test_parse_path_no_query() {
        let (path, params) = parse_path_and_query("/ari/channels");
        assert_eq!(path, "/ari/channels");
        assert!(params.is_empty());
    }

    #[test]
    fn test_format_http_response() {
        let resp = AriResponse {
            status: 200,
            body: Some(b"{\"ok\":true}".to_vec()),
            content_type: "application/json".to_string(),
        };
        let formatted = format_http_response(&resp);
        assert!(formatted.contains("HTTP/1.1 200 OK"));
        assert!(formatted.contains("Content-Type: application/json"));
        assert!(formatted.contains("Content-Length: 11"));
    }

    #[test]
    fn test_query_string_parsing() {
        let params = parse_query_string("foo=bar&baz=qux");
        assert_eq!(params.get("foo"), Some(&"bar".to_string()));
        assert_eq!(params.get("baz"), Some(&"qux".to_string()));
    }
}
