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
    void exactlyAtArrayCeilingIsAccepted() {
        // 4 + 0 + (Integer.MAX_VALUE - 12) == Integer.MAX_VALUE - 8 — the
        // largest wire request a byte[] can hold, must NOT throw.
        assertEquals(
                Integer.MAX_VALUE - 8,
                VesperaWireCodec.wireTotalLength(0, Integer.MAX_VALUE - 12));
    }

    @Test
    void oneOverArrayCeilingThrows() {
        // Integer.MAX_VALUE - 7: representable as an int, but HotSpot cannot
        // allocate the array (OutOfMemoryError) — reject it up front.
        assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.wireTotalLength(1, Integer.MAX_VALUE - 12));
    }

    @Test
    void exactlyAtIntMaxThrows() {
        // 4 + 0 + (Integer.MAX_VALUE - 4) == Integer.MAX_VALUE: past the
        // array ceiling, so an IllegalArgumentException, not an OOM later.
        assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.wireTotalLength(0, Integer.MAX_VALUE - 4));
    }
}
