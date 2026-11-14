package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.ByteBuffer;
import java.util.Map;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

/**
 * {@code DirectStash.fetch0} against the real {@code rust_jni_demo} cdylib
 * when the stashed response's body panics while it is being fetched: the
 * caller gets a complete {@code 500} panic wire in its buffer, not the
 * "entry expired" sentinel (which would send the pooled path down its
 * stale-handle fallback and hide the panic).
 */
class DirectStashPanicE2ETest {

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    @Test
    void panicWhileFetchingAStashedResponseAnswersAPanicWire() {
        // 512 KiB of body, then the stream panics: far more than the 64-byte
        // buffer holds, so the response is stashed before the panic point.
        byte[] request = VesperaBridge.encodeRequest(
                "POST", "/diag/panic-mid-body/512", null, Map.of(), new byte[0]);
        ByteBuffer in = ByteBuffer.allocateDirect(request.length);
        in.put(request).flip();
        ByteBuffer small = ByteBuffer.allocateDirect(64);
        int n = DirectStash.dispatchResumable(in, request.length, small);
        assertTrue(n < 0, "the response overflows the 64-byte buffer: " + n);
        long token = DirectStash.token(small);
        assertNotEquals(0L, token, "an overflow leaves a stash handle");

        ByteBuffer big = ByteBuffer.allocateDirect(4 * 1024 * 1024);
        int m = DirectStash.fetch0(token, big);
        assertTrue(m >= 0, "a panic during the fetch is a complete wire, not 'expired': " + m);
        byte[] wire = new byte[m];
        big.get(0, wire);
        VesperaBridge.DecodedResponse r = VesperaBridge.decodeResponse(wire);
        assertEquals(500, r.status());
        assertEquals("true", r.metadata().get("panicked"), r.metadata().toString());
    }
}
