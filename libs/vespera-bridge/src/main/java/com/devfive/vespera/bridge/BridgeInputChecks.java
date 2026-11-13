package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.util.Map;
import java.util.Objects;

/**
 * Caller-side argument checks behind the public {@link VesperaBridge}
 * encode / dispatch / configure entry points.
 *
 * <p>Every check runs on the Java side <em>before</em> anything is encoded
 * or crosses JNI, so a bad argument surfaces as a plain
 * {@link NullPointerException} / {@link IllegalArgumentException} instead
 * of a malformed wire request or a native fault.
 */
final class BridgeInputChecks {

    private BridgeInputChecks() {}

    /** {@link #requireRequest(String, String)} plus non-null header keys and values. */
    static void requireRequest(String method, String path, Map<String, String> headers) {
        requireRequest(method, path);
        if (headers != null) {
            for (Map.Entry<String, String> header : headers.entrySet()) {
                Objects.requireNonNull(header.getKey(), "header key");
                Objects.requireNonNull(header.getValue(), "header value");
            }
        }
    }

    /** Non-null method and path; the path must not carry a query string. */
    static void requireRequest(String method, String path) {
        Objects.requireNonNull(method, "method");
        Objects.requireNonNull(path, "path");
        if (path.indexOf('?') >= 0) {
            throw new IllegalArgumentException(
                    "path must not contain '?' — pass the raw query string via the query parameter");
        }
    }

    /** Range checks of {@link VesperaBridge#configureStreaming(int, int)}. */
    static void requireStreamingConfig(int chunkBytes, int channelCapacity) {
        if (chunkBytes < 4096 || chunkBytes > 8388608) {
            throw new IllegalArgumentException(
                    "chunkBytes " + chunkBytes
                            + " out of range [4096, 8388608] (4 KiB – 8 MiB)");
        }
        if (channelCapacity < 1 || channelCapacity > 1024) {
            throw new IllegalArgumentException(
                    "channelCapacity " + channelCapacity + " out of range [1, 1024]");
        }
    }

    /** Caller-side checks shared by the direct-buffer natives. */
    static void requireDirectBuffers(ByteBuffer in, int inLen, ByteBuffer out) {
        Objects.requireNonNull(in, "in");
        Objects.requireNonNull(out, "out");
        if (!in.isDirect() || !out.isDirect()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires direct ByteBuffers (use ByteBuffer.allocateDirect)");
        }
        if (in.isReadOnly()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires a writable in ByteBuffer (got a read-only buffer)");
        }
        // SEC-2: the native side writes the wire response straight into
        // `out` via a `&mut [u8]`; a read-only direct buffer (e.g. a
        // read-only MappedByteBuffer) is backed by read-only pages, so
        // writing to it is undefined behavior / a process crash.  Reject
        // it here — the native code cannot recover from a write fault.
        if (out.isReadOnly()) {
            throw new IllegalArgumentException(
                    "dispatchDirect requires a writable out ByteBuffer (got a read-only buffer)");
        }
        if (inLen < 0 || inLen > in.capacity()) {
            throw new IllegalArgumentException(
                    "inLen " + inLen + " out of range for in.capacity() " + in.capacity());
        }
    }
}
