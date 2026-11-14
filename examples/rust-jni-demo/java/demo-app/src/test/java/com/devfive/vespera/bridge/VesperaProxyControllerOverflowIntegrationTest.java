package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.util.UUID;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * The DIRECT overflow fallbacks against the real cdylib, observed through the
 * handler run counter ({@code /diag/runs/<key>}) and the controller's
 * overflow memory ({@code avoidsDirect}) instead of only the response body.
 */
class VesperaProxyControllerOverflowIntegrationTest {

    /** Beyond the 4 MiB pooled DIRECT cap: the response is stashed natively. */
    private static final int BEYOND_POOL_CAP = 5 * 1024 * 1024;

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

    private static VesperaProxyController controller(boolean retryOnOverflow) {
        return new VesperaProxyController(
                request -> null,
                request -> DispatchMode.DIRECT,
                Runnable::run,
                retryOnOverflow);
    }

    private static String key() {
        return "ovf" + UUID.randomUUID().toString().replace("-", "");
    }

    private static int runs(String key) throws Exception {
        MockHttpServletResponse response = new MockHttpServletResponse();
        VesperaProxyController.dispatchSync(
                response, null, "GET", "/diag/runs/" + key, "", sink -> {}, VesperaWireCodec.EMPTY_BODY);
        assertEquals(200, response.getStatus());
        return Integer.parseInt(response.getContentAsString(StandardCharsets.UTF_8));
    }

    /**
     * {@code oversize-policy=throw} on a virtual thread refuses the heap
     * fallback before anything is dispatched: the request is streamed once
     * (the handler runs exactly once, no 500 even without retry) and the route
     * is not remembered as an overflowing one.
     */
    @ParameterizedTest
    @ValueSource(booleans = {true, false})
    void rejectedPooledFallbackStreamsOnceAndIsNotRememberedAsAnOverflow(boolean retry) throws Exception {
        System.setProperty("vespera.direct.oversize-policy", "throw");
        VesperaProxyController controller = controller(retry);
        String key = key();
        String path = "/diag/blob/" + key + "/16";

        MockHttpServletResponse response = new MockHttpServletResponse();
        controller.dispatchDirectMode(
                response, null, "GET", path, "", sink -> {}, VesperaWireCodec.EMPTY_BODY, Boolean.TRUE);

        assertEquals(200, response.getStatus());
        assertEquals("x".repeat(16), response.getContentAsString(StandardCharsets.UTF_8));
        assertEquals(1, runs(key), "streamed once, not dispatched twice");
        assertFalse(controller.avoidsDirect(null, "GET", path, ""), "not an overflow");
    }

    /**
     * A response beyond the pooled cap on a platform thread is stashed by the
     * first dispatch and streamed from the stash (handler runs once); the route
     * is remembered, so the next request streams up front.
     */
    @Test
    void realOverflowStreamsFromTheStashAndTheRouteAvoidsDirectNextTime() throws Exception {
        VesperaProxyController controller = controller(true);
        String key = key();
        String path = "/diag/blob/" + key + "/" + BEYOND_POOL_CAP;
        assertFalse(controller.avoidsDirect(null, "GET", path, ""));

        MockHttpServletResponse overflow = new MockHttpServletResponse();
        controller.dispatchDirectMode(
                overflow, null, "GET", path, "", sink -> {}, VesperaWireCodec.EMPTY_BODY, Boolean.FALSE);

        assertEquals(200, overflow.getStatus());
        assertEquals(BEYOND_POOL_CAP, overflow.getContentAsByteArray().length);
        assertEquals(1, runs(key), "the stashed response is streamed, the handler is not re-run");
        assertTrue(controller.avoidsDirect(null, "GET", path, ""), "overflow remembered");

        MockHttpServletRequest request = new MockHttpServletRequest("GET", path);
        request.setRequestURI(path);
        MockHttpServletResponse remembered = new MockHttpServletResponse();
        assertNull(controller.proxy(request, remembered));
        assertEquals(200, remembered.getStatus());
        assertEquals(BEYOND_POOL_CAP, remembered.getContentAsByteArray().length);
        assertEquals(2, runs(key), "streamed up front: one more run, no DIRECT attempt plus re-run");
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
