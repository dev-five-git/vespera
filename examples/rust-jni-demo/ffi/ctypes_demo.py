"""Drive the rust-jni-demo cdylib through vespera's C ABI from Python (ctypes).

The same library the Spring demo loads over JNI also exports the stable C
ABI (`vespera::ffi_apps!` in src/lib.rs, header
crates/vespera_inprocess/include/vespera.h).  No HTTP, no JVM:

    cargo build -p rust-jni-demo --release
    python3 examples/rust-jni-demo/ffi/ctypes_demo.py [path/to/library]

Every response is checked: the script exits non-zero on any mismatch.  CI
runs it against the release cdylib (jni-e2e, ubuntu-latest / JDK 17 leg).
"""

import ctypes
import json
import struct
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
NAMES = {"win32": "rust_jni_demo.dll", "darwin": "librust_jni_demo.dylib"}
DEFAULT_LIB = ROOT / "target" / "release" / NAMES.get(sys.platform, "librust_jni_demo.so")

VESPERA_OK = 0
VESPERA_ERR_TOO_LATE = -5

lib = ctypes.CDLL(str(sys.argv[1] if len(sys.argv) > 1 else DEFAULT_LIB))
u8p = ctypes.POINTER(ctypes.c_uint8)
CALLBACK = ctypes.CFUNCTYPE(None, ctypes.c_void_p, u8p, ctypes.c_size_t)

lib.vespera_abi_version.restype = ctypes.c_uint32
lib.vespera_init.restype = ctypes.c_int
lib.vespera_dispatch.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(u8p), ctypes.POINTER(ctypes.c_size_t)]
lib.vespera_dispatch.restype = ctypes.c_int
lib.vespera_dispatch_async.argtypes = [ctypes.c_char_p, ctypes.c_size_t, CALLBACK, ctypes.c_void_p]
lib.vespera_dispatch_async.restype = ctypes.c_int
lib.vespera_free.argtypes = [u8p, ctypes.c_size_t]
lib.vespera_free.restype = None
lib.vespera_shutdown.argtypes = [ctypes.c_uint64]
lib.vespera_shutdown.restype = ctypes.c_int
lib.vespera_configure_max_request_bytes.argtypes = [ctypes.c_size_t]
lib.vespera_configure_max_request_bytes.restype = ctypes.c_int
lib.vespera_configure_async_max_in_flight.argtypes = [ctypes.c_size_t]
lib.vespera_configure_async_max_in_flight.restype = ctypes.c_int


def check(condition, message):
    if not condition:
        raise SystemExit(f"FAIL: {message}")
    print(f"ok   {message}")


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


check(lib.vespera_abi_version() == 1, "ABI version 1")
# Configure before vespera_init (first call wins, the second is too late).
check(lib.vespera_configure_max_request_bytes(1 << 20) == VESPERA_OK, "configure max request bytes")
check(lib.vespera_configure_max_request_bytes(1 << 21) == VESPERA_ERR_TOO_LATE, "second configure is too late")
check(lib.vespera_configure_async_max_in_flight(64) == VESPERA_OK, "configure async in-flight cap")
check(lib.vespera_init() == VESPERA_OK, "vespera_init")

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

check(lib.vespera_shutdown(5000) == VESPERA_OK, "vespera_shutdown")
header, _ = dispatch("GET", "/health")
check(header["status"] == 503, f"dispatch after shutdown -> 503 ({header['status']})")
print("all C ABI checks passed")
