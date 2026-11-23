#!/usr/bin/env python3
"""Hydra acceptance-test file server (pure stdlib, threaded).

Serves one local file for any request path (canonical URL: "/") so the
headless speedtest harness can exercise the downloader's HTTP behaviours:

  --mode range   Full HTTP/1.1 Range support: ranged GET -> 206 with
                 Content-Range and Accept-Ranges: bytes; unsatisfiable or
                 suffix-0 ranges -> 416. HEAD is REJECTED with 405 so the
                 client must fall back to its ranged-GET probe.
  --mode norange ALWAYS 200 with the full body (Range ignored) plus
                 Accept-Ranges: none; HEAD answers 200 with true headers,
                 forcing the client's single-stream fallback.
  --mode oversend range mode, PLUS a 206 whose body is LONGER than the range
                 it declared in Content-Range (Content-Length covers the
                 excess, so the client really does receive it). Pins the
                 downloader's per-chunk byte cap: a client without one writes
                 past the chunk end and corrupts its neighbour.
  --mode stall   range mode, PLUS up to --stall-requests responses get their
                 headers plus --stall-prefix bytes and then silence (no FIN, no
                 RST) for --stall-secs. Once the budget is spent every response
                 is served in full, so a client with a read timeout converges
                 and one without hangs forever. Pins the stall timeout.
  --mode noadvertise  HEAD answers 200 with a truthful Content-Length but NO
                 Accept-Ranges header, while ranged GETs really do return 206.
                 Pins the active range verification: a client that trusts the
                 missing header would settle for one connection.
  --mode nolength  no Content-Length anywhere and a close-delimited body, so the
                 probe cannot learn the size. Pins the unknown-size path
                 (single sequential stream, no sidecar, no resume).
  --mode flaky   range mode, PLUS drops the connection mid-body with
                 probability --drop-p after sending a random 0-256 KiB
                 prefix. The server remembers the last dropped byte-range
                 and never drops the same range twice in a row, so client
                 retries always converge. Ranges of length 1 (probe
                 "bytes=0-0") are never dropped.

Every response carries Content-Type: application/octet-stream, a true
Content-Length, and Connection: close. The server is quiet (no per-request
logging), robust against client aborts, and handles 16+ concurrent
connections. The file is served at "/" regardless of its on-disk name.

Examples:
    python3 scripts/testserver.py --mode range   --port 8899 --file f.bin
    python3 scripts/testserver.py --mode norange --port 8901 --file f.bin
    python3 scripts/testserver.py --mode flaky   --port 8902 --file f.bin --drop-p 0.2
"""

import argparse
import mmap
import random
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CHUNK = 64 * 1024        # body streaming granularity
MAX_PREFIX = 256 * 1024  # max bytes sent before a flaky drop
OVERSEND_BYTES = 64 * 1024  # extra bytes appended to a 206 in oversend mode
STALL_PREFIX = 4096         # bytes sent before going silent in stall mode
# Bytes per second PER CONNECTION (0 = unlimited). Lets a test hold a transfer
# open long enough to cancel or interrupt it with real partial data on disk.
RATE_BYTES = 0

# Filled in by main().
MODE = "range"
DATA = b""
SIZE = 0
DROP_P = 0.15
STALL_SECS = 45
RNG = random.Random()

# Stall-mode memory.
#
# STALLED remembers ranges that already stalled, and STALL_BUDGET caps how many
# stalls the whole run may produce. The budget is what makes the fixture converge:
# a client that flushes what it received before retrying (correctly) asks for a
# SHIFTED range next time, which the per-range memory alone would stall forever.
STALLED = set()
STALL_BUDGET = 4

# Flaky-mode per-server memory: the most recently dropped (start, end).
LAST_DROPPED = None
STATE_LOCK = threading.Lock()


class QuietServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def handle_error(self, request, client_address):
        # Client aborts / reset connections must not spam output.
        pass


class QuietHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "HydraTestServer/1.0"
    timeout = 60  # drop dead or stalled sockets

    # ---- silence ---------------------------------------------------------
    def log_message(self, fmt, *args):
        pass

    def log_error(self, fmt, *args):
        pass

    # ---- response helpers --------------------------------------------------
    def _common_headers(self, length):
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(length))
        self.send_header("Connection", "close")

    def _reject(self, code, allow):
        self.send_response(code)
        self.send_header("Allow", allow)
        self._common_headers(0)
        self.end_headers()

    # ---- request routing ------------------------------------------------
    def do_HEAD(self):
        if MODE == "noadvertise":
            # Truthful length, but this origin simply never advertises ranges.
            self.send_response(200)
            self._common_headers(SIZE)
            self.end_headers()
        elif MODE == "nolength":
            # Close-delimited body: no length is learnable from HEAD either.
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Connection", "close")
            self.end_headers()
        elif MODE == "norange":
            self.send_response(200)
            self._common_headers(SIZE)
            self.send_header("Accept-Ranges", "none")
            self.end_headers()
        else:
            # range + flaky: force the ranged-GET probe fallback path.
            self._reject(405, "GET")
        self.close_connection = True

    def do_GET(self):
        try:
            self._serve_get()
        except OSError:
            # Client aborted mid-response: not an error for us.
            pass
        finally:
            self.close_connection = True

    def _method_not_allowed(self):
        self._reject(405, "GET, HEAD" if MODE == "norange" else "GET")

    do_POST = _method_not_allowed
    do_PUT = _method_not_allowed
    do_DELETE = _method_not_allowed
    do_PATCH = _method_not_allowed

    # ---- range parsing ----------------------------------------------------
    @staticmethod
    def _parse_range(value):
        """Parse a Range header value.

        Returns ("full",)              - no header: whole file with 200
                ("bad",)               - syntactically invalid: ignore it
                ("unsat",)             - unsatisfiable: 416
                ("range", start, end)  - inclusive bounds to serve with 206

        Only the first byte-range spec is honoured (that is all the
        downloader ever sends). "bad" -> 200 per RFC 7233.
        """
        if not value:
            return ("full",)
        spec = value.strip().split(",", 1)[0].strip()
        if not spec.lower().startswith("bytes="):
            return ("full",)
        body = spec[len("bytes="):].strip()
        if "-" not in body:
            return ("bad",)
        first, last = body.split("-", 1)
        first, last = first.strip(), last.strip()
        try:
            if first == "" and last == "":
                return ("bad",)
            if first == "":  # bytes=-N : the last N bytes
                n = int(last)
                if n <= 0 or SIZE == 0:
                    return ("unsat",)
                return ("range", max(0, SIZE - n), SIZE - 1)
            start = int(first)
            if last == "":  # bytes=N- : from N to EOF
                if start >= SIZE:
                    return ("unsat",)
                return ("range", start, SIZE - 1)
            end = int(last)
            if start > end:
                return ("bad",)
            if start >= SIZE:
                return ("unsat",)
            return ("range", start, min(end, SIZE - 1))
        except ValueError:
            return ("bad",)

    # ---- body -------------------------------------------------------------
    def _serve_get(self):
        if MODE == "nolength":
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                sent = 0
                while sent < SIZE:
                    piece = DATA[sent:sent + CHUNK]
                    if not piece:
                        break
                    self.wfile.write(piece)
                    sent += len(piece)
            except OSError:
                pass
            return
        if MODE == "norange":
            # Range is IGNORED: always the full body with 200.
            self._send_slice(200, 0, SIZE - 1, [("Accept-Ranges", "none")])
            return
        parsed = self._parse_range(self.headers.get("Range"))
        kind = parsed[0]
        if kind in ("full", "bad"):
            # No usable Range header (a flaky full-body request still counts
            # as the range (0, SIZE-1) for the drop memory).
            self._send_slice(200, 0, SIZE - 1, [("Accept-Ranges", "bytes")])
        elif kind == "unsat":
            self.send_response(416)
            self._common_headers(0)
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Range", "bytes */%d" % SIZE)
            self.end_headers()
        else:
            start, end = parsed[1], parsed[2]
            self._send_slice(206, start, end, [
                ("Accept-Ranges", "bytes"),
                ("Content-Range", "bytes %d-%d/%d" % (start, end, SIZE)),
            ])

    def _send_slice(self, status, start, end, extra):
        global LAST_DROPPED
        length = max(0, end - start + 1)
        # oversend: declare the honest range, then ship extra bytes past its end
        body_len = length + OVERSEND_BYTES if (MODE == "oversend" and status == 206) else length
        self.send_response(status)
        self._common_headers(body_len)
        for key, value in extra:
            self.send_header(key, value)
        self.end_headers()
        if self.command == "HEAD" or body_len == 0:
            return  # defensive: never a body for HEAD
        if MODE == "stall":
            with STATE_LOCK:
                # Only body responses longer than 1 byte consume the budget, so a
                # "bytes=0-0" probe is never the thing that stalls.
                global STALL_BUDGET
                stall_now = length > 1 and STALL_BUDGET > 0 and (start, end) not in STALLED
                if stall_now:
                    STALL_BUDGET -= 1
                    STALLED.add((start, end))
            if stall_now:
                # Headers are on the wire and Content-Length promises more: go
                # silent without closing, the worst case for a reader that has
                # no idle timeout.
                try:
                    if STALL_PREFIX:
                        self.wfile.write(DATA[start:start + min(STALL_PREFIX, length)])
                        self.wfile.flush()
                    time.sleep(STALL_SECS)
                except OSError:
                    pass
                return
        if MODE == "flaky":
            prefix = None
            with STATE_LOCK:
                droppable = length >= 2 and (start, end) != LAST_DROPPED
                if droppable and RNG.random() < DROP_P:
                    # Mid-body drop: headers are already on the wire; send a
                    # random 0-256 KiB prefix, then cut the connection. The
                    # range is remembered so an immediate re-request of this
                    # exact range is served in full.
                    prefix = RNG.randint(0, min(MAX_PREFIX, length - 1))
                    LAST_DROPPED = (start, end)
            if prefix is not None:
                try:
                    if prefix:
                        self.wfile.write(DATA[start:start + prefix])
                    self.wfile.flush()
                except OSError:
                    pass
                return  # connection dies here: short body vs Content-Length
        sent = 0
        slice_start = time.time()
        try:
            while sent < body_len:
                take = min(CHUNK, body_len - sent)
                if sent + take <= length:
                    piece = DATA[start + sent:start + sent + take]
                elif sent < length:
                    # final honest piece, then straight into the excess
                    piece = DATA[start + sent:start + length]
                    need = take - len(piece)
                    piece = piece + DATA[end + 1:end + 1 + need]
                else:
                    # Past the declared range end: the excess the client must
                    # ignore. On the FINAL chunk there is nothing past EOF, so the
                    # filler wraps around from the start - otherwise this mode
                    # could never exercise the last-chunk cap at all.
                    off = end + 1 + (sent - length)
                    piece = DATA[off:off + take]
                    if not piece and SIZE:
                        wrapped = (off - (end + 1)) % SIZE
                        piece = DATA[wrapped:wrapped + take]
                if not piece:
                    break
                self.wfile.write(piece)
                sent += len(piece)
                if RATE_BYTES > 0:
                    # pace this connection so a test can catch it mid-transfer
                    ahead = sent / RATE_BYTES - (time.time() - slice_start)
                    if ahead > 0:
                        time.sleep(min(ahead, 0.5))
        except OSError:
            pass  # client went away; nothing to do


def main(argv=None):
    global MODE, DATA, SIZE, DROP_P, STALL_SECS, RATE_BYTES, STALL_PREFIX, STALL_BUDGET
    parser = argparse.ArgumentParser(
        description="Hydra acceptance-test file server (see scripts/README-TESTS.md)")
    parser.add_argument("--mode",
                        choices=("range", "norange", "flaky", "oversend", "stall",
                                 "noadvertise", "nolength"),
                        default="range", help="server behaviour (default: range)")
    parser.add_argument("--port", type=int, default=8899,
                        help="listen port (default: 8899)")
    parser.add_argument("--file", required=True,
                        help="file to serve at '/' regardless of its name")
    parser.add_argument("--drop-p", dest="drop_p", type=float, default=0.15,
                        help="flaky mode: drop probability per droppable request (default: 0.15)")
    parser.add_argument("--bind", default="127.0.0.1",
                        help="bind address (default: 127.0.0.1)")
    parser.add_argument("--stall-secs", dest="stall_secs", type=float, default=45.0,
                        help="stall mode: seconds of silence before closing (default: 45)")
    parser.add_argument("--rate-kib", dest="rate_kib", type=float, default=0.0,
                        help="throttle each connection to this many KiB/s (default: unlimited)")
    parser.add_argument("--stall-prefix", dest="stall_prefix", type=int,
                        default=STALL_PREFIX,
                        help="stall mode: bytes sent before going silent (default: 4096)")
    parser.add_argument("--stall-requests", dest="stall_requests", type=int, default=STALL_BUDGET,
                        help="stall mode: how many responses may stall in total (default: 4)")
    args = parser.parse_args(argv)
    if not 0.0 <= args.drop_p <= 1.0:
        parser.error("--drop-p must be within [0.0, 1.0]")
    try:
        f = open(args.file, "rb")
    except OSError as exc:
        raise SystemExit("cannot read --file: %s" % exc)
    with f:
        try:
            DATA = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
        except (ValueError, OSError):  # empty (or unmappable) file
            f.seek(0)
            DATA = f.read()
    MODE, SIZE, DROP_P = args.mode, len(DATA), args.drop_p
    STALL_SECS = args.stall_secs
    STALL_PREFIX = max(0, args.stall_prefix)
    STALL_BUDGET = max(0, args.stall_requests)
    RATE_BYTES = int(args.rate_kib * 1024) if args.rate_kib > 0 else 0
    server = QuietServer((args.bind, args.port), QuietHandler)
    print("testserver mode=%s bind=%s port=%d file=%s size=%d drop_p=%s"
          % (MODE, args.bind, args.port, args.file, SIZE, DROP_P), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
