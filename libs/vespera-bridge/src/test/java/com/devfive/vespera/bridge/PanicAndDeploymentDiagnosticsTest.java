package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.function.BiConsumer;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * {@code metadata.panicked} detection on every response-commit path, and the
 * actionable "already loaded in another classloader" link error.
 */
class PanicAndDeploymentDiagnosticsTest {

    private static final String PANIC_HEADER = "{\"v\":1,\"status\":500,"
            + "\"headers\":{\"content-type\":\"text/plain; charset=utf-8\"},"
            + "\"metadata\":{\"version\":\"0.5.0\",\"panicked\":\"true\"}}";

    private static boolean panicked(String json) {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        HeaderPolicy.ResponseHeaderAccumulator heap = new HeaderPolicy.ResponseHeaderAccumulator();
        WireHeaderReader.apply(bytes, 0, bytes.length, heap);
        ByteBuffer direct = ByteBuffer.allocateDirect(bytes.length).put(bytes);
        HeaderPolicy.ResponseHeaderAccumulator viaDirect = new HeaderPolicy.ResponseHeaderAccumulator();
        WireHeaderReader.apply(direct, 0, bytes.length, viaDirect);
        assertEquals(heap.panicked, viaDirect.panicked);
        return heap.panicked;
    }

    @ParameterizedTest
    @CsvSource(delimiter = '|', value = {
            "{\"status\":500,\"metadata\":{\"version\":\"1\",\"panicked\":\"true\"}}|true",
            "{\"status\":500,\"metadata\":{ \"panicked\" : \"true\" , \"x\":[1,{}]}}|true",
            "{\"status\":500,\"metadata\":{\"panicked\":\"false\"}}|false",
            "{\"status\":500,\"metadata\":{\"panicked\":true}}|false",
            "{\"status\":500,\"metadata\":{\"p\\u0061nicked\":\"true\"}}|false",
            "{\"status\":500,\"metadata\":{\"panickeD\":\"true\",\"version\":\"1\"}}|false",
            "{\"status\":500,\"metadata\":{}}|false",
            "{\"status\":500,\"metadata\":null}|false",
            "{\"status\":500}|false",
    })
    void panickedFlagIsReadFromMetadataOnly(String json, boolean expected) {
        assertEquals(expected, panicked(json));
    }

    @Test
    void plainSinksSkipTheMetadataUnchanged() {
        byte[] bytes = PANIC_HEADER.getBytes(StandardCharsets.UTF_8);
        BiConsumer<String, String> plain = (name, value) -> { };
        assertEquals(500, WireHeaderReader.apply(bytes, 0, bytes.length, plain));
        assertEquals("true", VesperaBridge.decodeResponse(wire(PANIC_HEADER, "boom"))
                .metadata().get("panicked"));
    }

    private static byte[] wire(String header, String body) {
        byte[] h = header.getBytes(StandardCharsets.UTF_8);
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        ByteBuffer out = ByteBuffer.allocate(4 + h.length + b.length);
        out.putInt(h.length).put(h).put(b);
        return out.array();
    }

    @Test
    void everyCommitPathFlagsTheProxyResponse() throws Exception {
        byte[] wire = wire(PANIC_HEADER, "panic in Rust engine");

        ProxyServletResponse sync = new ProxyServletResponse(new MockHttpServletResponse(), null, null);
        WireResponseWriter.writeWireResponse(wire, sync, "GET");
        assertTrue(sync.panicked());
        assertEquals(500, sync.getStatus());

        ProxyServletResponse direct = new ProxyServletResponse(new MockHttpServletResponse(), null, null);
        ByteBuffer directWire = ByteBuffer.allocateDirect(wire.length).put(wire).flip();
        WireResponseWriter.applyDirectHeaderAndPositionBody(directWire, direct, "GET");
        assertTrue(direct.panicked());

        ProxyServletResponse streaming =
                new ProxyServletResponse(new MockHttpServletResponse(), null, null);
        WireResponseWriter.applyDecodedHeader(wire(PANIC_HEADER, ""), streaming, "GET");
        assertTrue(streaming.panicked());

        MockHttpServletResponse raw = new MockHttpServletResponse();
        ProxyServletResponse async = new ProxyServletResponse(raw, "X-Request-Id", "id-9");
        ResponseEntity<byte[]> entity = WireResponseWriter.buildResponseEntityFromWire(wire, "GET", async);
        assertTrue(async.panicked());
        assertEquals("id-9", entity.getHeaders().getFirst("X-Request-Id"));
        assertEquals("panic in Rust engine", new String(entity.getBody(), StandardCharsets.UTF_8));

        ProxyServletResponse calm = new ProxyServletResponse(raw, "X-Request-Id", "id-9");
        ResponseEntity<byte[]> rustId = WireResponseWriter.buildResponseEntityFromWire(
                wire("{\"status\":200,\"headers\":{\"x-request-id\":\"rust\"}}", "ok"), "GET", calm);
        assertFalse(calm.panicked());
        assertEquals("rust", rustId.getHeaders().getFirst("X-Request-Id"));
        assertEquals(1, rustId.getHeaders().getValuesAsList("X-Request-Id").size());
        ResponseEntity<byte[]> noId = WireResponseWriter.buildResponseEntityFromWire(
                wire("{\"status\":200,\"headers\":{}}", "ok"), "GET",
                new ProxyServletResponse(raw, "X-Request-Id", null));
        assertEquals(null, noId.getHeaders().getFirst("X-Request-Id"));
    }

    @Test
    void anotherClassLoaderLinkErrorCarriesDeploymentGuidance() {
        UnsatisfiedLinkError original = new UnsatisfiedLinkError(
                "Native Library /tmp/librust_jni_demo.so already loaded in another classloader");
        UnsatisfiedLinkError guided = assertThrows(UnsatisfiedLinkError.class,
                () -> VesperaBridge.loadNativeLibrary("rust_jni_demo",
                        name -> { throw original; }, name -> { }));
        assertTrue(guided.getMessage().startsWith(original.getMessage()));
        assertTrue(guided.getMessage().contains("shared lib"), guided.getMessage());
        assertSame(original, guided.getCause());

        UnsatisfiedLinkError other = new UnsatisfiedLinkError("no rust_jni_demo in java.library.path");
        assertSame(other, VesperaNativeLoader.withDeploymentGuidance(other, "x"));
        UnsatisfiedLinkError noMessage = new UnsatisfiedLinkError();
        assertSame(noMessage, VesperaNativeLoader.withDeploymentGuidance(noMessage, "x"));
    }
}
