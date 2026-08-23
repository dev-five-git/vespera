package com.devfive.vespera.bridge;

import jakarta.servlet.ServletRequestEvent;
import jakarta.servlet.ServletRequestListener;
import org.springframework.context.ApplicationListener;
import org.springframework.context.event.ContextClosedEvent;

/**
 * Spring lifecycle hook for vespera-bridge ThreadLocal buffers.
 *
 * <p>The DIRECT fast path intentionally keeps per-thread direct buffers hot in
 * {@link VesperaDirectBufferPool}.  A static {@code ThreadLocal} can otherwise
 * retain buffers on servlet worker threads across webapp redeploys when the
 * worker pool outlives the Spring context.  This listener marks the context as
 * closing and clears vespera buffers from any worker thread as its in-flight
 * request is destroyed; it also clears the thread that receives the close event.
 *
 * <p>Normal request handling is unchanged: before shutdown, request destruction
 * only reads one volatile boolean and leaves pooling intact.
 */
public final class VesperaBridgeThreadLocalCleanup
        implements ServletRequestListener, ApplicationListener<ContextClosedEvent> {

    private volatile boolean closing;

    @Override
    public void onApplicationEvent(ContextClosedEvent event) {
        closing = true;
        VesperaBridge.clearCurrentThreadBuffers();
    }

    @Override
    public void requestDestroyed(ServletRequestEvent sre) {
        if (closing) {
            VesperaBridge.clearCurrentThreadBuffers();
        }
    }
}
