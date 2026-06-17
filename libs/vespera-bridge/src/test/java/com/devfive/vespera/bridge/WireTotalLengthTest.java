package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

/**
 * {@link VesperaWireCodec#wireTotalLength} int-overflow guard: a body near
 * the 2 GiB Java array limit must fail loud rather than wrap the {@code
 * 4 + headerLen + bodyLen} addition into a negative / small value that
 * would corrupt capacity checks and array sizing downstream.
 */
class WireTotalLengthTest {

    @Test
    void normalSizesAddUp() {
        assertEquals(4, VesperaWireCodec.wireTotalLength(0, 0));
        assertEquals(114, VesperaWireCodec.wireTotalLength(10, 100));
    }

    @Test
    void overflowThrowsInsteadOfWrapping() {
        // 4 + 10 + Integer.MAX_VALUE overflows a plain `int` add to a
        // negative value; the long-based guard must reject it explicitly.
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.wireTotalLength(10, Integer.MAX_VALUE));
        assertTrue(
                e.getMessage().contains("2 GiB"),
                "message should mention the 2 GiB limit: " + e.getMessage());
    }

    @Test
    void exactlyAtIntMaxIsAccepted() {
        // 4 + 0 + (Integer.MAX_VALUE - 4) == Integer.MAX_VALUE exactly — the
        // largest representable wire request, must NOT throw.
        assertEquals(
                Integer.MAX_VALUE,
                VesperaWireCodec.wireTotalLength(0, Integer.MAX_VALUE - 4));
    }

    @Test
    void oneOverIntMaxThrows() {
        // 4 + 1 + (Integer.MAX_VALUE - 4) == Integer.MAX_VALUE + 1 → reject.
        assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.wireTotalLength(1, Integer.MAX_VALUE - 4));
    }
}
