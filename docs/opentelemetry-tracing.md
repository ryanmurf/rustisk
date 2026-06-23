# OpenTelemetry Tracing Integration

This document describes the OpenTelemetry tracing implementation added to Rustisk.

## Overview

The implementation provides distributed tracing capabilities for SIP calls and transactions, enabling observability across hops and systems. It includes:

1. **Core telemetry module** (`asterisk-core/src/telemetry.rs`) - Full OpenTelemetry integration with OTLP exporter
2. **SIP tracing utilities** (`asterisk-sip/src/tracing.rs`) - SIP-specific trace context propagation
3. **Transaction tracing** - Automatic span creation for SIP transactions
4. **Main integration** - Telemetry initialization in daemon startup

## Features

### 1. OTLP Exporter Setup
- Configurable OTLP endpoint via environment variables
- Automatic service identification and resource attributes  
- Graceful shutdown handling

### 2. SIP Transaction Tracing
- Automatic span creation for each SIP transaction
- Trace context extraction from incoming SIP messages
- Trace context injection into outgoing SIP messages
- Transaction outcome recording (success/failure/timeout)

### 3. SIP Trace Context Propagation
- Custom X-Trace-* headers for W3C trace context
- Continuation of existing traces across SIP hops
- Automatic trace ID and span ID generation

## Configuration

The telemetry system is configured via environment variables:

- `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (default: http://localhost:4317)
- `OTEL_SERVICE_NAME`: Service name (default: rustisk)
- `OTEL_SERVICE_VERSION`: Service version (default: 0.1.0)
- `OTEL_RESOURCE_ATTRIBUTES`: Additional resource attributes
- `OTEL_DISABLE`: Set to "true" to disable OpenTelemetry

## Usage

### Starting Rustisk with Tracing

```bash
# Start with default OTLP endpoint
./rustisk

# Start with custom endpoint  
OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:14250 ./rustisk

# Start with telemetry disabled
OTEL_DISABLE=true ./rustisk
```

### SIP Message Tracing

The SIP stack automatically handles trace context:

```rust
use asterisk_sip::{inject_trace_headers, extract_trace_headers, generate_trace_id, generate_span_id};

// Extract trace context from incoming SIP message
if let Some(trace_headers) = extract_trace_headers(&sip_message) {
    println!("Continuing trace: {:?}", trace_headers);
}

// Inject trace context into outgoing SIP message
let trace_id = generate_trace_id();
let span_id = generate_span_id();
inject_trace_headers(&mut sip_message, &trace_id, &span_id, 0x01);
```

### Transaction Tracing

SIP transactions automatically create spans:

```rust
// Client transaction automatically extracts/creates trace context
let transaction = ClientTransaction::new(request, remote_addr, branch);

// Get trace context for propagation
let (trace_id, span_id) = transaction.trace_context();

// Inject into response messages
inject_trace_headers(&mut response, trace_id, span_id, 0x01);
```

## Architecture

### Core Components

1. **TracingGuard**: Ensures proper OpenTelemetry shutdown
2. **Span Creation Functions**: Create spans for SIP transactions and calls
3. **Context Propagation**: Extract/inject W3C trace context
4. **SIP Integration**: Transaction-level tracing automation

### Data Flow

```
Incoming SIP Message
     ↓
Extract X-Trace-* Headers
     ↓
Create/Continue Span
     ↓
Process Transaction
     ↓
Create Outgoing Message
     ↓
Inject X-Trace-* Headers
     ↓
Send Message
```

### SIP Headers

The implementation uses custom headers for trace context:
- `X-Trace-Id`: W3C trace ID (32-character hex)
- `X-Span-Id`: W3C span ID (16-character hex)
- `X-Trace-Flags`: W3C trace flags (usually "1" for sampled)
- `X-Trace-State`: W3C trace state (optional)

## Integration Points

### 1. Daemon Startup
- `main.rs`: Initialize telemetry during logging setup
- Keeps `TracingGuard` alive for daemon lifetime

### 2. SIP Transaction Layer
- `transaction/mod.rs`: Automatic span creation for transactions
- Trace context extraction from incoming messages
- Debug logging with trace IDs included

### 3. SIP Message Handling
- `tracing.rs`: Utilities for header injection/extraction
- ID generation for new traces
- Message modification for context propagation

## Testing

### Unit Tests

The implementation includes unit tests for:
- Trace header round-trip (inject → extract)
- ID generation (uniqueness, format)
- SIP message modification

Run tests with:
```bash
cargo test -p asterisk-sip tracing::tests
```

### Integration Testing

To test with a real observability backend:

1. Start Jaeger:
```bash
docker run -d -p 14250:14250 -p 16686:16686 jaegertracing/all-in-one:latest
```

2. Run Rustisk:
```bash  
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:14250 ./rustisk -vvv
```

3. Generate SIP traffic and view traces at http://localhost:16686

## Limitations

### Current Implementation
- Simplified tracing without full OpenTelemetry span lifecycle
- Custom SIP headers (not RFC standard)
- Manual trace context management in some scenarios

### Future Enhancements
- Full OpenTelemetry span lifecycle management
- RFC-compliant trace context propagation
- Metrics collection integration
- Performance optimizations for high-traffic scenarios

## Example Trace Flow

```
INVITE sip:bob@example.com SIP/2.0
Call-ID: abc123@alicephone
X-Trace-Id: 4bf92f3577b34da6a3ce929d0e0e4736
X-Span-Id: 00f067aa0ba902b7
X-Trace-Flags: 1

↓ (Transaction processing)

SIP/2.0 200 OK
Call-ID: abc123@alicephone  
X-Trace-Id: 4bf92f3577b34da6a3ce929d0e0e4736
X-Span-Id: a3ce929d0e0e4736
X-Trace-Flags: 1
```

This creates a distributed trace showing the complete SIP call flow across multiple Asterisk instances or SIP components.
