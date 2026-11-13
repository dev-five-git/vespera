"""Drive the rust-jni-demo cdylib through vespera's C ABI from Python (ctypes).

The same library the Spring demo loads over JNI also exports the stable C
ABI (`vespera::ffi_apps!` in src/lib.rs, header
crates/vespera_inprocess/include/vespera.h).  No HTTP, no JVM:

    cargo build -p rust-jni-demo --release
    python examples/rust-jni-demo/ffi/ctypes_demo.py [path/to/library]

Not run in CI (the Rust test `c_abi_dispatches_both_apps` covers the ABI).
"""

import ctypes
import json
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
NAMES = {"win32": "rust_jni_demo.dll", "darwin": "librust_jni_demo.dylib"}
DEFAULT_LIB = ROOT / "target" / "release" / NAMES.get(sys.platform, "librust_jni_demo.so")

lib = ctypes.CDLL(str(sys.argv[1] if len(sys.argv) > 1 else DEFAULT_LIB))
u8p = ctypes.POINTER(ctypes.c_uint8)
lib.vespera_init.restype = ctypes.c_int
lib.vespera_dispatch.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(u8p), ctypes.POINTER(ctypes.c_size_t)]
lib.vespera_dispatch.restype = ctypes.c_int
lib.vespera_free.argtypes = [u8p, ctypes.c_size_t]
lib.vespera_free.restype = None
lib.vespera_shutdown.argtypes = [ctypes.c_uint64]
lib.vespera_shutdown.restype = ctypes.c_int


def dispatch(method, path, body=b"", app=None, headers=None):
    """Encode a wire request, dispatch it, decode the wire response."""
    head = {"v": 1, "method": method, "path": path}
    if app:
        head["app"] = app
    if headers:
        head["headers"] = headers
    head_bytes = json.dumps(head).encode()
    request = struct.pack(">I", len(head_bytes)) + head_bytes + body

    out, out_len = u8p(), ctypes.c_size_t()
    rc = lib.vespera_dispatch(request, len(request), ctypes.byref(out), ctypes.byref(out_len))
    if rc != 0:
        raise RuntimeError(f"vespera_dispatch failed: {rc}")
    try:
        wire = ctypes.string_at(out, out_len.value)
    finally:
        lib.vespera_free(out, out_len.value)  # the response is Rust-owned

    (n,) = struct.unpack(">I", wire[:4])
    return json.loads(wire[4 : 4 + n]), wire[4 + n :]


if lib.vespera_init() != 0:
    raise SystemExit("vespera_init failed")

header, body = dispatch("GET", "/health")
print(header["status"], body.decode())

header, body = dispatch("GET", "/dashboard", app="admin")
print(header["status"], body.decode())

doc = {"title": "", "pageCount": 0}
header, body = dispatch(
    "POST", "/documents/register", json.dumps(doc).encode(), headers={"content-type": "application/json"}
)
print(header["status"], header.get("validation_errors"))  # 422 errors hoisted into the wire header

lib.vespera_shutdown(5000)
