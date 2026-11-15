package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.Test;

/** {@link VesperaClient} / {@link VesperaRequest} / {@link VesperaResponse} over a fake transport. */
class VesperaClientTest {

    private static final ObjectMapper JSON = new ObjectMapper();

    /** Records every wire request and answers a canned wire response. */
    private static class FakeTransport implements VesperaClient.Transport {
        final List<byte[]> requests = new ArrayList<>();
        byte[] response = responseWire(200, "{\"content-type\":\"text/plain\"}", null, "ok");
        byte[] streamedBody = new byte[0];
        byte[] uploaded;
        RuntimeException failure;
        Error error;

        @Override
        public byte[] dispatch(byte[] wireRequest) {
            requests.add(wireRequest);
            if (error != null) {
                throw error;
            }
            if (failure != null) {
                throw failure;
            }
            return response;
        }

        @Override
        public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
            requests.add(wireRequest);
            if (error != null) {
                throw error;
            }
            if (failure != null) {
                throw failure;
            }
            return CompletableFuture.completedFuture(response);
        }

        @Override
        public byte[] dispatchStreaming(byte[] header, InputStream in, OutputStream out)
                throws IOException {
            requests.add(header);
            uploaded = in.readAllBytes();
            out.write(streamedBody);
            return response;
        }
    }

    static byte[] responseWire(int status, String headersJson, String validationErrorsJson, String body) {
        String header = "{\"v\":1,\"status\":" + status + ",\"headers\":" + headersJson
                + ",\"metadata\":{\"version\":\"0.1.0\"}"
                + (validationErrorsJson == null ? "" : ",\"validation_errors\":" + validationErrorsJson)
                + "}";
        byte[] h = header.getBytes(StandardCharsets.UTF_8);
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        return ByteBuffer.allocate(4 + h.length + b.length).putInt(h.length).put(h).put(b).array();
    }

    private static JsonNode header(byte[] wire) throws IOException {
        int len = ByteBuffer.wrap(wire).getInt();
        return JSON.readTree(new String(wire, 4, len, StandardCharsets.UTF_8));
    }

    private static byte[] body(byte[] wire) {
        int len = ByteBuffer.wrap(wire).getInt();
        byte[] out = new byte[wire.length - 4 - len];
        System.arraycopy(wire, 4 + len, out, 0, out.length);
        return out;
    }

    private final FakeTransport transport = new FakeTransport();
    private final VesperaClient client = new VesperaClient(transport, null);

    @Test
    void getEncodesMethodPathHeadersAndOmitsDefaultApp() throws IOException {
        VesperaResponse r = client.get("/users/42").header("Accept", "application/json").send();
        JsonNode h = header(transport.requests.get(0));
        assertEquals(1, h.get("v").asInt());
        assertEquals("GET", h.get("method").asText());
        assertEquals("/users/42", h.get("path").asText());
        assertEquals("application/json", h.get("headers").get("accept").asText());
        assertFalse(h.has("app"));
        assertFalse(h.has("query"));
        assertFalse(h.has("remote_addr"));
        assertEquals(0, body(transport.requests.get(0)).length);
        assertEquals(200, r.status());
        assertTrue(r.isSuccessful());
        assertEquals("ok", r.bodyString());
        assertEquals("text/plain", r.header("Content-Type"));
        assertEquals("0.1.0", r.metadata().get("version"));
        assertFalse(r.hasValidationErrors());
        assertTrue(r.validationErrors().isEmpty());
        assertFalse(r.panicked());
    }

    @Test
    void queryParametersArePercentEncodedAndAppendedToAPathQuery() throws IOException {
        Map<String, Object> more = new LinkedHashMap<>();
        more.put("flag", null);
        more.put("n", 3);
        VesperaRequest req = client.get("/search?page=2")
                .query("q", "a b&c=d/é")
                .query("tag", "x+y")
                .query(more)
                .rawQuery("raw=%2F");
        req.send();
        assertEquals("/search", req.path());
        String expected = "page=2&q=a%20b%26c%3Dd%2F%C3%A9&tag=x%2By&flag&n=3&raw=%2F";
        assertEquals(expected, req.queryString());
        // The codec sends the target as one combined "path" field.
        assertEquals("/search?" + expected, header(transport.requests.get(0)).get("path").asText());
        assertEquals("a-b_c.d~1", VesperaClient.encodePathSegment("a-b_c.d~1"));
        assertEquals("a%2Fb%20c", VesperaClient.encodePathSegment("a/b c"));
    }

    @Test
    void headersAreLowerCasedReplacedAndJoined() throws IOException {
        client.post("/h")
                .header("X-One", "1")
                .header("x-one", "2")
                .addHeader("Accept", "a")
                .addHeader("accept", "b")
                .addHeader("Cookie", "a=1")
                .addHeader("cookie", "b=2")
                .headers(Map.of("X-Gone", "z"))
                .removeHeader("X-GONE")
                .send();
        JsonNode headers = header(transport.requests.get(0)).get("headers");
        assertEquals("2", headers.get("x-one").asText());
        assertEquals("a, b", headers.get("accept").asText());
        assertEquals("a=1; b=2", headers.get("cookie").asText());
        assertFalse(headers.has("x-gone"));
    }

    @Test
    void invalidInputsAreRejectedBeforeDispatch() {
        assertThrows(IllegalArgumentException.class, () -> client.get("users"));
        assertThrows(IllegalArgumentException.class, () -> client.get(""));
        assertThrows(IllegalArgumentException.class, () -> client.get("/a#frag"));
        assertThrows(IllegalArgumentException.class, () -> client.request("GE T", "/"));
        assertThrows(IllegalArgumentException.class, () -> client.get("/").header("bad name", "v"));
        assertThrows(IllegalArgumentException.class, () -> client.get("/").header("x", "a\r\nb: c"));
        // Every control character but HTAB is rejected (RFC 9110 field-value).
        for (char c : new char[] {0x00, 0x01, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x1B, 0x1F, 0x7F}) {
            assertThrows(IllegalArgumentException.class,
                    () -> client.get("/").header("x", "a" + c + "b"), "0x" + Integer.toHexString(c));
        }
        client.get("/").header("x", "a\tb ~é"); // HTAB, SP, visible ASCII, obs-text: fine
        assertThrows(NullPointerException.class, () -> client.get("/").header("x", null));
        assertThrows(NullPointerException.class, () -> client.get(null));
        assertTrue(transport.requests.isEmpty());
    }

    @Test
    void rawQueryRejectsAFragmentLikeThePath() {
        // The URI parser would cut the query at '#' and silently drop `b=2`.
        assertThrows(IllegalArgumentException.class, () -> client.get("/s").rawQuery("a=1#x&b=2"));
        assertEquals(0, transport.requests.size());
    }

    @Test
    void unpairedSurrogatesBecomeTheReplacementCharacterEverywhere() throws IOException {
        String lone = "a\uD800";
        client.post("/s").query("q", lone).text(lone).send();
        byte[] wire = transport.requests.get(0);
        // U+FFFD (EF BF BD), as in the path and the headers — never '?' (%3F).
        assertEquals("/s?q=a%EF%BF%BD", header(wire).get("path").asText());
        assertArrayEquals(new byte[] {'a', (byte) 0xEF, (byte) 0xBF, (byte) 0xBD}, body(wire));

        client.post("/s").json("\uDC00").send();
        assertArrayEquals(new byte[] {(byte) 0xEF, (byte) 0xBF, (byte) 0xBD}, body(transport.requests.get(1)));
        String paired = "😀";
        assertArrayEquals(paired.getBytes(StandardCharsets.UTF_8), VesperaClient.utf8(paired));
    }

    @Test
    void decodeOnlyCharsetIsRejectedWithAClearMessage() {
        java.nio.charset.Charset decodeOnly = java.nio.charset.Charset.availableCharsets().values().stream()
                .filter(cs -> !cs.canEncode()).findFirst().orElse(null);
        org.junit.jupiter.api.Assumptions.assumeTrue(decodeOnly != null, "the JDK ships a decode-only charset");
        IllegalArgumentException rejected = assertThrows(IllegalArgumentException.class,
                () -> client.post("/s").body("x", "text/plain; charset=" + decodeOnly.name()));
        assertTrue(rejected.getMessage().contains("can only decode"), rejected.getMessage());
    }

    @Test
    void appSelectionFromClientAndRequest() throws IOException {
        VesperaClient admin = client.forApp("  admin ");
        assertEquals("admin", admin.appName());
        admin.get("/dashboard").send();
        assertEquals("admin", header(transport.requests.get(0)).get("app").asText());

        admin.get("/x").app("public").send();
        assertEquals("public", header(transport.requests.get(1)).get("app").asText());

        admin.get("/x").app("  ").send();
        assertFalse(header(transport.requests.get(2)).has("app"));

        assertNull(client.forApp(null).appName());
        assertEquals("VesperaClient{app=_default}", client.toString());
    }

    @Test
    void bodyHelpersSetContentTypeAndRawBytes() throws IOException {
        client.post("/users").json("{\"name\":\"é\"}").send();
        byte[] wire = transport.requests.get(0);
        assertEquals("application/json", header(wire).get("headers").get("content-type").asText());
        assertArrayEquals("{\"name\":\"é\"}".getBytes(StandardCharsets.UTF_8), body(wire));

        byte[] raw = {0, 1, 2, (byte) 0xFF};
        client.post("/upload").body(raw, "application/octet-stream").send();
        assertArrayEquals(raw, body(transport.requests.get(1)));

        client.post("/t").body("é", "text/plain; charset=ISO-8859-1").send();
        assertArrayEquals(new byte[] {(byte) 0xE9}, body(transport.requests.get(2)));

        Map<String, Object> form = new LinkedHashMap<>();
        form.put("name", "Jane Doe");
        form.put("count", 3);
        client.patch("/form").form(form).send();
        assertEquals("name=Jane%20Doe&count=3",
                new String(body(transport.requests.get(3)), StandardCharsets.US_ASCII));
        assertEquals("application/x-www-form-urlencoded",
                header(transport.requests.get(3)).get("headers").get("content-type").asText());

        client.put("/t").text("hi").send();
        assertEquals("text/plain; charset=utf-8",
                header(transport.requests.get(4)).get("headers").get("content-type").asText());
    }

    @Test
    void remoteAddrAndSchemeTravelAsWireFields() throws IOException {
        client.get("/headers/client").remoteAddr("[::1]:4321").scheme("https").send();
        JsonNode h = header(transport.requests.get(0));
        assertEquals("[::1]:4321", h.get("remote_addr").asText());
        assertEquals("https", h.get("scheme").asText());
    }

    @Test
    void principalTravelsAsTrustedWireField() throws IOException {
        client.get("/headers/principal")
                .principal(VesperaPrincipal.of("alice", "ROLE_USER"))
                .send();
        JsonNode p = header(transport.requests.get(0)).get("principal");
        assertEquals("alice", p.get("name").asText());
        assertEquals("ROLE_USER", p.get("authorities").get(0).asText());

        client.get("/headers/principal").send();
        assertNull(header(transport.requests.get(1)).get("principal"));
    }

    @Test
    void validationErrorsAndMultiValueHeadersAreExposed() {
        transport.response = responseWire(422,
                "{\"content-type\":\"application/json\",\"set-cookie\":[\"a=1\",\"b=2\"]}",
                "[{\"path\":\"title\",\"message\":\"length is lower than 1\"}]",
                "{\"errors\":[]}");
        VesperaResponse r = client.post("/documents/register").json("{}").send();
        assertEquals(422, r.status());
        assertFalse(r.isSuccessful());
        assertTrue(r.hasValidationErrors());
        assertEquals("title", r.validationErrors().get(0).get("path"));
        assertEquals("a=1", r.header("set-cookie"));
        assertEquals(List.of("a=1", "b=2"), r.headerValues("Set-Cookie"));
        assertEquals(List.of(), r.headerValues("x-missing"));
        assertNull(r.header("x-missing"));
        assertEquals("{\"errors\":[]}", r.bodyString());
        assertTrue(r.toString().contains("status=422"));
    }

    @Test
    void errorStatusesAreResponsesNotExceptions() {
        transport.response = responseWire(503, "{}", null, "runtime shut down");
        assertEquals(503, client.get("/x").send().status());
    }

    @Test
    void transportFailuresAreVesperaClientExceptions() throws Exception {
        transport.response = new byte[] {0, 0, 0, 99, '{'};
        VesperaClientException malformed =
                assertThrows(VesperaClientException.class, () -> client.get("/x").send());
        assertTrue(malformed.getMessage().contains("malformed"));
        ExecutionException async = assertThrows(ExecutionException.class,
                () -> client.get("/x").sendAsync().get());
        assertInstanceOf(VesperaClientException.class, async.getCause());

        transport.error = new UnsatisfiedLinkError("dispatchBytes");
        assertThrows(VesperaClientException.class, () -> client.get("/x").send());
        CompletableFuture<VesperaResponse> f = client.get("/x").sendAsync();
        CompletionException c = assertThrows(CompletionException.class, f::join);
        assertInstanceOf(VesperaClientException.class, c.getCause());

        transport.error = null;
        transport.failure = new VesperaClientException("not initialized");
        assertThrows(VesperaClientException.class, () -> client.get("/x").send());
        assertTrue(client.get("/x").sendAsync().isCompletedExceptionally());
    }

    @Test
    void nativeTransportRequiresAnInitializedLibrary() {
        org.junit.jupiter.api.Assumptions.assumeFalse(VesperaBridge.isInitialized(),
                "another test in this JVM loaded a native library");
        VesperaClientException e =
                assertThrows(VesperaClientException.class, () -> VesperaClient.create().get("/x").send());
        assertTrue(e.getMessage().contains("not initialized"));
        assertTrue(VesperaClient.create().get("/x").sendAsync().isCompletedExceptionally());
        assertThrows(VesperaClientException.class, () -> VesperaClient.create().post("/x")
                .body(new ByteArrayInputStream(new byte[1]), null).send());
    }

    @Test
    void asyncDecodesTheResponse() throws Exception {
        VesperaResponse r = client.get("/x").sendAsync().get();
        assertEquals("ok", r.bodyString());
        VesperaResponse viaExecutor = client.get("/x").sendAsync(Runnable::run).get();
        assertEquals(200, viaExecutor.status());
    }

    @Test
    void cancellingTheAsyncResponseCancelsTheNativeDispatch() {
        List<CompletableFuture<byte[]>> pending = new ArrayList<>();
        VesperaClient pendingClient = new VesperaClient(new FakeTransport() {
            @Override
            public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
                CompletableFuture<byte[]> f = new CompletableFuture<>();
                pending.add(f);
                return f;
            }
        }, null);

        assertTrue(pendingClient.get("/x").sendAsync().cancel(true));
        assertTrue(pending.get(0).isCancelled(), "sendAsync() cancel reaches the dispatch future");

        CompletableFuture<VesperaResponse> viaExecutor = pendingClient.get("/x").sendAsync(Runnable::run);
        viaExecutor.completeExceptionally(new IllegalStateException("timeout"));
        assertTrue(pending.get(1).isCancelled(), "sendAsync(executor) failure reaches it too");

        CompletableFuture<VesperaResponse> done = pendingClient.get("/x").sendAsync();
        pending.get(2).complete(transport.response);
        assertEquals("ok", done.join().bodyString());
        assertFalse(pending.get(2).isCancelled(), "a normal completion never cancels");
    }

    @Test
    void streamedBodyUsesTheBidirectionalPathOnce() throws Exception {
        transport.response = responseWire(200, "{\"content-type\":\"application/octet-stream\"}", null, "");
        transport.streamedBody = "echoed".getBytes(StandardCharsets.UTF_8);
        byte[] payload = "upload-bytes".getBytes(StandardCharsets.UTF_8);
        VesperaRequest req = client.post("/echo/stream")
                .body(new ByteArrayInputStream(payload), "application/octet-stream");
        VesperaResponse r = req.send();
        assertArrayEquals(payload, transport.uploaded);
        assertEquals(0, body(transport.requests.get(0)).length, "header-only wire");
        assertEquals("application/octet-stream",
                header(transport.requests.get(0)).get("headers").get("content-type").asText());
        assertEquals("echoed", r.bodyString());
        assertThrows(IllegalStateException.class, req::send);
        assertThrows(IllegalStateException.class, req::sendAsync);

        // An error wire (nothing streamed) keeps its inline body.
        transport.streamedBody = new byte[0];
        transport.response = responseWire(413, "{}", null, "too large");
        VesperaResponse rejected = client.post("/echo/stream")
                .body(new ByteArrayInputStream(payload), null)
                .sendAsync(Runnable::run).get();
        assertEquals(413, rejected.status());
        assertEquals("too large", rejected.bodyString());
    }

    /** "Not initialised" before the body was read: the same request can be sent again. */
    @Test
    void streamedBodyRejectedBeforeReadingStaysSendable() throws Exception {
        boolean[] ready = {false};
        FakeTransport flaky = new FakeTransport() {
            @Override
            public byte[] dispatchStreaming(byte[] header, InputStream in, OutputStream out)
                    throws IOException {
                if (!ready[0]) {
                    throw new VesperaClientException("vespera native library is not initialized");
                }
                return super.dispatchStreaming(header, in, out);
            }
        };
        byte[] payload = "upload".getBytes(StandardCharsets.UTF_8);
        VesperaRequest req = new VesperaClient(flaky, null).post("/s")
                .body(new ByteArrayInputStream(payload), null);
        assertThrows(VesperaClientException.class, req::send);
        ready[0] = true;
        assertEquals(200, req.send().status(), "retried after init");
        assertArrayEquals(payload, flaky.uploaded);
        assertThrows(IllegalStateException.class, req::send, "now consumed");

        // A failure after the body was read keeps it consumed.
        VesperaRequest partial = new VesperaClient(new FakeTransport() {
            @Override
            public byte[] dispatchStreaming(byte[] header, InputStream in, OutputStream out)
                    throws IOException {
                in.read();
                throw new IllegalStateException("boom");
            }
        }, null).post("/s").body(new ByteArrayInputStream(payload), null);
        assertThrows(IllegalStateException.class, partial::send);
        IllegalStateException again = assertThrows(IllegalStateException.class, partial::send);
        assertTrue(again.getMessage().contains("already sent"), again.getMessage());
    }

    @Test
    void streamedBodyFailureAfterBytesIsATransportFailure() {
        VesperaClient failing = new VesperaClient(new FakeTransport() {
            @Override
            public byte[] dispatchStreaming(byte[] header, InputStream in, OutputStream out)
                    throws IOException {
                throw new IOException("truncated");
            }
        }, null);
        VesperaClientException e = assertThrows(VesperaClientException.class, () -> failing.post("/s")
                .body(new ByteArrayInputStream(new byte[1]), null).send());
        assertInstanceOf(IOException.class, e.getCause());
    }

    /**
     * The native side swallows an {@link IOException} from the request body
     * stream (it only ends the Rust request body with an error, and the
     * handler answers some status): the client reports it as a transport
     * failure instead of returning that unrelated response.
     */
    @Test
    void aFailingRequestBodyStreamIsATransportFailure() {
        IOException broken = new IOException("disk gone");
        VesperaClient swallowing = new VesperaClient(new FakeTransport() {
            @Override
            public byte[] dispatchStreaming(byte[] header, InputStream in, OutputStream out) {
                byte[] chunk = new byte[16];
                try {
                    while (in.read(chunk, 0, chunk.length) >= 0) {
                        // keep pulling, like the native producer
                    }
                } catch (IOException e) {
                    // cleared natively: the handler sees a body error
                }
                return responseWire(400, "{}", null, "Failed to buffer the request body");
            }
        }, null);
        InputStream failing = new InputStream() {
            private boolean first = true;

            @Override
            public int read() throws IOException {
                throw broken;
            }

            @Override
            public int read(byte[] b, int off, int len) throws IOException {
                if (first) {
                    first = false;
                    b[off] = 'x';
                    return 1;
                }
                throw broken;
            }
        };
        VesperaClientException e = assertThrows(VesperaClientException.class,
                () -> swallowing.post("/s").body(failing, null).send());
        assertSame(broken, e.getCause());

        // Same through sendAsync(executor), which runs send() there.
        InputStream failingAgain = new InputStream() {
            @Override
            public int read() throws IOException {
                throw broken;
            }
        };
        ExecutionException async = assertThrows(ExecutionException.class, () -> swallowing.post("/s")
                .body(failingAgain, null).sendAsync(Runnable::run).get());
        assertInstanceOf(VesperaClientException.class, async.getCause());
        assertSame(broken, async.getCause().getCause());
    }

    @Test
    void aParkedReadFailingBecauseTheNativeSideClosedTheSourceIsNotATransportFailure()
            throws Exception {
        CountDownLatch parked = new CountDownLatch(1);
        CountDownLatch closed = new CountDownLatch(1);
        // A socket-like source: once drained of its first byte, a read parks
        // until the stream is closed, then fails with the close.
        InputStream socketLike = new InputStream() {
            private boolean first = true;

            @Override
            public int read() throws IOException {
                byte[] one = new byte[1];
                return read(one, 0, 1) < 0 ? -1 : one[0];
            }

            @Override
            public int read(byte[] b, int off, int len) throws IOException {
                if (first) {
                    first = false;
                    b[off] = 'x';
                    return 1;
                }
                parked.countDown();
                try {
                    closed.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
                throw new IOException("Socket closed");
            }

            @Override
            public void close() {
                closed.countDown();
            }
        };
        VesperaClient closing = new VesperaClient(new FakeTransport() {
            @Override
            public byte[] dispatchStreaming(byte[] header, InputStream in, OutputStream out)
                    throws IOException {
                // The native producer: pulls on its own thread and parks in
                // the second read.
                Thread producer = new Thread(() -> {
                    byte[] chunk = new byte[16];
                    try {
                        while (in.read(chunk, 0, chunk.length) >= 0) {
                            // keep pulling
                        }
                    } catch (IOException expected) {
                        // swallowed natively
                    }
                });
                producer.start();
                try {
                    assertTrue(parked.await(10, TimeUnit.SECONDS));
                    // The handler answered and the response drained: Rust
                    // closes the request source to unblock the producer.
                    in.close();
                    producer.join(10_000);
                } catch (InterruptedException e) {
                    throw new AssertionError(e);
                }
                assertFalse(producer.isAlive());
                return responseWire(200, "{}", null, "done");
            }
        }, null);

        VesperaResponse response = closing.post("/s").body(socketLike, null).send();

        assertEquals(200, response.status());
        assertEquals("done", response.bodyString());
    }

    @Test
    void theDefaultAppNameReportsAsNull() {
        assertNull(client.forApp("_default").appName());
        assertNull(client.forApp(" _default ").appName());
        assertNull(client.get("/a").app("_default").appName());
        assertEquals("admin", client.forApp(" admin ").appName());
        assertEquals("admin", client.get("/a").app("admin").appName());
        assertEquals("VesperaClient{app=_default}", client.forApp("_default").toString());
    }

    @Test
    void charsetParsingFallsBackToUtf8() {
        assertSame(StandardCharsets.UTF_8, VesperaResponse.charsetOf(null));
        assertSame(StandardCharsets.UTF_8, VesperaResponse.charsetOf("text/plain"));
        assertSame(StandardCharsets.UTF_8, VesperaResponse.charsetOf("text/plain; charset=bogus-x"));
        assertEquals(StandardCharsets.ISO_8859_1,
                VesperaResponse.charsetOf("text/plain; Charset=\"ISO-8859-1\""));
    }

    @Test
    void convenienceMethodsUseTheirVerbs() throws IOException {
        client.head("/a").send();
        client.delete("/a").send();
        client.options("/a").send();
        assertEquals("HEAD", header(transport.requests.get(0)).get("method").asText());
        assertEquals("DELETE", header(transport.requests.get(1)).get("method").asText());
        assertEquals("OPTIONS", header(transport.requests.get(2)).get("method").asText());
        assertEquals("OPTIONS", client.options("/a").method());
        assertEquals("v", client.get("/a").header("X-K", "v").headerValue("x-k"));
        assertNull(client.get("/a").appName());
    }

    /** {@code bodyString} decodes exactly like {@code Charset.decode}, malformed input included. */
    @Test
    void bodyStringMatchesCharsetDecodeIncludingMalformedInput() {
        byte[][] bodies = {
                "plain ascii".getBytes(StandardCharsets.UTF_8),
                "héllo 😀 世界".getBytes(StandardCharsets.UTF_8),
                {(byte) 0xC3},
                {'a', (byte) 0xE2, (byte) 0x82, 'b', (byte) 0xF0, (byte) 0x9F, (byte) 0x98},
                {(byte) 0xED, (byte) 0xA0, (byte) 0x80, (byte) 0xC0, (byte) 0x80, (byte) 0xFF},
                {(byte) 0xF4, (byte) 0x90, (byte) 0x80, (byte) 0x80, 'z'},
                {},
        };
        java.nio.charset.Charset[] charsets = {
                StandardCharsets.UTF_8, StandardCharsets.ISO_8859_1, StandardCharsets.UTF_16,
                StandardCharsets.US_ASCII,
        };
        for (byte[] body : bodies) {
            for (java.nio.charset.Charset charset : charsets) {
                VesperaResponse response = new VesperaResponse(new VesperaBridge.DecodedResponse(
                        200, Map.of(), Map.of(), ByteBuffer.wrap(body), null));
                String expected = charset.decode(ByteBuffer.wrap(body).asReadOnlyBuffer()).toString();
                assertEquals(expected, response.bodyString(charset), charset + " " + java.util.Arrays.toString(body));
            }
        }
    }
}
