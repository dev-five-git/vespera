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
import java.util.concurrent.ExecutionException;
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
        assertThrows(NullPointerException.class, () -> client.get("/").header("x", null));
        assertThrows(NullPointerException.class, () -> client.get(null));
        assertTrue(transport.requests.isEmpty());
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
        if (VesperaBridge.isInitialized()) {
            return;
        }
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
}
