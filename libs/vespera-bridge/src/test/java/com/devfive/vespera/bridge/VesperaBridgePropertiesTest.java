package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

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

        assertEquals("X-Target-App", properties.getAppHeader());
        assertFalse(properties.isControllerEnabled());
        assertEquals("bidirectional-streaming", properties.getDispatchMode());
        assertFalse(properties.isDirectRetryOnOverflow());
        assertEquals(1234L, properties.getMaxBufferedRequestBytes());
        assertEquals(5678L, properties.getMaxBufferedResponseBytes());
        assertEquals(7, properties.getAsyncPoolSize());
        assertTrue(properties.isClearThreadlocalsAfterRequest());
    }
}
