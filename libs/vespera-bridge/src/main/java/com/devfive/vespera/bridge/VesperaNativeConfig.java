package com.devfive.vespera.bridge;

import java.lang.System.Logger.Level;

/**
 * Process-wide Rust-engine configuration seeded through the optional
 * {@code VesperaBridge.configure*0} JNI hooks.
 *
 * <p>Split out of {@link VesperaBridge} (which must keep the {@code native}
 * declarations because their JNI symbol names are bound to that class) so the
 * pending-config bookkeeping lives in one place.  Every knob follows the same
 * <em>pending-config</em> pattern as {@code configureStreaming}: a value set
 * before {@link VesperaBridge#init(String)} is stored and applied right after
 * the native library loads (before any dispatch); a value set afterwards is
 * applied immediately.  Precedence: programmatic &gt; system property &gt;
 * Rust-side environment variable &gt; built-in default.
 *
 * <p>Every hook returns whether the Rust side applied the value.  A
 * {@code false} (the setting was already fixed — e.g. the runtime already
 * started or an earlier dispatch read the value) is logged as a warning
 * instead of being silently dropped.  A native library that predates a hook
 * ({@link UnsatisfiedLinkError}) is tolerated: the knob falls back to the
 * Rust-side environment variable / default, with a warning only when a value
 * was actually requested.
 */
final class VesperaNativeConfig {

    private static final System.Logger LOG = System.getLogger(VesperaBridge.class.getName());

    private VesperaNativeConfig() {}

    @FunctionalInterface
    interface StreamingConfigurator {
        boolean configure(int chunkBytes, int channelCapacity);
    }

    @FunctionalInterface
    interface LongConfigurator {
        boolean configure(long value);
    }

    /**
     * One single-valued knob: its system property, the value meaning
     * "not configured" ({@code unset} — the Rust side leaves the setting
     * untouched for it), the native hook and the pending programmatic value.
     */
    static final class Knob {
        final String label;
        final String property;
        final long unset;
        private final LongConfigurator hook;
        private volatile Long pending;
        /** Last value the native side confirmed applying, or {@link Long#MIN_VALUE}. */
        private volatile long applied = Long.MIN_VALUE;

        Knob(String label, String property, long unset, LongConfigurator hook) {
            this.label = label;
            this.property = property;
            this.unset = unset;
            this.hook = hook;
        }

        /** Pending programmatic value, else the system property, else {@code unset}. */
        long resolve() {
            Long value = pending;
            return value != null ? value : Long.getLong(property, unset);
        }

        /**
         * Whether {@code value} means "leave this setting untouched": every
         * value at or below the knob's {@code unset} sentinel ({@code <= 0}
         * for the runtime knobs whose sentinel is {@code 0}, {@code < 0} for
         * the knobs whose sentinel is {@code -1}).
         */
        boolean isUnset(long value) {
            return value <= unset;
        }

        /**
         * Store (before init) or apply (after init) a programmatic value.  An
         * unset value is neither stored nor applied, so it never shadows the
         * system property / environment variable for this knob.
         */
        boolean set(long value, boolean nativeLoaded) {
            if (isUnset(value)) {
                return true;
            }
            if (!nativeLoaded) {
                pending = value;
                return true;
            }
            return apply(value, hook);
        }

        boolean apply(long value, LongConfigurator configurator) {
            try {
                boolean ok = configurator.configure(value);
                if (!ok) {
                    warnNotApplied(label + "=" + value);
                } else if (!isUnset(value)) {
                    applied = value;
                }
                return ok;
            } catch (UnsatisfiedLinkError olderNativeLibrary) {
                if (value != unset) {
                    warnUnsupported(label + "=" + value);
                }
                return false;
            }
        }

        void applyResolved() {
            apply(resolve(), hook);
        }

        /** Whether a programmatic value is pending or the system property is set. */
        boolean explicitlyConfigured() {
            return pending != null || System.getProperty(property) != null;
        }

        /**
         * Whether the native side is known to enforce a positive limit no
         * larger than {@code cap} (a confirmed applied value).
         */
        boolean enforcesAtMost(long cap) {
            long value = applied;
            return cap > 0 && value > 0 && value <= cap;
        }

        void clearPendingForTest() {
            pending = null;
            applied = Long.MIN_VALUE;
        }
    }

    // `<= 0` leaves these untouched on the Rust side; `0` is the unset value.
    static final Knob RUNTIME_WORKER_THREADS = new Knob(
            "runtime.workerThreads", "vespera.runtime.workerThreads", 0,
            v -> VesperaBridge.configureRuntime0(toInt(v)));
    static final Knob RUNTIME_MAX_BLOCKING_THREADS = new Knob(
            "runtime.maxBlockingThreads", "vespera.runtime.maxBlockingThreads", 0,
            v -> VesperaBridge.configureMaxBlockingThreads0(toInt(v)));
    // `0` is meaningful (unlimited / disabled) for these; negative = untouched.
    static final Knob MAX_REQUEST_BYTES = new Knob(
            "limits.maxRequestBytes", "vespera.limits.maxRequestBytes", -1,
            VesperaBridge::configureMaxRequestBytes0);
    static final Knob MAX_STREAMING_REQUEST_BYTES = new Knob(
            "limits.maxStreamingRequestBytes", "vespera.limits.maxStreamingRequestBytes", -1,
            VesperaBridge::configureMaxStreamingRequestBytes0);
    static final Knob MAX_BUFFERED_RESPONSE_BYTES = new Knob(
            "limits.maxBufferedResponseBytes", "vespera.limits.maxBufferedResponseBytes", -1,
            VesperaBridge::configureMaxBufferedResponseBytes0);
    static final Knob DISPATCH_TIMEOUT_MILLIS = new Knob(
            "dispatch.timeoutMillis", "vespera.dispatch.timeoutMillis", -1,
            VesperaBridge::configureDispatchTimeout0);
    static final Knob ASYNC_MAX_IN_FLIGHT = new Knob(
            "async.maxInFlight", "vespera.async.maxInFlight", -1,
            v -> VesperaBridge.configureAsyncMaxInFlight0(toInt(v)));

    private static final Knob[] ALL = {
        RUNTIME_WORKER_THREADS,
        RUNTIME_MAX_BLOCKING_THREADS,
        MAX_REQUEST_BYTES,
        MAX_STREAMING_REQUEST_BYTES,
        MAX_BUFFERED_RESPONSE_BYTES,
        DISPATCH_TIMEOUT_MILLIS,
        ASYNC_MAX_IN_FLIGHT,
    };

    private static volatile Integer pendingChunkBytes = null;
    private static volatile Integer pendingChannelCapacity = null;

    /** Apply every knob right after the native library loaded. */
    static void applyAfterLoad() {
        configureStreamingIfSupported(
                pendingOrProperty(pendingChunkBytes, "vespera.streaming.chunkBytes"),
                pendingOrProperty(pendingChannelCapacity, "vespera.streaming.channelCapacity"),
                VesperaBridge::configureStreaming0);
        for (Knob knob : ALL) {
            knob.applyResolved();
        }
    }

    static boolean setStreaming(int chunkBytes, int channelCapacity, boolean nativeLoaded) {
        if (!nativeLoaded) {
            pendingChunkBytes = chunkBytes;
            pendingChannelCapacity = channelCapacity;
            return true;
        }
        return configureStreamingIfSupported(
                chunkBytes, channelCapacity, VesperaBridge::configureStreaming0);
    }

    static boolean configureStreamingIfSupported(
            int chunkBytes, int channelCapacity, StreamingConfigurator configurator) {
        String requested = "streaming.chunkBytes=" + chunkBytes
                + ", streaming.channelCapacity=" + channelCapacity;
        try {
            boolean applied = configurator.configure(chunkBytes, channelCapacity);
            if (!applied) {
                warnNotApplied(requested);
            }
            return applied;
        } catch (UnsatisfiedLinkError olderNativeLibrary) {
            // Pre-0.2 native libraries don't export configureStreaming0;
            // streaming config then falls back to env vars / defaults.
            if (chunkBytes > 0 || channelCapacity > 0) {
                warnUnsupported(requested);
            }
            return false;
        }
    }

    static int pendingOrProperty(Integer pendingValue, String propertyName) {
        return pendingValue != null ? pendingValue : Integer.getInteger(propertyName, 0);
    }

    static int toInt(long value) {
        return (int) Math.max(Integer.MIN_VALUE, Math.min(Integer.MAX_VALUE, value));
    }

    private static void warnNotApplied(String requested) {
        LOG.log(Level.WARNING, "vespera: " + requested + " was not applied: the setting "
                + "is already fixed for this process (configure it before "
                + "VesperaBridge.init / the first dispatch)");
    }

    private static void warnUnsupported(String requested) {
        LOG.log(Level.WARNING, "vespera: " + requested + " ignored: the loaded native "
                + "library does not support this setting (upgrade the Rust side)");
    }
}
