package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

class VesperaProxyControllerOverflowIntegrationTest {

    private static String previousOversizePolicy;

    @BeforeAll
    static void loadNativeLibrary() {
        VesperaBridge.init("rust_jni_demo");
        previousOversizePolicy = System.getProperty("vespera.direct.oversize-policy");
    }

    @AfterEach
    void restoreOversizePolicy() {
        if (previousOversizePolicy == null) {
            System.clearProperty("vespera.direct.oversize-policy");
        } else {
            System.setProperty("vespera.direct.oversize-policy", previousOversizePolicy);
        }
        VesperaBridge.clearCurrentThreadBuffers();
    }

    @Test
    void safeDirectOverflowStreamsAndFutureRequestAvoidsDirect() throws Exception {
        System.setProperty("vespera.direct.oversize-policy", "throw");
        VesperaProxyController controller = new VesperaProxyController(
                request -> null,
                request -> DispatchMode.DIRECT,
                Runnable::run,
                true);

        MockHttpServletResponse overflowResponse = new MockHttpServletResponse();
        controller.dispatchDirectMode(
                overflowResponse,
                null,
                "GET",
                "/health",
                "",
                sink -> {},
                VesperaWireCodec.EMPTY_BODY,
                Boolean.TRUE);

        assertEquals(200, overflowResponse.getStatus());
        assertEquals("ok", overflowResponse.getContentAsString(StandardCharsets.UTF_8));

        MockHttpServletRequest request = new MockHttpServletRequest("GET", "/health");
        request.setRequestURI("/health");
        MockHttpServletResponse rememberedResponse = new MockHttpServletResponse();

        assertNull(controller.proxy(request, rememberedResponse));
        assertEquals(200, rememberedResponse.getStatus());
        assertEquals("ok", rememberedResponse.getContentAsString(StandardCharsets.UTF_8));
    }

    @Test
    void legacySyncDelegateCompletesAgainstTheLoadedNativeLibrary() throws Exception {
        MockHttpServletResponse response = new MockHttpServletResponse();

        VesperaProxyController.dispatchSync(
                response,
                null,
                "GET",
                "/health",
                "",
                sink -> {},
                VesperaWireCodec.EMPTY_BODY);

        assertEquals(200, response.getStatus());
        assertEquals("ok", response.getContentAsString(StandardCharsets.UTF_8));
    }
}
