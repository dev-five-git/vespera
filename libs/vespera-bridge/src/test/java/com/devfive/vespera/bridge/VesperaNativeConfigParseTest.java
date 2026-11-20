package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

/**
 * Source of truth for the integer system-property rule that the demo apps'
 * E2E tests copy ({@code NonHeaderStreamingE2ETest.parseBridgeInteger},
 * {@code ReactiveProxyE2ETest.parseBridgeInteger}): the demos keep their
 * copies for their assertions, and these tests pin the real
 * {@link VesperaNativeConfig} behaviour those copies must match.
 */
class VesperaNativeConfigParseTest {

    private static final String PROPERTY = "vespera.test.parseIntegerProperty";

    @AfterEach
    void clearProperty() {
        System.clearProperty(PROPERTY);
    }

    @Test
    void decimalWithAnOptionalSign() {
        assertEquals(42, VesperaNativeConfig.parseIntegerProperty("42"));
        assertEquals(42, VesperaNativeConfig.parseIntegerProperty("+42"));
        assertEquals(-42, VesperaNativeConfig.parseIntegerProperty("-42"));
    }

    @Test
    void aLeadingZeroIsDecimalNotOctal() {
        // Long.decode would read 0100 as octal (64) and reject 0524288.
        assertEquals(100, VesperaNativeConfig.parseIntegerProperty("0100"));
        assertEquals(524288, VesperaNativeConfig.parseIntegerProperty("0524288"));
        assertEquals(1000000, VesperaNativeConfig.parseIntegerProperty("01000000"));
    }

    @Test
    void hexadecimalWithPrefixAndOptionalSign() {
        assertEquals(16, VesperaNativeConfig.parseIntegerProperty("0x10"));
        assertEquals(16, VesperaNativeConfig.parseIntegerProperty("0X10"));
        assertEquals(255, VesperaNativeConfig.parseIntegerProperty("#ff"));
        assertEquals(-16, VesperaNativeConfig.parseIntegerProperty("-0x10"));
        assertEquals(16, VesperaNativeConfig.parseIntegerProperty("+#10"));
    }

    @Test
    void aSignAfterTheRadixPrefixOrAnyJunkIsRejected() {
        assertThrows(NumberFormatException.class, () -> VesperaNativeConfig.parseIntegerProperty("0x-1"));
        assertThrows(NumberFormatException.class, () -> VesperaNativeConfig.parseIntegerProperty("+-1"));
        assertThrows(NumberFormatException.class, () -> VesperaNativeConfig.parseIntegerProperty("64MiB"));
        assertThrows(NumberFormatException.class, () -> VesperaNativeConfig.parseIntegerProperty("1_000"));
        assertThrows(NumberFormatException.class, () -> VesperaNativeConfig.parseIntegerProperty(""));
        assertThrows(NumberFormatException.class, () -> VesperaNativeConfig.parseIntegerProperty("-"));
    }

    @Test
    void longPropertyTrimsWhitespaceAndFallsBackWhenUnsetOrMalformed() {
        assertEquals(99, VesperaNativeConfig.longProperty(PROPERTY, 99), "unset: fallback");

        System.setProperty(PROPERTY, "  42  ");
        assertEquals(42, VesperaNativeConfig.longProperty(PROPERTY, 99));

        System.setProperty(PROPERTY, "+0100");
        assertEquals(100, VesperaNativeConfig.longProperty(PROPERTY, 99));

        System.setProperty(PROPERTY, "64MiB");
        assertEquals(99, VesperaNativeConfig.longProperty(PROPERTY, 99));
    }

    @Test
    void aPendingValueWinsOverTheSystemProperty() {
        System.setProperty(PROPERTY, "7");
        assertEquals(3, VesperaNativeConfig.pendingOrProperty(3, PROPERTY));
        assertEquals(7, VesperaNativeConfig.pendingOrProperty(null, PROPERTY));
    }
}
