package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import com.devfive.vespera.bridge.VesperaClient;
import com.devfive.vespera.bridge.VesperaResponse;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.util.Random;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * {@link VesperaClient} against the real {@code rust_jni_demo} cdylib —
 * programmatic in-process calls (no HTTP, no proxy controller) through the
 * SYNC, ASYNC and bidirectional-streaming paths, a named app, and a
 * {@code Validated<T>} 422 with hoisted validation errors.
 */
class VesperaClientE2ETest {

    private static VesperaClient client;

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
        client = VesperaClient.create();
    }

    @Test
    void syncGet() {
        VesperaResponse r = client.get("/health").send();
        assertEquals(200, r.status());
        assertEquals("ok", r.bodyString());
        assertTrue(r.header("Content-Type").startsWith("text/plain"), r.toString());
        assertFalse(r.metadata().isEmpty());
    }

    @Test
    void syncPostJsonEchoesRawBytes() {
        String json = "{\"name\":\"é\",\"n\":1}";
        VesperaResponse r = client.post("/echo").json(json).send();
        assertEquals(200, r.status());
        assertEquals("application/json", r.contentType());
        assertEquals(json, r.bodyString());
    }

    @Test
    void asyncGet() throws Exception {
        CompletableFuture<VesperaResponse> f = client.get("/health").sendAsync();
        VesperaResponse r = f.get(30, TimeUnit.SECONDS);
        assertEquals(200, r.status());
        assertEquals("ok", r.bodyString());

        VesperaResponse viaExecutor = client.post("/echo")
                .body(new byte[] {1, 2, 3}, "application/octet-stream")
                .sendAsync(Runnable::run)
                .get(30, TimeUnit.SECONDS);
        assertArrayEquals(new byte[] {1, 2, 3}, viaExecutor.bodyBytes());
    }

    @Test
    void validationFailureIs422WithHoistedErrors() throws Exception {
        VesperaResponse r = client.post("/documents/register")
                .json("{\"title\":\"\",\"pageCount\":0}")
                .send();
        assertEquals(422, r.status(), r.bodyString());
        assertTrue(r.hasValidationErrors(), r.toString());
        assertTrue(r.validationErrors().stream().anyMatch(e -> "title".equals(e.get("path"))),
                r.validationErrors().toString());
        assertTrue(r.bodyString().contains("\"errors\""), r.bodyString());

        VesperaResponse async = client.post("/documents/register")
                .json("{\"title\":\"\",\"pageCount\":0}")
                .sendAsync().get(30, TimeUnit.SECONDS);
        assertEquals(422, async.status());
        assertTrue(async.hasValidationErrors());

        VesperaResponse ok = client.post("/documents/register")
                .json("{\"title\":\"Policy\",\"pageCount\":3}")
                .send();
        assertEquals(200, ok.status(), ok.bodyString());
        assertFalse(ok.hasValidationErrors());
    }

    @Test
    void namedAppAndUnknownRoute() {
        VesperaResponse admin = client.forApp("admin").get("/dashboard").send();
        assertEquals(200, admin.status(), admin.bodyString());
        assertTrue(admin.bodyString().contains("\"mode\":\"admin\""), admin.bodyString());

        // The default app does not serve /dashboard: an HTTP status, not an exception.
        assertEquals(404, client.get("/dashboard").send().status());
        assertEquals(404, client.forApp("no-such-app").get("/health").send().status());
    }

    @Test
    void connectionFieldsAndHeadersReachTheHandler() {
        VesperaResponse r = client.get("/headers/client")
                .remoteAddr("[::1]:4321")
                .scheme("https")
                .send();
        assertEquals("https [::1]:4321", r.bodyString());

        VesperaResponse names = client.get("/headers")
                .header("X-B", "1")
                .header("X-A", "2")
                .query("ignored", "a b")
                .send();
        assertEquals("[\"x-a\",\"x-b\"]", names.bodyString());
    }

    @Test
    void streamedUploadRoundTrips() {
        byte[] payload = new byte[600 * 1024];
        new Random(7).nextBytes(payload);
        VesperaResponse r = client.post("/echo/stream")
                .body(new ByteArrayInputStream(payload), "application/octet-stream")
                .send();
        assertEquals(200, r.status());
        assertArrayEquals(payload, r.bodyBytes());

        VesperaResponse small = client.post("/echo")
                .body("hi", "text/plain; charset=utf-8")
                .send();
        assertEquals("hi", new String(small.bodyBytes(), StandardCharsets.UTF_8));
    }
}
