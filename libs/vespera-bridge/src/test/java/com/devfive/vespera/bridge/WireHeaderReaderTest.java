package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

/** Correctness gate for the zero-copy DIRECT-path header reader. */
class WireHeaderReaderTest {

    private record Captured(int status, List<String> headers) {}

    /** Parse {@code headerJson} from a direct buffer laid out as the wire is. */
    private static Captured run(String headerJson) {
        byte[] hb = headerJson.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buf = ByteBuffer.allocateDirect(4 + hb.length);
        buf.putInt(hb.length);
        buf.put(hb);
        int[] status = {-1};
        List<String> headers = new ArrayList<>();
        WireHeaderReader.apply(
                buf, 4, hb.length, s -> status[0] = s, (k, v) -> headers.add(k + "=" + v));
        return new Captured(status[0], headers);
    }

    @Test
    void parsesStatusAndSingleHeader() {
        Captured c =
                run(
                        "{\"v\":1,\"status\":200,\"headers\":{\"content-type\":\"text/plain\"},"
                                + "\"metadata\":{\"version\":\"0.1.0\"}}");
        assertEquals(200, c.status());
        assertEquals(List.of("content-type=text/plain"), c.headers());
    }

    @Test
    void parsesMultiValuedHeaderArray() {
        Captured c =
                run(
                        "{\"v\":1,\"status\":201,\"headers\":{\"set-cookie\":[\"a=1\",\"b=2\"],"
                                + "\"x\":\"y\"}}");
        assertEquals(201, c.status());
        assertEquals(List.of("set-cookie=a=1", "set-cookie=b=2", "x=y"), c.headers());
    }

    @Test
    void handlesEscapesAndUtf8InValues() {
        Captured c =
                run(
                        "{\"status\":200,\"headers\":{\"x-q\":\"a\\\"b\\\\c\\n\",\"x-u\":\"caf\u00e9\"}}");
        assertEquals(200, c.status());
        assertEquals(List.of("x-q=a\"b\\c\n", "x-u=caf\u00e9"), c.headers());
    }

    @Test
    void statusAbsentDefaultsTo500() {
        Captured c = run("{\"v\":1,\"headers\":{\"a\":\"b\"}}");
        assertEquals(500, c.status());
        assertEquals(List.of("a=b"), c.headers());
    }

    @Test
    void emptyHeadersAndEmptyMetadataDoNotCorruptParsing() {
        // The exact shape (empty nested object before another field) that broke
        // a prior stateful reader.
        Captured c = run("{\"v\":1,\"status\":204,\"headers\":{},\"metadata\":{}}");
        assertEquals(204, c.status());
        assertEquals(List.of(), c.headers());
    }

    @Test
    void skipsUnknownNestedAndArrayFields() {
        Captured c =
                run(
                        "{\"status\":422,\"validation_errors\":[{\"path\":\"a\",\"message\":\"m\"}],"
                                + "\"headers\":{\"content-type\":\"application/json\"}}");
        assertEquals(422, c.status());
        assertEquals(List.of("content-type=application/json"), c.headers());
    }

    @Test
    void nonObjectHeaderIsSkipped() {
        Captured c = run("{\"status\":200,\"headers\":null}");
        assertEquals(200, c.status());
        assertEquals(List.of(), c.headers());
    }

}
