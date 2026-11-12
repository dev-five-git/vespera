package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * Header values cross the wire under the Latin-1 contract (see
 * {@link WireHeaderStringSupport}): the servlet container's ISO-8859-1 view
 * of the raw header bytes travels as JSON chars {@code U+0000–U+00FF}, so a
 * UTF-8 filename survives both directions byte-for-byte.
 */
class Latin1HeaderContractTest {

    /** Raw bytes {@code résumé} as a servlet container exposes them (ISO-8859-1). */
    private static final String SERVLET_VIEW =
            new String("résumé".getBytes(StandardCharsets.UTF_8), StandardCharsets.ISO_8859_1);

    @Test
    void requestHeaderCharsTravelAsJsonCharsTheRustSideMapsBackToBytes() throws Exception {
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", "/files", "", Map.of("x-name", SERVLET_VIEW), new byte[0]);
        int headerLen = ByteBuffer.wrap(wire, 0, 4).getInt();
        JsonNode header = new ObjectMapper().readTree(new String(wire, 4, headerLen, StandardCharsets.UTF_8));

        String sent = header.get("headers").get("x-name").asText();
        assertEquals(SERVLET_VIEW, sent);
        // Rust maps every char <= U+00FF back to one byte: the original UTF-8.
        assertArrayEquals("résumé".getBytes(StandardCharsets.UTF_8), sent.getBytes(StandardCharsets.ISO_8859_1));
    }

    @Test
    void responseHeaderLatin1CharsRestoreTheRawBytesOnTheServletResponse() throws Exception {
        String disposition = "attachment; filename=\"" + SERVLET_VIEW + ".pdf\"";
        byte[] headerJson = ("{\"v\":1,\"status\":200,\"headers\":{\"content-disposition\":\""
                + disposition.replace("\"", "\\\"") + "\"},\"metadata\":{\"version\":\"1\"}}")
                .getBytes(StandardCharsets.UTF_8);
        ByteArrayOutputStream wire = new ByteArrayOutputStream();
        wire.write(ByteBuffer.allocate(4).putInt(headerJson.length).array());
        wire.write(headerJson);

        MockHttpServletResponse response = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(wire.toByteArray(), response, "GET");

        String applied = response.getHeader("content-disposition");
        assertEquals(disposition, applied);
        // The container writes header chars as ISO-8859-1 bytes on the wire.
        assertArrayEquals(
                "attachment; filename=\"résumé.pdf\"".getBytes(StandardCharsets.UTF_8),
                applied.getBytes(StandardCharsets.ISO_8859_1));
        assertEquals(disposition, VesperaBridge.decodeResponse(wire.toByteArray())
                .headers().get("content-disposition"));
    }
}
