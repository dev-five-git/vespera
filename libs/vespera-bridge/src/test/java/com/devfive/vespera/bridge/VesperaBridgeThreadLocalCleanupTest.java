package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.ServletRequestEvent;
import org.junit.jupiter.api.Test;
import org.springframework.context.event.ContextClosedEvent;
import org.springframework.context.support.StaticApplicationContext;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockServletContext;

class VesperaBridgeThreadLocalCleanupTest {

    @Test
    void requestDestructionOnlyClearsBuffersAfterContextStartsClosing() {
        VesperaBridgeThreadLocalCleanup cleanup = new VesperaBridgeThreadLocalCleanup();
        ServletRequestEvent requestEvent = new ServletRequestEvent(
                new MockServletContext(), new MockHttpServletRequest());

        VesperaDirectBufferPool.directPoolForTest();
        cleanup.requestDestroyed(requestEvent);
        assertTrue(VesperaDirectBufferPool.directPoolPresentForTest());

        StaticApplicationContext context = new StaticApplicationContext();
        cleanup.onApplicationEvent(new ContextClosedEvent(context));
        assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());

        VesperaDirectBufferPool.directPoolForTest();
        cleanup.requestDestroyed(requestEvent);
        assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());
        context.close();
    }

    @Test
    void aClosingChildContextDoesNotDisablePooling() {
        StaticApplicationContext parent = new StaticApplicationContext();
        StaticApplicationContext child = new StaticApplicationContext(parent);
        VesperaBridgeThreadLocalCleanup cleanup = new VesperaBridgeThreadLocalCleanup();
        cleanup.setApplicationContext(parent);
        ServletRequestEvent requestEvent = new ServletRequestEvent(
                new MockServletContext(), new MockHttpServletRequest());

        // A child's close event is also delivered to the parent's listeners.
        cleanup.onApplicationEvent(new ContextClosedEvent(child));
        VesperaDirectBufferPool.directPoolForTest();
        cleanup.requestDestroyed(requestEvent);
        assertTrue(VesperaDirectBufferPool.directPoolPresentForTest(), "pooling stays on");

        cleanup.onApplicationEvent(new ContextClosedEvent(parent));
        VesperaDirectBufferPool.directPoolForTest();
        cleanup.requestDestroyed(requestEvent);
        assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());
        child.close();
        parent.close();
    }
}
