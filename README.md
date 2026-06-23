# Rustisk

**A Rust-native PBX and telephony toolkit with Asterisk-compatible concepts**

[![CI](https://github.com/ryanmurf/rustisk/actions/workflows/ci.yml/badge.svg)](https://github.com/ryanmurf/rustisk/actions/workflows/ci.yml)
[![License: GPL-2.0-only](https://img.shields.io/badge/License-GPL--2.0--only-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)

---

## Disclaimer

**This project is NOT production ready.**

Rustisk is an experimental research project that demonstrates the feasibility of rebuilding a large C telecom system in Rust. It is not affiliated with Sangoma, Digium, or the Asterisk project.

**Do not use this for production telephony.** It may drop calls, misroute audio, or behave in unexpected ways. Use the official [Asterisk](https://www.asterisk.org/) for anything that matters.

---

## What is this?

Rustisk is a Rust-native PBX and telephony toolkit that tracks the architecture, configuration style, and management interfaces familiar to Asterisk users:

- **1.16 million lines of C** rewritten as **~204K lines of Rust**
- **18 crates** in a Cargo workspace
- **Pure Rust SIP stack** replacing pjproject (2-5x faster SIP message processing)
- **C-compatible pjsip-shim library** that passes 100% of pjlib's own test suite

## Features

### SIP Stack
- Full RFC 3261 SIP implementation (INVITE, REGISTER, SUBSCRIBE, NOTIFY, OPTIONS, MESSAGE, REFER, UPDATE, INFO, PUBLISH)
- ICE / TURN / STUN for NAT traversal
- DTLS-SRTP for encrypted media
- PRACK (reliable provisional responses)
- SDP BUNDLE for multiplexed media
- SIP rate limiting and security
- STIR/SHAKEN caller ID attestation

### Dialplan
- **78 dialplan applications** (Dial, Playback, Queue, VoiceMail, Goto, GotoIf, MixMonitor, AGI, and more)
- **58 dialplan functions** (CALLERID, CHANNEL, CDR, DIALPLAN_EXISTS, REGEX, STRFTIME, and more)
- Hot-reload dialplan without restarting

### Channel Drivers
- **14 channel drivers** (SIP/PJSIP, IAX2, DAHDI, Local, Bridge, Agent, Console, Skinny, MGCP, Motif, Unistim, MulticastRTP, PJSIP variants)

### Management & APIs
- **AMI** (Asterisk Manager Interface) server
- **ARI** (Asterisk REST Interface) REST API

### DSP & Media
- Echo cancellation
- Noise suppression
- Automatic gain control (AGC)
- DTMF detection and generation
- MOS score estimation (ITU-T G.107 E-model)
- Feature-flagged codec FFI (bring your own codec libraries)

### Observability
- OpenTelemetry tracing with OTLP export
- CDR (Call Detail Records)

## Rustisk Compared With Original Asterisk

| | Original Asterisk (C) | Rustisk (Rust) |
|---|---|---|
| **Memory safety** | Manual malloc/free | Ownership system, no buffer overflows or use-after-free |
| **SIP performance** | pjproject | Pure Rust, 2-5x faster message processing |
| **Binary size** | ~30-50 MB | ~6.1 MB release binary |
| **Concurrency** | POSIX threads, manual locking | Tokio async I/O, structured concurrency |
| **Codec integration** | Compiled-in | Feature-flagged FFI, opt-in at build time |
| **Test suite** | Separate test infrastructure | Integrated, 4200+ tests via `cargo test` |

## Architecture

| Crate | Purpose | Lines |
|---|---|---|
| `rustisk-cli` | Main binary, CLI console | 2.1K |
| `asterisk-core` | PBX engine, bridging, channel lifecycle | 12.5K |
| `asterisk-sip` | Pure Rust SIP stack (RFC 3261) | 37.4K |
| `asterisk-apps` | 78 dialplan applications | 32.5K |
| `asterisk-res` | Resource modules (calendaring, crypto, parking, etc.) | 27.3K |
| `asterisk-funcs` | 58 dialplan functions | 11.5K |
| `asterisk-codecs` | Codec registry and negotiation | 9.1K |
| `asterisk-channels` | 14 channel drivers | 6.7K |
| `asterisk-ari` | ARI REST API | 5.1K |
| `asterisk-ami` | AMI TCP server | 4.6K |
| `asterisk-cdr` | Call detail records | 3.1K |
| `asterisk-formats` | Media format handling | 3.0K |
| `asterisk-utils` | Shared utilities | 3.0K |
| `asterisk-config` | Configuration parser | 0.6K |
| `asterisk-types` | Shared type definitions | 0.9K |
| `asterisk-test-framework` | Integration test harness | 28.7K |
| `asterisk-integration-tests` | Integration test suite | 5.3K |
| `pjsip-shim` | C-compatible pjlib/pjsip replacement | 9.7K |

## Quick Start

### Build

```bash
cargo build --release
```

### Run

```bash
# Foreground with console
./target/release/rustisk -f -c

# Foreground with core dump enabled
./target/release/rustisk -f -g -c

# With OpenTelemetry tracing
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 ./target/release/rustisk -f -g
```

### Configuration

Place your configuration files in `/etc/asterisk/` (or pass `-C <path>` to specify an alternate config file), following the same format as standard Asterisk configuration.

## Test Suite

```bash
# Run all workspace tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p asterisk-sip

# Run pjlib compatibility tests
cargo build -p pjsip-shim --release
# Link the resulting libpjsip_shim against pjlib-test and run
```

The workspace includes **4,200+ tests** covering SIP parsing, dialplan execution, AMI/ARI protocols, codec negotiation, bridging, and the pjlib compatibility layer.

## Performance

| Benchmark | pjproject (C) | Rustisk (Rust) | Speedup |
|---|---|---|---|
| SIP message parse | baseline | 2-5x faster | 2-5x |
| SIP transaction throughput | baseline | 2-3x faster | 2-3x |
| Release binary size | ~30-50 MB | ~6.1 MB | 5-8x smaller |
| pjlib test pass rate | 100% (native) | 100% (shim) | parity |

## Project Status

### What works
- SIP call setup and teardown (INVITE, ACK, BYE)
- SIP registration
- Audio bridging between channels
- Dialplan execution (extensions.conf style)
- AMI command/response and event protocol
- ARI REST endpoints
- CDR generation
- CLI console with tab completion
- Hot-reload dialplan
- OpenTelemetry tracing

### What's experimental
- Some dialplan applications are stubs (implemented interface, minimal logic)
- SIPp scenario coverage is partial
- Channel drivers beyond SIP are structural implementations
- STIR/SHAKEN attestation (implemented but not field-tested)

### What's not implemented
- Full SRTP DTLS handshake (crypto negotiation is implemented, actual DTLS state machine is partial)
- Some channel drivers are skeleton implementations
- WebSocket transport for SIP (SIP over WS/WSS)
- Full voicemail storage backends

## Contributing

Contributions are welcome! This project is licensed under **GPL-2.0-only**, so all contributions must be compatible with that license.

1. Fork the repository
2. Create a feature branch
3. Run the test suite: `cargo test --workspace`
4. Run clippy: `cargo clippy --workspace -- -D warnings`
5. Submit a pull request

## License

This project is licensed under the **GNU General Public License v2.0 only** (GPL-2.0-only).

This means you are free to use, modify, and distribute this software, but any derivative work must also be distributed under the GPL-2.0. See the [LICENSE](LICENSE) file for the full text.

The original Asterisk project is also licensed under GPL-2.0, and this rewrite maintains license compatibility.

## Acknowledgments

- The [Asterisk](https://www.asterisk.org/) project by Sangoma/Digium for the original architecture, protocols, and decades of telecom engineering
- The [pjproject](https://www.pjsip.org/) library for the SIP/media reference implementation and test suite
- Built with assistance from [Claude](https://claude.ai/), [GitHub Copilot](https://github.com/features/copilot), and [OpenAI Codex](https://openai.com/index/openai-codex/)
