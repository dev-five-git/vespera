package com.devfive.vespera.bridge;

import jakarta.servlet.ServletRequestEvent;
import jakarta.servlet.ServletRequestListener;
import org.springframework.context.ApplicationContext;
import org.springframework.context.ApplicationContextAware;
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
 * <p>Only the close of the context this bean belongs to counts: Spring also
 * publishes a child context's {@link ContextClosedEvent} to its parent, and a
 * closed child (per-job contexts, {@code SpringApplicationBuilder.child})
 * must not disable buffer pooling for the still-running web context.  An
 * instance created outside a context (no {@link #setApplicationContext}) reacts
 * to any close event.
 *
 * <p>Normal request handling is unchanged: before shutdown, request destruction
 * only reads one volatile boolean and leaves pooling intact.
 */
public final class VesperaBridgeThreadLocalCleanup
        implements ServletRequestListener, ApplicationListener<ContextClosedEvent>,
                ApplicationContextAware {

    private volatile boolean closing;
    private volatile ApplicationContext context;

    @Override
    public void setApplicationContext(ApplicationContext applicationContext) {
        this.context = applicationContext;
    }

    @Override
    public void onApplicationEvent(ContextClosedEvent event) {
        ApplicationContext own = context;
        if (own != null && event.getApplicationContext() != own) {
            return;
        }
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
