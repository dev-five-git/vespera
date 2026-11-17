package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.time.Duration;
import org.junit.jupiter.api.Test;

class VesperaBridgePropertiesTest {

    @Test
    void defaultsMatchDocumentedZeroConfigurationBehavior() {
        VesperaBridgeProperties properties = new VesperaBridgeProperties();

        assertEquals("X-Vespera-App", properties.getAppHeader());
        assertTrue(properties.isControllerEnabled());
        assertEquals("smart", properties.getDispatchMode());
        assertTrue(properties.isDirectRetryOnOverflow());
        assertEquals(VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES,
                properties.getMaxBufferedRequestBytes());
        assertEquals(VesperaProxyController.DEFAULT_MAX_BUFFERED_RESPONSE_BYTES,
                properties.getMaxBufferedResponseBytes());
        assertEquals(0, properties.getAsyncPoolSize());
        assertFalse(properties.isClearThreadlocalsAfterRequest());
        assertNull(properties.getExposedApps(), "absent property is unrestricted");
    }

    @Test
    void everyConfigurationPropertyCanBeUpdatedAndReadBack() {
        VesperaBridgeProperties properties = new VesperaBridgeProperties();

        properties.setAppHeader("X-Target-App");
        properties.setControllerEnabled(false);
        properties.setDispatchMode("bidirectional-streaming");
        properties.setDirectRetryOnOverflow(false);
        properties.setMaxBufferedRequestBytes(1234L);
        properties.setMaxBufferedResponseBytes(5678L);
        properties.setAsyncPoolSize(7);
        properties.setClearThreadlocalsAfterRequest(true);
        properties.setExposedApps(java.util.List.of("public"));

        assertEquals("X-Target-App", properties.getAppHeader());
        assertFalse(properties.isControllerEnabled());
        assertEquals("bidirectional-streaming", properties.getDispatchMode());
        assertFalse(properties.isDirectRetryOnOverflow());
        assertEquals(1234L, properties.getMaxBufferedRequestBytes());
        assertEquals(5678L, properties.getMaxBufferedResponseBytes());
        assertEquals(7, properties.getAsyncPoolSize());
        assertTrue(properties.isClearThreadlocalsAfterRequest());
        assertEquals(java.util.List.of("public"), properties.getExposedApps());
        properties.setExposedApps(null);
        assertNull(properties.getExposedApps());
    }

    @Test
    void subMillisecondDispatchTimeoutRoundsUpInsteadOfDisabling() {
        assertEquals(0, VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ZERO));
        assertEquals(1, VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ofNanos(1)));
        assertEquals(1, VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ofNanos(500_000)));
        assertEquals(1, VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ofMillis(1)));
        assertEquals(1501, VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ofNanos(1_500_000_001)));
        assertEquals(30_000, VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ofSeconds(30)));
        assertEquals(Long.MAX_VALUE,
                VesperaBridgeProperties.Native.dispatchTimeoutMillis(Duration.ofSeconds(Long.MAX_VALUE)));
    }

    @Test
    void negativeDispatchTimeoutIsRejectedAtBinding() {
        VesperaBridgeProperties.Native nativeSettings = new VesperaBridgeProperties.Native();
        IllegalArgumentException e = assertThrows(IllegalArgumentException.class,
                () -> nativeSettings.setDispatchTimeout(Duration.ofSeconds(-1)));
        assertTrue(e.getMessage().contains("dispatch-timeout"), e.getMessage());
        nativeSettings.setDispatchTimeout(Duration.ofMillis(5));
        assertEquals(Duration.ofMillis(5), nativeSettings.getDispatchTimeout());
        nativeSettings.setDispatchTimeout(null);
        assertEquals(null, nativeSettings.getDispatchTimeout());
    }
}
