package com.devfive.vespera.bridge;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.IOException;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.function.Consumer;
import reactor.core.scheduler.Schedulers;

/** Fake native transport + wire helpers for the WebFlux proxy tests. */
final class ReactiveTestSupport {
    private ReactiveTestSupport() {}

    static final ObjectMapper JSON = new ObjectMapper();

    /** Records every wire request; answers with {@link #response} (or {@link #pending}). */
    static final class FakeTransport implements VesperaReactiveProxyHandler.Transport {
        final List<byte[]> requests = new CopyOnWriteArrayList<>();
        final List<CompletableFuture<byte[]>> futures = new CopyOnWriteArrayList<>();
        volatile boolean initialized = true;
        volatile byte[] response = responseWire(200, "{\"content-type\":\"text/plain\"}", "ok");
        /** When set, dispatchAsync returns this (never-completing) future instead. */
        volatile CompletableFuture<byte[]> pending;
        /** Streaming: header bytes, then each chunk (flushing after each). */
        volatile byte[] streamHeader = responseWire(200, "{\"content-type\":\"text/event-stream\"}", "");
        volatile List<String> streamChunks = List.of("data: a\n\n", "data: b\n\n");
        volatile Throwable streamFailure;
        /** When set, a streaming dispatch blocks on it (holding its thread) before the header. */
        volatile java.util.concurrent.CountDownLatch streamGate;
        /** Released once each time a streaming dispatch starts. */
        final java.util.concurrent.Semaphore streamsEntered = new java.util.concurrent.Semaphore(0);

        @Override
        public boolean isInitialized() {
            return initialized;
        }

        @Override
        public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
            requests.add(wireRequest);
            CompletableFuture<byte[]> future = pending != null
                    ? pending : CompletableFuture.completedFuture(response);
            futures.add(future);
            return future;
        }

        @Override
        public void dispatchStreaming(byte[] wireRequest, Consumer<byte[]> headerConsumer, OutputStream body)
                throws IOException {
            requests.add(wireRequest);
            streamsEntered.release();
            java.util.concurrent.CountDownLatch gate = streamGate;
            if (gate != null) {
                try {
                    gate.await();
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                    throw new IOException(interrupted);
                }
            }
            headerConsumer.accept(streamHeader);
            for (String chunk : streamChunks) {
                body.write(chunk.getBytes(StandardCharsets.UTF_8));
                body.flush();
            }
            if (streamFailure instanceof IOException io) {
                throw io;
            }
            if (streamFailure instanceof RuntimeException runtime) {
                throw runtime;
            }
        }

        JsonNode lastHeader() {
            return header(requests.get(requests.size() - 1));
        }

        String lastBody() {
            byte[] wire = requests.get(requests.size() - 1);
            int len = ByteBuffer.wrap(wire).getInt();
            return new String(wire, 4 + len, wire.length - 4 - len, StandardCharsets.UTF_8);
        }
    }

    static VesperaReactiveProxyHandler handler(FakeTransport transport, ReactiveAppNameResolver apps,
            ReactivePrincipalResolver principals, long maxRequest, long maxResponse, String requestIdHeader,
            List<DispatchObserver> observers, CorsHandling cors) {
        return handler(transport, apps, principals, maxRequest, maxResponse, requestIdHeader, observers, cors,
                new VesperaStreamingExecutor(4, 16));
    }

    static VesperaReactiveProxyHandler handler(FakeTransport transport, ReactiveAppNameResolver apps,
            ReactivePrincipalResolver principals, long maxRequest, long maxResponse, String requestIdHeader,
            List<DispatchObserver> observers, CorsHandling cors, VesperaStreamingExecutor streaming) {
        return new VesperaReactiveProxyHandler(apps, principals, maxRequest, maxResponse, requestIdHeader,
                observers, cors, transport, Schedulers.parallel(), streaming);
    }

    static byte[] responseWire(int status, String headersJson, String body) {
        return responseWire(status, headersJson, "{\"version\":\"0.1.0\"}", body);
    }

    static byte[] responseWire(int status, String headersJson, String metadataJson, String body) {
        String header = "{\"v\":1,\"status\":" + status + ",\"headers\":" + headersJson
                + ",\"metadata\":" + metadataJson + "}";
        byte[] h = header.getBytes(StandardCharsets.UTF_8);
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        return ByteBuffer.allocate(4 + h.length + b.length).putInt(h.length).put(h).put(b).array();
    }

    /** Status, headers and body of a WebTestClient exchange. */
    record Result(int status, org.springframework.http.HttpHeaders headers, String body) {
        String header(String name) {
            return headers.getFirst(name);
        }
    }

    /**
     * Runs the exchange and collects the result WITHOUT the WebTestClient
     * assertion DSL: its assertion classes changed return types between
     * Spring 6.2 and 7 (binary-incompatible under testBoot3), while
     * {@code exchange()} / {@code returnResult(..)} are stable.
     */
    static Result call(org.springframework.test.web.reactive.server.WebTestClient.RequestHeadersSpec<?> spec) {
        org.springframework.test.web.reactive.server.FluxExchangeResult<byte[]> result =
                spec.exchange().returnResult(byte[].class);
        java.io.ByteArrayOutputStream body = new java.io.ByteArrayOutputStream();
        result.getResponseBody().toIterable().forEach(chunk -> body.write(chunk, 0, chunk.length));
        return new Result(result.getStatus().value(), result.getResponseHeaders(),
                body.toString(StandardCharsets.UTF_8));
    }

    static JsonNode header(byte[] wire) {
        int len = ByteBuffer.wrap(wire).getInt();
        try {
            return JSON.readTree(new String(wire, 4, len, StandardCharsets.UTF_8));
        } catch (IOException failure) {
            throw new UncheckedIOException(failure);
        }
    }
}
