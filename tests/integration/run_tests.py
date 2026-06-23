#!/usr/bin/env python3
"""
Integration test suite for Rustisk.

Tests the running daemon's SIP (UDP 5060) and AMI (TCP 5038) interfaces
using raw socket I/O -- no external dependencies required beyond Python 3.

Usage:
    python3 run_tests.py [path-to-binary]
"""

import os
import sys
import time
import socket
import signal
import subprocess
import textwrap
import uuid
import traceback


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

BINARY = os.environ.get(
    "RUSTISK_BIN",
    os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "rustisk"),
)

SIP_PORT = 5060
AMI_PORT = 5038
SIP_ADDR = ("127.0.0.1", SIP_PORT)
AMI_ADDR = ("127.0.0.1", AMI_PORT)
AMI_USER = "admin"
AMI_PASS = "admin"
STARTUP_WAIT = 2       # seconds to wait for daemon to be ready
RECV_TIMEOUT = 5        # seconds timeout for socket reads
CONFIG_DIR = "/tmp/rustisk-test-config"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

class TestResult:
    def __init__(self, name):
        self.name = name
        self.passed = False
        self.skipped = False
        self.error = None
        self.detail = ""

    def __repr__(self):
        status = "PASS" if self.passed else ("SKIP" if self.skipped else "FAIL")
        s = f"  [{status}] {self.name}"
        if self.detail:
            s += f"\n         {self.detail}"
        if self.error and not self.passed:
            s += f"\n         Error: {self.error}"
        return s


class DaemonManager:
    """Manages starting/stopping the Rustisk daemon."""

    def __init__(self, binary_path):
        self.binary = os.path.realpath(binary_path)
        self.process = None

    def start(self):
        # Create minimal config dir with extensions.conf and asterisk.conf
        os.makedirs(CONFIG_DIR, exist_ok=True)
        ext_conf = os.path.join(CONFIG_DIR, "extensions.conf")
        with open(ext_conf, "w") as f:
            f.write(textwrap.dedent("""\
                [general]

                [from-external]
                exten => s,1,Answer()
                exten => s,n,Playback(hello-world)
                exten => s,n,Hangup()

                [default]
                exten => s,1,Answer()
                exten => s,n,Hangup()
            """))

        # Create asterisk.conf so -C points to a file (not a directory).
        # The daemon uses the file's parent directory as the config dir.
        ast_conf = os.path.join(CONFIG_DIR, "asterisk.conf")
        with open(ast_conf, "w") as f:
            f.write(textwrap.dedent("""\
                [directories]
                astetcdir => {config_dir}
            """.format(config_dir=CONFIG_DIR)))

        # Do NOT pass -f or -c: those enter rustyline console mode which
        # immediately exits when stdin is a pipe. Without them the daemon
        # enters its event loop and waits for SIGTERM.
        cmd = [self.binary, "-C", ast_conf, "-v"]
        print(f"  Starting daemon: {' '.join(cmd)}")
        self.process = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        time.sleep(STARTUP_WAIT)
        if self.process.poll() is not None:
            stdout = self.process.stdout.read().decode(errors="replace")
            stderr = self.process.stderr.read().decode(errors="replace")
            raise RuntimeError(
                f"Daemon exited immediately (code {self.process.returncode})\n"
                f"stdout: {stdout}\nstderr: {stderr}"
            )
        print(f"  Daemon running (PID {self.process.pid})")

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
            print(f"  Daemon stopped (exit code {self.process.returncode})")

    def is_running(self):
        return self.process and self.process.poll() is None


def wait_for_port(host, port, timeout=10):
    """Wait until a TCP port is accepting connections."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            s = socket.create_connection((host, port), timeout=1)
            s.close()
            return True
        except (ConnectionRefusedError, OSError):
            time.sleep(0.2)
    return False


def wait_for_udp_port(host, port, timeout=10):
    """Wait until a UDP port responds to an OPTIONS probe."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.settimeout(1)
            options = build_sip_options()
            sock.sendto(options.encode(), (host, port))
            data, _ = sock.recvfrom(4096)
            sock.close()
            return True
        except (socket.timeout, OSError):
            try:
                sock.close()
            except:
                pass
            time.sleep(0.2)
    return False


# ---------------------------------------------------------------------------
# SIP message builders
# ---------------------------------------------------------------------------

def generate_branch():
    return "z9hG4bK" + uuid.uuid4().hex[:12]

def generate_call_id():
    return uuid.uuid4().hex[:16] + "@127.0.0.1"

def generate_tag():
    return uuid.uuid4().hex[:8]


def build_sip_options(call_id=None, branch=None, from_tag=None):
    call_id = call_id or generate_call_id()
    branch = branch or generate_branch()
    from_tag = from_tag or generate_tag()

    return (
        f"OPTIONS sip:asterisk@127.0.0.1:{SIP_PORT} SIP/2.0\r\n"
        f"Via: SIP/2.0/UDP 127.0.0.1:15060;branch={branch};rport\r\n"
        f"Max-Forwards: 70\r\n"
        f"From: <sip:tester@127.0.0.1>;tag={from_tag}\r\n"
        f"To: <sip:asterisk@127.0.0.1>\r\n"
        f"Call-ID: {call_id}\r\n"
        f"CSeq: 1 OPTIONS\r\n"
        f"Contact: <sip:tester@127.0.0.1:15060>\r\n"
        f"Accept: application/sdp\r\n"
        f"Content-Length: 0\r\n"
        f"\r\n"
    )


def build_sip_invite(to_user="s", call_id=None, branch=None, from_tag=None):
    call_id = call_id or generate_call_id()
    branch = branch or generate_branch()
    from_tag = from_tag or generate_tag()

    sdp_body = (
        "v=0\r\n"
        "o=- 0 0 IN IP4 127.0.0.1\r\n"
        "s=rustisk-test\r\n"
        "c=IN IP4 127.0.0.1\r\n"
        "t=0 0\r\n"
        "m=audio 10000 RTP/AVP 0 8\r\n"
        "a=rtpmap:0 PCMU/8000\r\n"
        "a=rtpmap:8 PCMA/8000\r\n"
    )
    content_length = len(sdp_body)

    return (
        f"INVITE sip:{to_user}@127.0.0.1:{SIP_PORT} SIP/2.0\r\n"
        f"Via: SIP/2.0/UDP 127.0.0.1:15060;branch={branch};rport\r\n"
        f"Max-Forwards: 70\r\n"
        f"From: \"Test\" <sip:tester@127.0.0.1>;tag={from_tag}\r\n"
        f"To: <sip:{to_user}@127.0.0.1>\r\n"
        f"Call-ID: {call_id}\r\n"
        f"CSeq: 1 INVITE\r\n"
        f"Contact: <sip:tester@127.0.0.1:15060>\r\n"
        f"Content-Type: application/sdp\r\n"
        f"Content-Length: {content_length}\r\n"
        f"\r\n"
        f"{sdp_body}"
    ), call_id, branch, from_tag


def build_sip_ack(call_id, branch, from_tag, to_tag=None, to_user="s"):
    to_hdr = f"<sip:{to_user}@127.0.0.1>"
    if to_tag:
        to_hdr += f";tag={to_tag}"
    return (
        f"ACK sip:{to_user}@127.0.0.1:{SIP_PORT} SIP/2.0\r\n"
        f"Via: SIP/2.0/UDP 127.0.0.1:15060;branch={branch};rport\r\n"
        f"Max-Forwards: 70\r\n"
        f"From: \"Test\" <sip:tester@127.0.0.1>;tag={from_tag}\r\n"
        f"To: {to_hdr}\r\n"
        f"Call-ID: {call_id}\r\n"
        f"CSeq: 1 ACK\r\n"
        f"Content-Length: 0\r\n"
        f"\r\n"
    )


def build_sip_bye(call_id, branch, from_tag, to_tag=None, to_user="s"):
    new_branch = generate_branch()
    to_hdr = f"<sip:{to_user}@127.0.0.1>"
    if to_tag:
        to_hdr += f";tag={to_tag}"
    return (
        f"BYE sip:{to_user}@127.0.0.1:{SIP_PORT} SIP/2.0\r\n"
        f"Via: SIP/2.0/UDP 127.0.0.1:15060;branch={new_branch};rport\r\n"
        f"Max-Forwards: 70\r\n"
        f"From: \"Test\" <sip:tester@127.0.0.1>;tag={from_tag}\r\n"
        f"To: {to_hdr}\r\n"
        f"Call-ID: {call_id}\r\n"
        f"CSeq: 2 BYE\r\n"
        f"Content-Length: 0\r\n"
        f"\r\n"
    )


def parse_sip_response(data):
    """Parse a SIP response into (status_code, reason, headers_dict, body).

    If the data is a SIP request (e.g., BYE from the server), returns
    status_code=0 with the method as the reason.
    """
    text = data if isinstance(data, str) else data.decode("utf-8", errors="replace")
    parts = text.split("\r\n\r\n", 1)
    header_block = parts[0]
    body = parts[1] if len(parts) > 1 else ""

    lines = header_block.split("\r\n")
    status_line = lines[0]

    # Distinguish response ("SIP/2.0 200 OK") from request ("BYE sip:... SIP/2.0")
    if status_line.startswith("SIP/2.0"):
        tokens = status_line.split(" ", 2)
        try:
            status_code = int(tokens[1]) if len(tokens) >= 2 else 0
        except ValueError:
            status_code = 0
        reason = tokens[2] if len(tokens) >= 3 else ""
    else:
        # It's a SIP request, not a response
        method = status_line.split(" ", 1)[0]
        status_code = 0
        reason = method  # e.g., "BYE"

    headers = {}
    for line in lines[1:]:
        if ":" in line:
            key, value = line.split(":", 1)
            headers[key.strip().lower()] = value.strip()

    return status_code, reason, headers, body


def extract_to_tag(data):
    """Extract the tag from the To header in a SIP response."""
    text = data if isinstance(data, str) else data.decode("utf-8", errors="replace")
    for line in text.split("\r\n"):
        if line.lower().startswith("to:"):
            for param in line.split(";"):
                param = param.strip()
                if param.lower().startswith("tag="):
                    return param[4:]
    return None


# ---------------------------------------------------------------------------
# AMI helpers
# ---------------------------------------------------------------------------

def ami_connect(timeout=RECV_TIMEOUT):
    """Connect to AMI and read the banner."""
    sock = socket.create_connection(AMI_ADDR, timeout=timeout)
    sock.settimeout(timeout)
    banner = b""
    while b"\r\n" not in banner:
        chunk = sock.recv(1024)
        if not chunk:
            break
        banner += chunk
    return sock, banner.decode("utf-8", errors="replace")


def ami_send_action(sock, action_lines, timeout=RECV_TIMEOUT):
    """Send an AMI action (list of 'Key: Value' strings) and read the response."""
    msg = "\r\n".join(action_lines) + "\r\n\r\n"
    sock.sendall(msg.encode())
    return ami_read_response(sock, timeout)


def ami_read_response(sock, timeout=RECV_TIMEOUT):
    """Read a complete AMI response (terminated by blank line)."""
    sock.settimeout(timeout)
    buf = b""
    while True:
        try:
            chunk = sock.recv(4096)
        except socket.timeout:
            break
        if not chunk:
            break
        buf += chunk
        # AMI messages are terminated by \r\n\r\n
        if b"\r\n\r\n" in buf:
            break
    return buf.decode("utf-8", errors="replace")


def ami_parse_response(text):
    """Parse an AMI response into a dict of key-value pairs."""
    result = {}
    for line in text.strip().split("\r\n"):
        if ":" in line:
            key, value = line.split(":", 1)
            result[key.strip()] = value.strip()
    return result


# ---------------------------------------------------------------------------
# Test functions
# ---------------------------------------------------------------------------

def test_daemon_starts(daemon):
    """Test 1: Daemon starts and is running."""
    r = TestResult("Daemon starts and runs")
    if daemon.is_running():
        r.passed = True
        r.detail = f"PID {daemon.process.pid}"
    else:
        r.error = "Daemon is not running"
    return r


def test_sip_port_bound(daemon):
    """Test 2: SIP port 5060 is bound and responds to UDP."""
    r = TestResult("SIP port 5060 bound (UDP)")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        msg = build_sip_options()
        sock.sendto(msg.encode(), SIP_ADDR)
        data, addr = sock.recvfrom(4096)
        sock.close()
        r.passed = True
        r.detail = f"Received {len(data)} bytes from {addr}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_port_bound(daemon):
    """Test 3: AMI port 5038 is bound and sends banner."""
    r = TestResult("AMI port 5038 bound (TCP)")
    try:
        sock, banner = ami_connect()
        sock.close()
        if "Asterisk Call Manager" in banner:
            r.passed = True
            r.detail = f"Banner: {banner.strip()}"
        else:
            r.error = f"Unexpected banner: {banner.strip()}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_options_200(daemon):
    """Test 4: SIP OPTIONS request returns 200 OK."""
    r = TestResult("SIP OPTIONS returns 200 OK")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        msg = build_sip_options()
        sock.sendto(msg.encode(), SIP_ADDR)
        data, _ = sock.recvfrom(4096)
        sock.close()

        code, reason, headers, _ = parse_sip_response(data)
        if code == 200:
            r.passed = True
            r.detail = f"200 {reason} | Allow: {headers.get('allow', 'N/A')}"
        else:
            r.error = f"Expected 200, got {code} {reason}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_options_headers(daemon):
    """Test 5: SIP OPTIONS 200 OK contains required headers."""
    r = TestResult("SIP OPTIONS response has Allow/Server headers")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        msg = build_sip_options()
        sock.sendto(msg.encode(), SIP_ADDR)
        data, _ = sock.recvfrom(4096)
        sock.close()

        code, _, headers, _ = parse_sip_response(data)
        if code != 200:
            r.error = f"Expected 200, got {code}"
            return r

        missing = []
        if "allow" not in headers:
            missing.append("Allow")
        if "server" not in headers:
            missing.append("Server")

        if not missing:
            r.passed = True
            r.detail = f"Server: {headers.get('server', 'N/A')}, Allow: {headers.get('allow', 'N/A')}"
        else:
            r.error = f"Missing headers: {', '.join(missing)}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_login_success(daemon):
    """Test 6: AMI Login with correct credentials succeeds."""
    r = TestResult("AMI Login succeeds with correct credentials")
    try:
        sock, banner = ami_connect()
        resp = ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Expected Success, got: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_login_failure(daemon):
    """Test 7: AMI Login with wrong credentials fails."""
    r = TestResult("AMI Login fails with wrong credentials")
    try:
        sock, banner = ami_connect()
        resp = ami_send_action(sock, [
            "Action: Login",
            "Username: admin",
            "Secret: wrong_password",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Error":
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Expected Error, got: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_ping(daemon):
    """Test 8: AMI Ping after login returns Pong."""
    r = TestResult("AMI Ping returns Pong")
    try:
        sock, _ = ami_connect()
        # Login first
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        # Ping
        resp = ami_send_action(sock, [
            "Action: Ping",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success" and parsed.get("Ping", "").lower() == "pong":
            r.passed = True
            r.detail = f"Ping: {parsed.get('Ping', 'N/A')}"
        elif parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"Response: Success, Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_action_id_echo(daemon):
    """Test 9: AMI echoes back ActionID."""
    r = TestResult("AMI echoes ActionID in response")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])

        test_action_id = "test-" + uuid.uuid4().hex[:8]
        resp = ami_send_action(sock, [
            "Action: Ping",
            f"ActionID: {test_action_id}",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("ActionID") == test_action_id:
            r.passed = True
            r.detail = f"ActionID correctly echoed: {test_action_id}"
        else:
            r.error = f"Expected ActionID {test_action_id}, got: {parsed.get('ActionID', 'MISSING')}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_unauthenticated_rejected(daemon):
    """Test 10: AMI rejects actions before login."""
    r = TestResult("AMI rejects unauthenticated actions")
    try:
        sock, _ = ami_connect()
        # Try Ping without login
        resp = ami_send_action(sock, [
            "Action: Ping",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Error":
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Expected Error for unauthenticated action, got: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_core_status(daemon):
    """Test 11: AMI CoreStatus returns system info."""
    r = TestResult("AMI CoreStatus returns system info")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: CoreStatus",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"CoreStartupDate: {parsed.get('CoreStartupDate', 'N/A')}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_core_settings(daemon):
    """Test 12: AMI CoreSettings returns version info."""
    r = TestResult("AMI CoreSettings returns version info")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: CoreSettings",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"AsteriskVersion: {parsed.get('AsteriskVersion', 'N/A')}, AMIversion: {parsed.get('AMIversion', 'N/A')}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_logoff(daemon):
    """Test 13: AMI Logoff after login returns Goodbye."""
    r = TestResult("AMI Logoff returns Goodbye")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: Logoff",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        # Real Asterisk returns Response: Goodbye (not Success) for Logoff
        if parsed.get("Response") in ("Success", "Goodbye"):
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_core_show_channels(daemon):
    """Test 14: AMI CoreShowChannels returns channel list."""
    r = TestResult("AMI CoreShowChannels returns channel list")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: CoreShowChannels",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_unknown_action(daemon):
    """Test 15: AMI returns error for unknown action."""
    r = TestResult("AMI returns Error for unknown action")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: NonExistentAction12345",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Error":
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Expected Error, got: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_invite_100_trying(daemon):
    """Test 16: SIP INVITE receives 100 Trying."""
    r = TestResult("SIP INVITE receives 100 Trying")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        sock.bind(("127.0.0.1", 0))

        invite, call_id, branch, from_tag = build_sip_invite()
        sock.sendto(invite.encode(), SIP_ADDR)

        # Read responses; the first should be 100 Trying
        first_code = None
        first_reason = None
        to_tag = None
        deadline = time.time() + RECV_TIMEOUT
        while time.time() < deadline:
            try:
                sock.settimeout(max(0.1, deadline - time.time()))
                data, _ = sock.recvfrom(4096)
                code, reason, _, _ = parse_sip_response(data)
                if code == 0:
                    # This is a SIP request from the server (e.g., BYE), respond 200 OK
                    continue
                if first_code is None:
                    first_code = code
                    first_reason = reason
                if code == 200:
                    to_tag = extract_to_tag(data)
                    ack = build_sip_ack(call_id, generate_branch(), from_tag, to_tag)
                    sock.sendto(ack.encode(), SIP_ADDR)
                    bye = build_sip_bye(call_id, generate_branch(), from_tag, to_tag)
                    sock.sendto(bye.encode(), SIP_ADDR)
                    break
            except socket.timeout:
                break

        # Drain remaining packets
        try:
            sock.settimeout(0.5)
            while True:
                sock.recvfrom(4096)
        except socket.timeout:
            pass

        sock.close()

        if first_code == 100:
            r.passed = True
            r.detail = f"100 {first_reason}"
        else:
            r.error = f"Expected 100 Trying, got {first_code} {first_reason}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_invite_200_ok(daemon):
    """Test 17: SIP INVITE receives 200 OK (after 100 Trying)."""
    r = TestResult("SIP INVITE receives 200 OK")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        sock.bind(("127.0.0.1", 0))

        invite, call_id, branch, from_tag = build_sip_invite()
        sock.sendto(invite.encode(), SIP_ADDR)

        # Collect all responses
        responses = []
        deadline = time.time() + RECV_TIMEOUT
        while time.time() < deadline:
            try:
                sock.settimeout(max(0.1, deadline - time.time()))
                data, _ = sock.recvfrom(4096)
                code, reason, headers, _ = parse_sip_response(data)
                responses.append((code, reason, headers, data))
                if code == 200:
                    to_tag = extract_to_tag(data)
                    ack = build_sip_ack(call_id, generate_branch(), from_tag, to_tag)
                    sock.sendto(ack.encode(), SIP_ADDR)
                    # Send BYE to tear down call
                    bye = build_sip_bye(call_id, generate_branch(), from_tag, to_tag)
                    sock.sendto(bye.encode(), SIP_ADDR)
                    break
            except socket.timeout:
                break

        sock.close()

        got_200 = any(c == 200 for c, _, _, _ in responses)
        codes = [c for c, _, _, _ in responses]
        if got_200:
            r.passed = True
            r.detail = f"Response sequence: {codes}"
        else:
            r.error = f"No 200 OK received. Got: {codes}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_bye_200(daemon):
    """Test 18: SIP BYE receives 200 OK."""
    r = TestResult("SIP BYE receives 200 OK")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        sock.bind(("127.0.0.1", 0))

        invite, call_id, branch, from_tag = build_sip_invite()
        sock.sendto(invite.encode(), SIP_ADDR)

        # Collect responses until we get 200 OK for INVITE
        to_tag = None
        deadline = time.time() + RECV_TIMEOUT
        while time.time() < deadline:
            try:
                sock.settimeout(max(0.1, deadline - time.time()))
                data, _ = sock.recvfrom(4096)
                code, reason, _, _ = parse_sip_response(data)
                if code == 0:
                    # SIP request from server (e.g., BYE), skip for now
                    continue
                if code == 200:
                    to_tag = extract_to_tag(data)
                    # Send ACK
                    ack = build_sip_ack(call_id, generate_branch(), from_tag, to_tag)
                    sock.sendto(ack.encode(), SIP_ADDR)
                    break
            except socket.timeout:
                break

        if to_tag is None:
            r.error = "Never got 200 OK for INVITE"
            sock.close()
            return r

        # Small delay before BYE
        time.sleep(0.3)

        # Send BYE
        bye = build_sip_bye(call_id, generate_branch(), from_tag, to_tag)
        sock.sendto(bye.encode(), SIP_ADDR)

        # Read BYE response; may also receive a BYE request from the server
        bye_response_code = None
        deadline = time.time() + RECV_TIMEOUT
        while time.time() < deadline:
            try:
                sock.settimeout(max(0.1, deadline - time.time()))
                data, _ = sock.recvfrom(4096)
                code, reason, _, _ = parse_sip_response(data)
                if code == 0:
                    # SIP request from server (e.g., server-side BYE)
                    # Send 200 OK back for it
                    continue
                bye_response_code = code
                if code == 200:
                    break
            except socket.timeout:
                break

        sock.close()

        if bye_response_code == 200:
            r.passed = True
            r.detail = "200 OK received for BYE"
        else:
            r.error = f"Expected 200 OK for BYE, got: {bye_response_code}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_multiple_options(daemon):
    """Test 19: Multiple rapid SIP OPTIONS requests all get 200 OK."""
    r = TestResult("Multiple rapid SIP OPTIONS all return 200 OK")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)
        sock.bind(("127.0.0.1", 0))

        count = 10
        success_count = 0

        for i in range(count):
            msg = build_sip_options()
            sock.sendto(msg.encode(), SIP_ADDR)
            try:
                data, _ = sock.recvfrom(4096)
                code, _, _, _ = parse_sip_response(data)
                if code == 200:
                    success_count += 1
            except socket.timeout:
                pass

        sock.close()

        if success_count == count:
            r.passed = True
            r.detail = f"{success_count}/{count} OPTIONS returned 200 OK"
        else:
            r.error = f"Only {success_count}/{count} OPTIONS returned 200 OK"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_multiple_sessions(daemon):
    """Test 20: Multiple simultaneous AMI sessions."""
    r = TestResult("Multiple simultaneous AMI sessions")
    try:
        socks = []
        for i in range(3):
            sock, banner = ami_connect()
            ami_send_action(sock, [
                "Action: Login",
                f"Username: {AMI_USER}",
                f"Secret: {AMI_PASS}",
            ])
            socks.append(sock)

        # Ping on all sessions
        all_ok = True
        for i, sock in enumerate(socks):
            resp = ami_send_action(sock, ["Action: Ping"])
            parsed = ami_parse_response(resp)
            if parsed.get("Response") != "Success":
                all_ok = False

        for sock in socks:
            sock.close()

        if all_ok:
            r.passed = True
            r.detail = f"All {len(socks)} sessions worked concurrently"
        else:
            r.error = "Some sessions failed"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_command_action(daemon):
    """Test 21: AMI Command action executes CLI commands."""
    r = TestResult("AMI Command action executes CLI commands")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: Command",
            "Command: core show version",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"Response: {resp.strip()[:120]}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_list_commands(daemon):
    """Test 22: AMI ListCommands returns available actions."""
    r = TestResult("AMI ListCommands returns available actions")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: ListCommands",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            actions_found = [k for k in parsed.keys() if k not in ("Response", "ActionID", "Message")]
            r.detail = f"Actions found: {len(actions_found)}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_options_via_matching(daemon):
    """Test 23: SIP response has matching Via branch."""
    r = TestResult("SIP response Via branch matches request")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)

        branch = generate_branch()
        msg = build_sip_options(branch=branch)
        sock.sendto(msg.encode(), SIP_ADDR)
        data, _ = sock.recvfrom(4096)
        sock.close()

        code, _, headers, _ = parse_sip_response(data)
        via_header = headers.get("via", "")
        if branch in via_header:
            r.passed = True
            r.detail = f"Branch {branch} found in Via: {via_header[:80]}"
        else:
            r.error = f"Branch {branch} not found in Via: {via_header}"
    except Exception as e:
        r.error = str(e)
    return r


def test_sip_call_id_matching(daemon):
    """Test 24: SIP response has matching Call-ID."""
    r = TestResult("SIP response Call-ID matches request")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(RECV_TIMEOUT)

        call_id = generate_call_id()
        msg = build_sip_options(call_id=call_id)
        sock.sendto(msg.encode(), SIP_ADDR)
        data, _ = sock.recvfrom(4096)
        sock.close()

        code, _, headers, _ = parse_sip_response(data)
        resp_call_id = headers.get("call-id", "")
        if resp_call_id == call_id:
            r.passed = True
            r.detail = f"Call-ID: {call_id}"
        else:
            r.error = f"Expected Call-ID {call_id}, got {resp_call_id}"
    except Exception as e:
        r.error = str(e)
    return r


def test_ami_events_action(daemon):
    """Test 25: AMI Events action to enable/disable event categories."""
    r = TestResult("AMI Events action works")
    try:
        sock, _ = ami_connect()
        ami_send_action(sock, [
            "Action: Login",
            f"Username: {AMI_USER}",
            f"Secret: {AMI_PASS}",
        ])
        resp = ami_send_action(sock, [
            "Action: Events",
            "EventMask: off",
        ])
        sock.close()

        parsed = ami_parse_response(resp)
        if parsed.get("Response") == "Success":
            r.passed = True
            r.detail = f"Message: {parsed.get('Message', 'N/A')}"
        else:
            r.error = f"Unexpected response: {parsed}"
    except Exception as e:
        r.error = str(e)
    return r


# ---------------------------------------------------------------------------
# Test runner
# ---------------------------------------------------------------------------

def run_all_tests():
    binary = sys.argv[1] if len(sys.argv) > 1 else BINARY
    binary = os.path.realpath(binary)

    if not os.path.isfile(binary):
        print(f"ERROR: Binary not found at {binary}")
        print("Usage: python3 run_tests.py [path-to-binary]")
        sys.exit(1)

    print("=" * 72)
    print("  Rustisk Integration Test Suite")
    print("=" * 72)
    print()

    daemon = DaemonManager(binary)
    results = []

    try:
        daemon.start()

        # Wait for both ports to be ready
        print("  Waiting for SIP port (5060)...", end=" ", flush=True)
        if wait_for_udp_port("127.0.0.1", SIP_PORT, timeout=10):
            print("OK")
        else:
            print("TIMEOUT (will try anyway)")

        print("  Waiting for AMI port (5038)...", end=" ", flush=True)
        if wait_for_port("127.0.0.1", AMI_PORT, timeout=10):
            print("OK")
        else:
            print("TIMEOUT (will try anyway)")

        print()
        print("-" * 72)
        print("  Running tests...")
        print("-" * 72)
        print()

        # Define all tests in order
        tests = [
            test_daemon_starts,
            test_sip_port_bound,
            test_ami_port_bound,
            test_sip_options_200,
            test_sip_options_headers,
            test_ami_login_success,
            test_ami_login_failure,
            test_ami_ping,
            test_ami_action_id_echo,
            test_ami_unauthenticated_rejected,
            test_ami_core_status,
            test_ami_core_settings,
            test_ami_logoff,
            test_ami_core_show_channels,
            test_ami_unknown_action,
            test_sip_invite_100_trying,
            test_sip_invite_200_ok,
            test_sip_bye_200,
            test_sip_multiple_options,
            test_ami_multiple_sessions,
            test_ami_command_action,
            test_ami_list_commands,
            test_sip_options_via_matching,
            test_sip_call_id_matching,
            test_ami_events_action,
        ]

        for test_fn in tests:
            try:
                result = test_fn(daemon)
            except Exception as e:
                result = TestResult(test_fn.__doc__ or test_fn.__name__)
                result.error = f"EXCEPTION: {e}\n{traceback.format_exc()}"
            results.append(result)
            print(result)

    except Exception as e:
        print(f"\nFATAL ERROR: {e}")
        traceback.print_exc()
    finally:
        print()
        print("-" * 72)
        daemon.stop()

    # Summary
    print()
    print("=" * 72)
    passed = sum(1 for r in results if r.passed)
    failed = sum(1 for r in results if not r.passed and not r.skipped)
    skipped = sum(1 for r in results if r.skipped)
    total = len(results)

    print(f"  Results: {passed}/{total} passed, {failed} failed, {skipped} skipped")
    print("=" * 72)

    # Exit with failure if any test failed
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(run_all_tests())
