package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.SpringBootTest.WebEnvironment;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.boot.test.web.server.LocalServerPort;
import org.springframework.context.annotation.Bean;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestPart;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.multipart.MultipartFile;

/**
 * Multipart uploads through Tomcat + the real JNI bridge, with Spring Boot's
 * multipart autoconfiguration active (StandardServletMultipartResolver,
 * {@code max-file-size} 1 MB): the raw body reaches the Rust
 * {@code Multipart} extractor ({@code POST /upload}) intact — small (SYNC),
 * over Spring's 1 MB limit (BIDIRECTIONAL_STREAMING) and chunked — while a
 * Spring {@code MultipartFile} endpoint in the same app keeps Spring's
 * parsing and limits.
 */
@SpringBootTest(
        classes = {VesperaHttpTestApplication.class, MultipartUploadHttpTest.JavaUpload.class},
        webEnvironment = WebEnvironment.RANDOM_PORT)
class MultipartUploadHttpTest extends AbstractVesperaHttpIntegrationTest {

    private static final String BOUNDARY = "----vesperaE2E9x8y7z";

    @TestConfiguration(proxyBeanMethods = false)
    static class JavaUpload {
        @Bean
        JavaUploadController javaUploadController() {
            return new JavaUploadController();
        }
    }

    @RestController
    static class JavaUploadController {
        @PostMapping("/java/upload")
        String upload(@RequestPart("file") MultipartFile file) {
            return file.getOriginalFilename() + ":" + file.getSize();
        }
    }

    private final ObjectMapper objectMapper = new ObjectMapper();

    @LocalServerPort
    private int port;

    @Test
    void smallUploadReachesRust() throws Exception {
        assertRustSawUpload(patternedBytes(10_000), false);
    }

    @Test
    void uploadOverSpringsDefaultMaxFileSizeReachesRust() throws Exception {
        assertRustSawUpload(patternedBytes(1_500_000), false);
    }

    @Test
    void chunkedUploadReachesRust() throws Exception {
        assertRustSawUpload(patternedBytes(300_000), true);
    }

    @Test
    void springMultipartEndpointKeepsSpringHandling() throws Exception {
        HttpResponse<String> ok = post("/java/upload", multipart(patternedBytes(1234)), false);
        assertEquals(200, ok.statusCode(), ok.body());
        assertEquals("report.bin:1234", ok.body());
        HttpResponse<String> tooLarge = post("/java/upload", multipart(patternedBytes(1_500_000)), false);
        assertEquals(413, tooLarge.statusCode(), tooLarge.body());
    }

    private void assertRustSawUpload(byte[] file, boolean chunked) throws Exception {
        HttpResponse<String> response = post("/upload", multipart(file), chunked);
        assertEquals(200, response.statusCode(), response.body());
        JsonNode parts = objectMapper.readTree(response.body()).path("parts");
        assertEquals(2, parts.size(), response.body());
        assertEquals("title", parts.get(0).path("name").asText());
        assertEquals(16, parts.get(0).path("size").asLong());
        JsonNode filePart = parts.get(1);
        assertEquals("file", filePart.path("name").asText());
        assertEquals("report.bin", filePart.path("fileName").asText());
        assertEquals("application/octet-stream", filePart.path("contentType").asText());
        assertEquals(file.length, filePart.path("size").asLong());
        assertEquals(String.format("%016x", fnv1a(file)), filePart.path("fnv1a").asText());
    }

    private static byte[] multipart(byte[] file) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        out.writeBytes(("--" + BOUNDARY + "\r\n"
                + "Content-Disposition: form-data; name=\"title\"\r\n\r\n"
                + "quarterly report\r\n"
                + "--" + BOUNDARY + "\r\n"
                + "Content-Disposition: form-data; name=\"file\"; filename=\"report.bin\"\r\n"
                + "Content-Type: application/octet-stream\r\n\r\n").getBytes(StandardCharsets.US_ASCII));
        out.writeBytes(file);
        out.writeBytes(("\r\n--" + BOUNDARY + "--\r\n").getBytes(StandardCharsets.US_ASCII));
        return out.toByteArray();
    }

    private HttpResponse<String> post(String path, byte[] body, boolean chunked) throws Exception {
        HttpRequest.BodyPublisher publisher = chunked
                ? HttpRequest.BodyPublishers.ofInputStream(() -> new ByteArrayInputStream(body))
                : HttpRequest.BodyPublishers.ofByteArray(body);
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://localhost:" + port + path))
                .header("Content-Type", "multipart/form-data; boundary=" + BOUNDARY)
                .POST(publisher)
                .build();
        return HttpClient.newBuilder().version(HttpClient.Version.HTTP_1_1).build()
                .send(request, HttpResponse.BodyHandlers.ofString());
    }

    /** FNV-1a 64-bit, as computed by the Rust route. */
    private static long fnv1a(byte[] bytes) {
        long hash = 0xcbf29ce484222325L;
        for (byte b : bytes) {
            hash ^= b & 0xff;
            hash *= 0x100000001b3L;
        }
        return hash;
    }
}
