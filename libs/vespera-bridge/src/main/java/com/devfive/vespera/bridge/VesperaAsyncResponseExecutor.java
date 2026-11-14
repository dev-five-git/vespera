package com.devfive.vespera.bridge;

import java.util.Objects;
import java.util.concurrent.Executor;
import java.util.concurrent.ExecutorService;

/**
 * Holder of the executor that builds {@code ASYNC} proxy responses (the
 * {@code vesperaBridgeAsyncResponseExecutor} bean).
 *
 * <p>Deliberately <em>not</em> an {@link Executor} itself: Spring Boot's
 * {@code TaskExecutionAutoConfiguration} only creates its
 * {@code applicationTaskExecutor} when the context holds no {@code Executor}
 * bean, so exposing the bridge's private pool as a plain
 * {@code ExecutorService} bean silently removed the application's
 * {@code @Async} / MVC async executor. Replace the bean (by name) with
 * {@link #of(Executor)} to supply your own executor; a replacement bean that
 * is a plain {@code Executor} is still accepted, at the cost of that Boot
 * back-off.
 *
 * <p>{@link #close()} shuts the wrapped executor down when it is an
 * {@link ExecutorService} (Spring calls it on context close).
 */
public final class VesperaAsyncResponseExecutor implements AutoCloseable {

    private final Executor executor;

    private VesperaAsyncResponseExecutor(Executor executor) {
        this.executor = Objects.requireNonNull(executor, "executor");
    }

    /** Wraps {@code executor}; {@link #close()} shuts it down if it is an {@link ExecutorService}. */
    public static VesperaAsyncResponseExecutor of(Executor executor) {
        return new VesperaAsyncResponseExecutor(executor);
    }

    /** The wrapped executor. */
    public Executor executor() {
        return executor;
    }

    @Override
    public void close() {
        if (executor instanceof ExecutorService service) {
            service.shutdown();
        }
    }

    /**
     * Resolves the {@code vesperaBridgeAsyncResponseExecutor} bean value: a
     * holder (the default) or a user-supplied plain {@link Executor}.
     */
    static Executor unwrap(Object bean) {
        if (bean instanceof VesperaAsyncResponseExecutor holder) {
            return holder.executor();
        }
        if (bean instanceof Executor executor) {
            return executor;
        }
        throw new IllegalStateException(
                "Bean 'vesperaBridgeAsyncResponseExecutor' must be a VesperaAsyncResponseExecutor or an Executor, got "
                        + (bean == null ? "null" : bean.getClass().getName()));
    }
}
