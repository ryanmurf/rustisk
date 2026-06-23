//! OpenTelemetry telemetry integration for Rustisk.
//!
//! Provides distributed tracing capabilities for SIP calls and transactions,
//! enabling observability across hops and systems. Uses OTLP to export traces
//! to observability backends like Jaeger, Zipkin, or cloud providers.

use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{SdkTracerProvider, Sampler};
use opentelemetry_sdk::Resource;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Initialize OpenTelemetry tracing with OTLP exporter and wire it into
/// the `tracing` subscriber as a layer.
///
/// Configuration is done via environment variables:
/// - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (default: http://localhost:4317)
/// - `OTEL_SERVICE_NAME`: Service name (default: rustisk)
///
/// # Returns
///
/// Returns a guard that should be kept alive for the duration of the application.
/// When dropped, it will flush remaining spans and shut down the tracer provider.
pub fn init_telemetry(
    env_filter: tracing_subscriber::EnvFilter,
) -> Result<TelemetryGuard, Box<dyn std::error::Error + Send + Sync>> {
    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "rustisk".to_string());

    let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4317".to_string());

    // Build OTLP span exporter (tonic/gRPC)
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&otlp_endpoint)
        .with_timeout(Duration::from_secs(3))
        .build()?;

    // Build resource
    let resource = Resource::builder()
        .with_service_name(service_name.clone())
        .with_attribute(KeyValue::new("service.version", "0.1.0"))
        .with_attribute(KeyValue::new(
            "service.instance.id",
            uuid::Uuid::new_v4().to_string(),
        ))
        .build();

    // Build tracer provider
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::AlwaysOn)
        .with_resource(resource)
        .build();

    // Get a tracer from the SDK provider (returns SdkTracer, which
    // implements PreSampledTracer -- required by OpenTelemetryLayer).
    use opentelemetry::trace::TracerProvider as _;
    let tracer = provider.tracer("rustisk");

    // Also register as the global provider so that other parts of the
    // codebase can obtain a BoxedTracer via `opentelemetry::global::tracer()`.
    opentelemetry::global::set_tracer_provider(provider.clone());

    // Build the layered tracing subscriber:
    //   fmt layer  (stdout)  +  OpenTelemetry layer  +  env-filter
    let otel_layer = OpenTelemetryLayer::new(tracer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(false).with_thread_ids(false))
        .with(otel_layer)
        .init();

    // Log *after* the subscriber is installed so this actually shows up.
    info!(
        service = %service_name,
        endpoint = %otlp_endpoint,
        "OpenTelemetry telemetry initialized"
    );

    Ok(TelemetryGuard { _provider: provider })
}

/// Guard that ensures proper shutdown of the tracer provider.
/// Keep this alive for the duration of the application.
pub struct TelemetryGuard {
    _provider: SdkTracerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        info!("Shutting down OpenTelemetry telemetry");
        if let Err(e) = self._provider.shutdown() {
            error!(error = %e, "Error shutting down tracer provider");
        }
    }
}

// ---------------------------------------------------------------------------
// SIP trace-context propagation helpers
// ---------------------------------------------------------------------------

/// Format a W3C `traceparent` header value from individual components.
///
/// Format: `{version}-{trace_id}-{span_id}-{trace_flags}`
///
/// See <https://www.w3.org/TR/trace-context/#traceparent-header>
pub fn format_traceparent(trace_id: &TraceId, span_id: &SpanId, flags: TraceFlags) -> String {
    format!("00-{}-{}-{:02x}", trace_id, span_id, flags.to_u8())
}

/// Parse a W3C `traceparent` header value.
///
/// Returns `(TraceId, SpanId, TraceFlags)` on success.
pub fn parse_traceparent(value: &str) -> Option<(TraceId, SpanId, TraceFlags)> {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 4 {
        return None;
    }
    // parts[0] is version, should be "00"
    let trace_id = TraceId::from_hex(parts[1]).ok()?;
    let span_id = SpanId::from_hex(parts[2]).ok()?;
    let flags_byte = u8::from_str_radix(parts[3], 16).ok()?;
    Some((trace_id, span_id, TraceFlags::new(flags_byte)))
}

/// Extract a `SpanContext` from SIP headers carrying W3C trace context.
///
/// Looks for the `traceparent` header (case-insensitive).  Optionally reads
/// `tracestate` as well.
pub fn extract_sip_trace_context(headers: &HashMap<String, String>) -> Option<SpanContext> {
    let tp_value = headers
        .get("traceparent")
        .or_else(|| headers.get("Traceparent"))?;

    let (trace_id, span_id, flags) = parse_traceparent(tp_value)?;

    let trace_state = headers
        .get("tracestate")
        .or_else(|| headers.get("Tracestate"))
        .and_then(|v| {
            TraceState::from_key_value(
                v.split(',')
                    .filter_map(|entry| {
                        let mut kv = entry.splitn(2, '=');
                        Some((kv.next()?.trim().to_string(), kv.next()?.trim().to_string()))
                    }),
            )
            .ok()
        })
        .unwrap_or_default();

    Some(SpanContext::new(
        trace_id,
        span_id,
        flags,
        true, // is_remote
        trace_state,
    ))
}

/// Inject the current trace context into SIP headers as W3C `traceparent`.
pub fn inject_sip_trace_context(
    headers: &mut HashMap<String, String>,
    span_context: &SpanContext,
) {
    if span_context.is_valid() {
        headers.insert(
            "traceparent".to_string(),
            format_traceparent(
                &span_context.trace_id(),
                &span_context.span_id(),
                span_context.trace_flags(),
            ),
        );

        let header_val = span_context.trace_state().header();
        if !header_val.is_empty() {
            headers.insert("tracestate".to_string(), header_val);
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience span constructors (using the `tracing` crate macros so they
// participate in the OpenTelemetry pipeline automatically).
// ---------------------------------------------------------------------------

/// Create a `tracing::Span` for a SIP transaction.
///
/// The returned span is entered automatically by the caller using
/// `let _guard = span.enter();`.
pub fn sip_transaction_span(
    method: &str,
    call_id: &str,
    transaction_id: &str,
    remote_addr: &std::net::SocketAddr,
) -> tracing::Span {
    tracing::info_span!(
        "sip.transaction",
        otel.kind = "server",
        sip.method = %method,
        sip.call_id = %call_id,
        sip.transaction.id = %transaction_id,
        net.peer.ip = %remote_addr.ip(),
        net.peer.port = remote_addr.port(),
    )
}

/// Create a `tracing::Span` for a SIP call (session-level, wrapping
/// multiple transactions such as INVITE / 200 OK / ACK / BYE).
pub fn sip_call_span(
    call_id: &str,
    from_uri: &str,
    to_uri: &str,
) -> tracing::Span {
    tracing::info_span!(
        "sip.call",
        otel.kind = "server",
        sip.call_id = %call_id,
        sip.from.uri = %from_uri,
        sip.to.uri = %to_uri,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_traceparent_roundtrip() {
        let trace_id = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let flags = TraceFlags::new(0x01);

        let header = format_traceparent(&trace_id, &span_id, flags);
        assert_eq!(
            header,
            "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01"
        );

        let (t, s, f) = parse_traceparent(&header).unwrap();
        assert_eq!(t, trace_id);
        assert_eq!(s, span_id);
        assert_eq!(f, flags);
    }

    #[test]
    fn test_inject_extract_roundtrip() {
        let trace_id = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let flags = TraceFlags::new(0x01);

        let ctx = SpanContext::new(trace_id, span_id, flags, false, TraceState::default());

        let mut headers = HashMap::new();
        inject_sip_trace_context(&mut headers, &ctx);
        assert!(headers.contains_key("traceparent"));

        let extracted = extract_sip_trace_context(&headers).unwrap();
        assert_eq!(extracted.trace_id(), trace_id);
        assert_eq!(extracted.span_id(), span_id);
        assert_eq!(extracted.trace_flags(), flags);
    }
}
