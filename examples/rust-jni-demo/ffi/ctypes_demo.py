"""Drive the rust-jni-demo cdylib through vespera's C ABI from Python (ctypes).

The same library the Spring demo loads over JNI also exports the stable C
ABI (`vespera::ffi_apps!` in src/lib.rs, header
crates/vespera_inprocess/include/vespera.h).  No HTTP, no JVM:

    cargo build -p rust-jni-demo --release
    python3 examples/rust-jni-demo/ffi/ctypes_demo.py [path/to/library]

Every response is checked: the script exits non-zero on any mismatch.  CI
runs it against the release cdylib on every host OS (jni-e2e, JDK 17 legs).

Every prototype in vespera.h is bound below and must resolve in the
library (a symbol the header declares but the cdylib does not export fails
here), and every function is called at least once.
"""

import ctypes
import json
import re
import struct
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
HEADER = ROOT / "crates" / "vespera_inprocess" / "include" / "vespera.h"
NAMES = {"win32": "rust_jni_demo.dll", "darwin": "librust_jni_demo.dylib"}
DEFAULT_LIB = ROOT / "target" / "release" / NAMES.get(sys.platform, "librust_jni_demo.so")

VESPERA_OK = 0
VESPERA_OVERFLOW = 1
VESPERA_ERR_INVALID_ARGUMENT = -1
VESPERA_ERR_RUNTIME = -4
VESPERA_ERR_TOO_LATE = -5

lib = ctypes.CDLL(str(sys.argv[1] if len(sys.argv) > 1 else DEFAULT_LIB))
u8p = ctypes.POINTER(ctypes.c_uint8)
size_tp = ctypes.POINTER(ctypes.c_size_t)
CALLBACK = ctypes.CFUNCTYPE(None, ctypes.c_void_p, u8p, ctypes.c_size_t)

# name -> (restype, argtypes); must cover exactly the header's prototypes.
SIGNATURES = {
    "vespera_abi_version": (ctypes.c_uint32, []),
    "vespera_init": (ctypes.c_int, []),
    "vespera_dispatch": (ctypes.c_int, [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(u8p), size_tp]),
    "vespera_dispatch_into": (
        ctypes.c_int,
        [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, size_tp],
    ),
    "vespera_dispatch_async": (ctypes.c_int, [ctypes.c_char_p, ctypes.c_size_t, CALLBACK, ctypes.c_void_p]),
    "vespera_free": (None, [u8p, ctypes.c_size_t]),
    "vespera_shutdown": (ctypes.c_int, [ctypes.c_uint64]),
    "vespera_configure_worker_threads": (ctypes.c_int, [ctypes.c_size_t]),
    "vespera_configure_max_blocking_threads": (ctypes.c_int, [ctypes.c_size_t]),
    "vespera_configure_max_request_bytes": (ctypes.c_int, [ctypes.c_size_t]),
    "vespera_configure_max_buffered_response_bytes": (ctypes.c_int, [ctypes.c_size_t]),
    "vespera_configure_dispatch_timeout_ms": (ctypes.c_int, [ctypes.c_uint64]),
    "vespera_configure_async_max_in_flight": (ctypes.c_int, [ctypes.c_size_t]),
}


def check(condition, message):
    if not condition:
        raise SystemExit(f"FAIL: {message}")
    print(f"ok   {message}")


# A prototype: `<return type> vespera_<name>(` at the start of a line.
declared = set(re.findall(r"^[A-Za-z_][\w \t\*]*?\b(vespera_\w+)\s*\(", HEADER.read_text(encoding="utf-8"), re.MULTILINE))
check(declared == set(SIGNATURES), f"bindings cover every vespera.h prototype ({sorted(declared ^ set(SIGNATURES))})")
for name, (restype, argtypes) in SIGNATURES.items():
    try:
        function = getattr(lib, name)
    except AttributeError:
        raise SystemExit(f"FAIL: {name} (declared in vespera.h) is not exported by the library")
    function.restype, function.argtypes = restype, argtypes
check(True, f"all {len(SIGNATURES)} vespera.h symbols resolve")


def encode(method, path, body=b"", app=None, headers=None):
    head = {"v": 1, "method": method, "path": path}
    if app:
        head["app"] = app
    if headers:
        head["headers"] = headers
    head_bytes = json.dumps(head).encode()
    return struct.pack(">I", len(head_bytes)) + head_bytes + body


def decode(wire):
    (n,) = struct.unpack(">I", wire[:4])
    return json.loads(wire[4 : 4 + n]), wire[4 + n :]


def dispatch(*args, **kwargs):
    """Encode a wire request, dispatch it synchronously, decode the response."""
    request = encode(*args, **kwargs)
    out, out_len = u8p(), ctypes.c_size_t()
    rc = lib.vespera_dispatch(request, len(request), ctypes.byref(out), ctypes.byref(out_len))
    check(rc == VESPERA_OK, f"vespera_dispatch returns VESPERA_OK (got {rc})")
    try:
        wire = ctypes.string_at(out, out_len.value)
    finally:
        lib.vespera_free(out, out_len.value)  # the response is Rust-owned
    return decode(wire)


def dispatch_async(*args, **kwargs):
    """Same, through vespera_dispatch_async (the callback runs on a runtime worker)."""
    request = encode(*args, **kwargs)
    done, result = threading.Event(), []

    def on_response(_user_data, response, response_len):
        result.append(ctypes.string_at(response, response_len))
        lib.vespera_free(response, response_len)  # the callback owns the response
        done.set()

    callback = CALLBACK(on_response)  # keep alive until the callback ran
    rc = lib.vespera_dispatch_async(request, len(request), callback, None)
    check(rc == VESPERA_OK, f"vespera_dispatch_async returns VESPERA_OK (got {rc})")
    check(done.wait(10), "async callback ran")
    return decode(result[0])


def dispatch_into(request, capacity):
    """vespera_dispatch_into with a `capacity`-byte buffer: (rc, written, bytes)."""
    out = (ctypes.c_uint8 * max(capacity, 1))()
    written = ctypes.c_size_t()
    rc = lib.vespera_dispatch_into(request, len(request), out if capacity else None, capacity, ctypes.byref(written))
    return rc, written.value, bytes(out[: min(written.value, capacity)])


check(lib.vespera_abi_version() == 1, "ABI version 1")
# Configure before vespera_init (first call wins, the second is too late).
check(lib.vespera_configure_worker_threads(2) == VESPERA_OK, "configure worker threads")
check(lib.vespera_configure_max_blocking_threads(64) == VESPERA_OK, "configure max blocking threads")
check(lib.vespera_configure_max_request_bytes(1 << 20) == VESPERA_OK, "configure max request bytes")
check(lib.vespera_configure_max_request_bytes(1 << 21) == VESPERA_ERR_TOO_LATE, "second configure is too late")
check(lib.vespera_configure_max_buffered_response_bytes(64 << 20) == VESPERA_OK, "configure buffered response cap")
check(lib.vespera_configure_dispatch_timeout_ms(30_000) == VESPERA_OK, "configure dispatch timeout")
check(lib.vespera_configure_async_max_in_flight(64) == VESPERA_OK, "configure async in-flight cap")
check(lib.vespera_init() == VESPERA_OK, "vespera_init")
check(lib.vespera_init() == VESPERA_OK, "vespera_init is idempotent")
check(lib.vespera_configure_worker_threads(4) == VESPERA_ERR_TOO_LATE, "runtime knobs are fixed by vespera_init")

header, body = dispatch("GET", "/health")
check((header["status"], body) == (200, b"ok"), f"GET /health -> 200 ok ({header['status']} {body!r})")

header, body = dispatch("GET", "/dashboard", app="admin")
check(header["status"] == 200 and json.loads(body)["mode"] == "admin", f"admin app dashboard ({header['status']} {body!r})")

doc = {"title": "", "pageCount": 0}
header, body = dispatch(
    "POST", "/documents/register", json.dumps(doc).encode(), headers={"content-type": "application/json"}
)
check(
    header["status"] == 422 and len(header.get("validation_errors") or []) > 0,
    f"422 errors hoisted into the wire header ({header['status']} {header.get('validation_errors')})",
)

header, body = dispatch("POST", "/documents/register", b"x" * (2 << 20), headers={"content-type": "application/json"})
check(header["status"] == 413, f"configured request cap -> 413 ({header['status']})")

header, body = dispatch_async("GET", "/health")
check((header["status"], body) == (200, b"ok"), f"async GET /health -> 200 ok ({header['status']} {body!r})")

# NULL request with a non-zero length: a 400 wire response, never a crash.
out, out_len = u8p(), ctypes.c_size_t()
check(lib.vespera_dispatch(None, 5, ctypes.byref(out), ctypes.byref(out_len)) == VESPERA_OK, "NULL request -> OK")
header, _ = decode(ctypes.string_at(out, out_len.value))
lib.vespera_free(out, out_len.value)
check(header["status"] == 400, f"NULL request -> 400 ({header['status']})")
check(lib.vespera_dispatch(None, 0, None, None) == VESPERA_ERR_INVALID_ARGUMENT, "NULL response out-params rejected")
lib.vespera_free(None, 0)  # NULL is a no-op

# Early rejections reach the async callback inline, before the call returns.
inline = []
on_inline = CALLBACK(lambda _u, response, n: (inline.append(decode(ctypes.string_at(response, n))[0]), lib.vespera_free(response, n)))
check(lib.vespera_dispatch_async(None, 5, on_inline, None) == VESPERA_OK, "async NULL request -> OK")
check(len(inline) == 1 and inline[0]["status"] == 400, f"async 400 delivered inline before returning ({inline})")

# vespera_dispatch_into: size probe, overflow, then an exact fit.
request = encode("GET", "/health")
rc, required, _ = dispatch_into(request, 0)
check(rc == VESPERA_OVERFLOW and required > 0, f"dispatch_into size probe -> OVERFLOW, {required} bytes")
rc, needed, _ = dispatch_into(request, required - 1)
check((rc, needed) == (VESPERA_OVERFLOW, required), f"dispatch_into one byte short -> OVERFLOW ({rc}, {needed})")
rc, written, wire = dispatch_into(request, required)
header, body = decode(wire)
check(
    (rc, written, header["status"], body) == (VESPERA_OK, required, 200, b"ok"),
    f"dispatch_into exact buffer -> 200 ok ({rc} {written} {header['status']} {body!r})",
)
check(
    lib.vespera_dispatch_into(request, len(request), None, 16, ctypes.byref(ctypes.c_size_t()))
    == VESPERA_ERR_INVALID_ARGUMENT,
    "dispatch_into NULL out with capacity rejected",
)

check(lib.vespera_shutdown(5000) == VESPERA_OK, "vespera_shutdown")
header, _ = dispatch("GET", "/health")
check(header["status"] == 503, f"dispatch after shutdown -> 503 ({header['status']})")
check(lib.vespera_shutdown(0) == VESPERA_ERR_RUNTIME, "second vespera_shutdown -> VESPERA_ERR_RUNTIME")
print("all C ABI checks passed")
