package com.devfive.vespera.bridge;

import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.test.web.servlet.MockMvc;
import org.springframework.test.web.servlet.setup.MockMvcBuilders;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

/**
 * The ASYNC response entity is a plain {@code byte[]}: Spring must serve it
 * exactly like SYNC / DIRECT — no {@code 206} for a {@code Range} request, no
 * {@code Accept-Ranges}, and the Rust {@code Content-Type} untouched.
 */
class AsyncResponseEntityRangeTest {

    private static byte[] wire(String header, String body) {
        byte[] h = header.getBytes(StandardCharsets.UTF_8);
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        return ByteBuffer.allocate(4 + h.length + b.length).putInt(h.length).put(h).put(b).array();
    }

    @RestController
    static class WireController {
        @GetMapping("/file")
        ResponseEntity<byte[]> file() {
            return WireResponseWriter.buildResponseEntityFromWire(wire(
                    "{\"status\":200,\"headers\":{\"content-type\":\"text/csv\"}}", "a,b,c\n1,2,3\n"),
                    "GET");
        }
    }

    @Test
    void rangeRequestsGetTheFullBodyWithTheRustContentType() throws Exception {
        MockMvc mvc = MockMvcBuilders.standaloneSetup(new WireController()).build();
        MockHttpServletResponse response = mvc.perform(get("/file").header("Range", "bytes=0-1"))
                .andReturn().getResponse();
        assertEquals(200, response.getStatus());
        assertNull(response.getHeader("Accept-Ranges"));
        assertNull(response.getHeader("Content-Range"));
        assertEquals("text/csv", response.getHeader("Content-Type"));
        assertEquals(12, response.getContentLength());
        assertEquals("a,b,c\n1,2,3\n", response.getContentAsString());
    }
}
