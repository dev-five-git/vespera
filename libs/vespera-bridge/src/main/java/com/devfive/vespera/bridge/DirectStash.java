package com.devfive.vespera.bridge;

import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.function.Consumer;

/**
 * DIRECT overflow stash — natives that keep a response which overflowed the
 * pooled direct out-buffer on the Rust side so it can be fetched or streamed
 * <strong>without re-running the handler</strong>.
 *
 * <p>{@link #dispatchResumable0} follows the {@code dispatchDirect} {@code int}
 * protocol ({@code >= 0} complete, {@code Integer.MIN_VALUE} unrepresentable)
 * except on overflow: it returns {@code -(required)} (exact only when
 * {@link #FLAG_EXACT} is set, otherwise a lower bound) and writes a
 * {@link #HANDLE_LEN}-byte handle into {@code out[0..9]} — an 8-byte
 * big-endian token followed by a flags byte.  The token is redeemed exactly
 * once by {@link #fetch0} (copy into a bigger buffer; an overflow keeps it
 * stashed under the same token), {@link #stream0} (header-first streaming,
 * same contract as {@link VesperaBridge#dispatchStreamingWithHeader}) or
 * released by {@link #discard0}.  An unredeemed entry expires after 30 s.
 */
final class DirectStash {
    /** Bytes of the overflow handle written into {@code out}. */
    static final int HANDLE_LEN = 9;
    /** Handle flag: the reported required size is exact. */
    static final int FLAG_EXACT = 1;
    /** Handle flag: the response is an event stream — deliver it with {@link #stream0}. */
    static final int FLAG_STREAM = 2;

    private DirectStash() {}

    /** Resumable {@code dispatchDirect}: buffers validated by the caller. */
    static native int dispatchResumable0(ByteBuffer in, int inLen, ByteBuffer out);

    /** Continue stashed response {@code token} into {@code out} (see class docs). */
    static native int fetch0(long token, ByteBuffer out);

    /** Stream stashed response {@code token}: header consumer first, then the body. */
    static native void stream0(long token, Consumer<byte[]> headerConsumer, OutputStream out);

    /** Drop stashed response {@code token} (no-op when unknown). */
    static native void discard0(long token);

    /** Token from an overflow handle in {@code out} ({@code 0} = none). */
    static long token(ByteBuffer out) {
        return out.capacity() >= HANDLE_LEN ? out.getLong(0) : 0L;
    }

    /** Flags byte from an overflow handle in {@code out}. */
    static int flags(ByteBuffer out) {
        return out.capacity() >= HANDLE_LEN ? out.get(8) & 0xFF : 0;
    }

    /** {@link VesperaDirectBufferPool.DirectDispatcher} over the resumable symbol. */
    static int dispatchResumable(ByteBuffer in, int inLen, ByteBuffer out) {
        BridgeInputChecks.requireDirectBuffers(in, inLen, out);
        return dispatchResumable0(in, inLen, out);
    }

    /** Operations on a stashed response; replaceable in tests. */
    interface Ops {
        int fetch(long token, ByteBuffer out);

        void stream(long token, Consumer<byte[]> headerConsumer, OutputStream out);

        void discard(long token);
    }

    /** The native-backed {@link Ops}. */
    static final Ops NATIVE = new Ops() {
        @Override
        public int fetch(long token, ByteBuffer out) {
            return fetch0(token, out);
        }

        @Override
        public void stream(long token, Consumer<byte[]> headerConsumer, OutputStream out) {
            stream0(token, headerConsumer, out);
        }

        @Override
        public void discard(long token) {
            discard0(token);
        }
    };
}
