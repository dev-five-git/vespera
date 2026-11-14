package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.AsyncEvent;
import jakarta.servlet.AsyncListener;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.jupiter.api.Test;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockAsyncContext;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.web.context.request.async.DeferredResult;
import org.springframework.web.context.request.async.StandardServletAsyncWebRequest;
import org.springframework.web.context.request.async.WebAsyncManager;
import org.springframework.web.context.request.async.WebAsyncUtils;

/**
 * An ASYNC dispatch that outlives Spring's async request timeout answers the
 * {@link ProxyErrors} plain-text {@code 503} (with the request id echoed),
 * not an {@code AsyncRequestTimeoutException} that Spring Boot would render
 * through its {@code /error} page.
 */
class AsyncTimeoutResultTest {

    private static Object timeOut(DeferredResult<ResponseEntity<?>> result) throws Exception {
        MockHttpServletRequest request = new MockHttpServletRequest("GET", "/slow");
        request.setAsyncSupported(true);
        MockHttpServletResponse servlet = new MockHttpServletResponse();
        WebAsyncManager manager = WebAsyncUtils.getAsyncManager(request);
        manager.setAsyncWebRequest(new StandardServletAsyncWebRequest(request, servlet));
        manager.startDeferredResultProcessing(result);
        MockAsyncContext context = (MockAsyncContext) request.getAsyncContext();
        for (AsyncListener listener : context.getListeners()) {
            listener.onTimeout(new AsyncEvent(context));
        }
        assertTrue(manager.hasConcurrentResult());
        return manager.getConcurrentResult();
    }

    @Test
    void timeoutAnswersAPlainText503WithTheRequestId() throws Exception {
        ProxyServletResponse proxied = new ProxyServletResponse(
                new MockHttpServletResponse(), "X-Request-Id", "rid-42");
        DeferredResult<ResponseEntity<?>> result = VesperaProxyController.newAsyncResult(proxied);
        AtomicBoolean timeoutCallback = new AtomicBoolean();
        result.onTimeout(() -> timeoutCallback.set(true));

        ResponseEntity<?> entity = assertInstanceOf(ResponseEntity.class, timeOut(result));

        assertTrue(timeoutCallback.get(), "onTimeout still runs (cancels the native future)");
        assertEquals(503, entity.getStatusCode().value());
        assertEquals("text/plain", entity.getHeaders().getContentType().getType() + "/" + entity.getHeaders().getContentType().getSubtype());
        assertEquals("rid-42", entity.getHeaders().getFirst("X-Request-Id"));
        assertEquals("vespera: async dispatch timed out",
                new String((byte[]) entity.getBody(), StandardCharsets.UTF_8));
    }

    @Test
    void timeoutWithoutARequestIdHeaderOmitsIt() throws Exception {
        ProxyServletResponse proxied = new ProxyServletResponse(new MockHttpServletResponse(), null, null);
        ResponseEntity<?> entity = assertInstanceOf(ResponseEntity.class,
                timeOut(VesperaProxyController.newAsyncResult(proxied)));
        assertEquals(503, entity.getStatusCode().value());
        assertNull(entity.getHeaders().getFirst("X-Request-Id"));
    }
}
