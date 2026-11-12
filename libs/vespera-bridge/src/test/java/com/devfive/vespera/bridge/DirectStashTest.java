package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Consumer;
import org.junit.jupiter.api.Test;

/** DIRECT overflow stash: fetch without re-running the handler. */
class DirectStashTest {

    private static final long TOKEN = 0x0102030405060708L;

    private static ByteBuffer[] freshPool() {
        return new ByteBuffer[] {ByteBuffer.allocateDirect(64), ByteBuffer.allocateDirect(64 * 1024)};
    }

    /** Dispatcher that overflows once and leaves a stash handle in {@code out}. */
    private static VesperaDirectBufferPool.DirectDispatcher overflowing(
            AtomicInteger calls, int required, int flags) {
        return (in, inLen, out) -> {
            calls.incrementAndGet();
            out.putLong(0, TOKEN);
            out.put(8, (byte) flags);
            return -required;
        };
    }

    /** Fake stash: answers the queued fetch codes and records every call. */
    private static final class FakeOps implements DirectStash.Ops {
        final List<Integer> fetchCodes = new ArrayList<>();
        final List<Integer> fetchCapacities = new ArrayList<>();
        final List<String> calls = new ArrayList<>();

        @Override
        public int fetch(long token, ByteBuffer out) {
            assertEquals(TOKEN, token);
            calls.add("fetch");
            fetchCapacities.add(out.capacity());
            int code = fetchCodes.remove(0);
            if (code > 0) {
                out.put(0, (byte) 42);
            }
            return code;
        }

        @Override
        public void stream(long token, Consumer<byte[]> headerConsumer, OutputStream out) {
            assertEquals(TOKEN, token);
            calls.add("stream");
            headerConsumer.accept(new byte[] {1});
        }

        @Override
        public void discard(long token) {
            assertEquals(TOKEN, token);
            calls.add("discard");
        }
    }

    @Test
    void overflowIsFetchedWithoutRedispatchEvenWhenRetryIsDisallowed() {
        AtomicInteger calls = new AtomicInteger();
        FakeOps ops = new FakeOps();
        // First fetch into 128 KiB is still short (inexact size), the second fits.
        ops.fetchCodes.add(-(200 * 1024));
        ops.fetchCodes.add(3);
        ByteBuffer[] pool = freshPool();
        ByteBuffer view = VesperaDirectBufferPool.dispatchViaPool(
                pool, 1, false, overflowing(calls, 70_000, 0), ops, false);
        assertEquals(1, calls.get(), "the handler ran exactly once");
        assertEquals(List.of("fetch", "fetch"), ops.calls);
        assertEquals(List.of(128 * 1024, 256 * 1024), ops.fetchCapacities);
        assertEquals(3, view.remaining());
        assertEquals(42, view.get(0));
        assertTrue(view.isReadOnly());
    }

    @Test
    void eventStreamIsKeptForStreamingOrDiscarded() {
        FakeOps ops = new FakeOps();
        StashedOverflowException kept = assertThrows(
                StashedOverflowException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, true,
                        overflowing(new AtomicInteger(), 100, DirectStash.FLAG_STREAM), ops, true));
        assertTrue(kept.eventStream());
        assertEquals(100, kept.requiredSize());
        assertTrue(ops.calls.isEmpty(), "nothing fetched for an event stream");
        List<byte[]> headers = new ArrayList<>();
        kept.streamTo(headers::add, new ByteArrayOutputStream());
        assertEquals(1, headers.size());
        kept.discard();
        assertEquals(List.of("stream", "discard"), ops.calls);

        FakeOps discarding = new FakeOps();
        VesperaBridge.BufferTooSmallException plain = assertThrows(
                VesperaBridge.BufferTooSmallException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, true,
                        overflowing(new AtomicInteger(), 100, DirectStash.FLAG_STREAM),
                        discarding, false));
        assertFalse(plain instanceof StashedOverflowException);
        assertEquals(List.of("discard"), discarding.calls);
    }

    @Test
    void responseBeyondThePoolCapIsKeptAfterGrowingToTheCap() {
        FakeOps ops = new FakeOps();
        int cap = 4 * 1024 * 1024;
        ops.fetchCodes.add(-(cap - 1));
        StashedOverflowException kept = assertThrows(
                StashedOverflowException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, false,
                        overflowing(new AtomicInteger(), cap - 10, DirectStash.FLAG_EXACT), ops, true));
        assertFalse(kept.eventStream());
        assertEquals(cap + 1, kept.requiredSize());
        assertEquals(List.of(cap), ops.fetchCapacities);

        FakeOps tooBig = new FakeOps();
        assertThrows(StashedOverflowException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, false,
                        overflowing(new AtomicInteger(), cap + 1, DirectStash.FLAG_EXACT), tooBig, true));
        assertTrue(tooBig.calls.isEmpty());
    }

    @Test
    void expiredStashEntryFailsLoudly() {
        FakeOps ops = new FakeOps();
        ops.fetchCodes.add(Integer.MIN_VALUE);
        IllegalStateException expired = assertThrows(
                IllegalStateException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, false,
                        overflowing(new AtomicInteger(), 70_000, DirectStash.FLAG_EXACT), ops, false));
        assertTrue(expired.getMessage().contains("expired"));
    }

    @Test
    void withoutAHandleTheLegacyRedispatchPolicyApplies() {
        AtomicInteger calls = new AtomicInteger();
        FakeOps ops = new FakeOps();
        ByteBuffer view = VesperaDirectBufferPool.dispatchViaPool(
                freshPool(), 1, true, (in, inLen, out) -> {
                    // token 0 = no handle
                    out.putLong(0, 0L);
                    return calls.getAndIncrement() == 0 ? -70_000 : 1;
                }, ops, true);
        assertEquals(2, calls.get());
        assertEquals(1, view.remaining());
        assertTrue(ops.calls.isEmpty());
    }

    @Test
    void handleAccessorsNeedNineBytes() {
        ByteBuffer small = ByteBuffer.allocateDirect(8);
        assertEquals(0L, DirectStash.token(small));
        assertEquals(0, DirectStash.flags(small));
        ByteBuffer handle = ByteBuffer.allocateDirect(DirectStash.HANDLE_LEN);
        handle.putLong(0, TOKEN).put(8, (byte) 0xFF);
        assertEquals(TOKEN, DirectStash.token(handle));
        assertEquals(0xFF, DirectStash.flags(handle));
        assertSame(DirectStash.NATIVE, DirectStash.NATIVE);
        assertThrows(IllegalArgumentException.class,
                () -> DirectStash.dispatchResumable(ByteBuffer.allocate(1), 1, handle));
        assertThrows(UnsatisfiedLinkError.class,
                () -> DirectStash.NATIVE.fetch(TOKEN, handle));
        assertThrows(UnsatisfiedLinkError.class,
                () -> DirectStash.NATIVE.stream(TOKEN, h -> { }, new ByteArrayOutputStream()));
        assertThrows(UnsatisfiedLinkError.class, () -> DirectStash.NATIVE.discard(TOKEN));
    }
}
