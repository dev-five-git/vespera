package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.Filter;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.lang.reflect.Field;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import java.util.Random;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.EnumSource;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.SpringBootConfiguration;
import org.springframework.boot.autoconfigure.EnableAutoConfiguration;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.springframework.context.ConfigurableApplicationContext;
import org.springframework.context.annotation.Bean;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestPart;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.multipart.MultipartFile;
import org.springframework.web.multipart.MultipartResolver;
import org.springframework.web.multipart.support.StandardServletMultipartResolver;

/**
 * Multipart uploads through the servlet proxy on a real Tomcat with Spring
 * Boot's multipart autoconfiguration (StandardServletMultipartResolver,
 * {@code max-file-size} 1 MB): the raw body and boundary reach the proxy's
 * dispatch for every mode, Spring controllers keep normal multipart handling,
 * and a body consumed before the proxy is answered {@code 500}, never
 * forwarded empty.
 */
class MultipartPassthroughTest {

    private static final String BOUNDARY = "----vesperaBoundary7MA4YWxkTrZu0gW";
    private static final String CONTENT_TYPE = "multipart/form-data; boundary=" + BOUNDARY;

    /** Picks the dispatch mode from {@code x-test-mode}. */
    static DispatchModeResolver headerModes() {
        return request -> DispatchMode.valueOf(request.getHeader("x-test-mode"));
    }

    @RestController
    static class JavaUploadController {
        @PostMapping("/java/upload")
        String upload(@RequestPart("file") MultipartFile file) {
            return file.getOriginalFilename() + ":" + file.getSize();
        }
    }

    /**
     * Stands in for the native dispatch: runs the body acquisition
     * {@code VesperaProxyController#dispatch} hands to the native call for
     * the requested mode and echoes what Rust would receive.
     */
    static class CapturingProxy extends VesperaProxyController {
        CapturingProxy() {
            super(request -> AppNameResolver.DEFAULT_APP, headerModes());
        }

        @Override
        public Object proxy(HttpServletRequest request, HttpServletResponse response) throws IOException {
            RequestShape shape = RequestShape.capture(request);
            DispatchMode mode = DispatchMode.valueOf(request.getHeader("x-test-mode"));
            byte[] body = mode == DispatchMode.BIDIRECTIONAL_STREAMING
                    ? streamingRequestBody(request, shape).readAllBytes()
                    : readBody(request, shape, DEFAULT_MAX_BUFFERED_REQUEST_BYTES);
            Map<String, String> headers = HeaderPolicy.collectHeaders(request);
            String contentLength = forwardedContentLength(shape, body);
            response.setStatus(200);
            response.setHeader("x-seen-content-type", headers.get("content-type"));
            response.setHeader("x-seen-content-length",
                    contentLength != null ? contentLength : String.valueOf(headers.get("content-length")));
            response.setHeader("x-seen-sha256", sha256(body));
            response.setHeader("x-seen-length", Integer.toString(body.length));
            return null;
        }
    }

    @SpringBootConfiguration
    @EnableAutoConfiguration
    static class CaptureApp {
        @Bean
        VesperaProxyController capturingProxy() {
            return new CapturingProxy();
        }

        @Bean
        JavaUploadController javaUploadController() {
            return new JavaUploadController();
        }
    }

    /** The real proxy controller; {@code x-test-consume} makes a filter call getParts() first. */
    @SpringBootConfiguration
    @EnableAutoConfiguration
    static class RealApp {
        @Bean
        DispatchModeResolver testModes() {
            return headerModes();
        }

        @Bean
        FilterRegistrationBean<Filter> partsConsumingFilter() {
            FilterRegistrationBean<Filter> registration = new FilterRegistrationBean<>((request, response, chain) -> {
                if (((HttpServletRequest) request).getHeader("x-test-consume") != null) {
                    ((HttpServletRequest) request).getParts();
                }
                chain.doFilter(request, response);
            });
            registration.addUrlPatterns("/*");
            return registration;
        }
    }

    private static ConfigurableApplicationContext start(Class<?> app, String... properties) {
        String[] args = new String[properties.length + 1];
        args[0] = "--server.port=0";
        System.arraycopy(properties, 0, args, 1, properties.length);
        return new SpringApplication(app).run(args);
    }

    static byte[] multipartBody(String fileName, byte[] content) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        String head = "--" + BOUNDARY + "\r\n"
                + "Content-Disposition: form-data; name=\"title\"\r\n\r\n"
                + "quarterly report\r\n"
                + "--" + BOUNDARY + "\r\n"
                + "Content-Disposition: form-data; name=\"file\"; filename=\"" + fileName + "\"\r\n"
                + "Content-Type: application/octet-stream\r\n\r\n";
        out.writeBytes(head.getBytes(StandardCharsets.US_ASCII));
        out.writeBytes(content);
        out.writeBytes(("\r\n--" + BOUNDARY + "--\r\n").getBytes(StandardCharsets.US_ASCII));
        return out.toByteArray();
    }

    static byte[] randomBytes(int size) {
        byte[] bytes = new byte[size];
        new Random(size).nextBytes(bytes);
        return bytes;
    }

    static String sha256(byte[] body) {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(body));
        } catch (java.security.NoSuchAlgorithmException impossible) {
            throw new IllegalStateException(impossible);
        }
    }

    private static HttpResponse<String> post(ConfigurableApplicationContext ctx, String path, byte[] body,
            boolean chunked, String... headers) throws Exception {
        String port = ctx.getEnvironment().getProperty("local.server.port");
        HttpRequest.BodyPublisher publisher = chunked
                ? HttpRequest.BodyPublishers.ofInputStream(() -> new ByteArrayInputStream(body))
                : HttpRequest.BodyPublishers.ofByteArray(body);
        HttpRequest.Builder builder = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + port + path))
                .header("Content-Type", CONTENT_TYPE)
                .POST(publisher);
        for (int i = 0; i < headers.length; i += 2) {
            builder.header(headers[i], headers[i + 1]);
        }
        return HttpClient.newBuilder().version(HttpClient.Version.HTTP_1_1).build()
                .send(builder.build(), HttpResponse.BodyHandlers.ofString());
    }

    private static void assertForwardedVerbatim(HttpResponse<String> response, byte[] body, boolean chunked) {
        assertEquals(200, response.statusCode(), response.body());
        assertEquals(CONTENT_TYPE, response.headers().firstValue("x-seen-content-type").orElse(null));
        assertEquals(Integer.toString(body.length), response.headers().firstValue("x-seen-length").orElse(null));
        assertEquals(sha256(body), response.headers().firstValue("x-seen-sha256").orElse(null));
        assertEquals(chunked ? "null" : Integer.toString(body.length),
                response.headers().firstValue("x-seen-content-length").orElse(null));
    }

    @ParameterizedTest
    @EnumSource(DispatchMode.class)
    void proxiedMultipartBodyReachesEveryDispatchModeVerbatim(DispatchMode mode) throws Exception {
        byte[] small = multipartBody("small.bin", randomBytes(4096));
        // Over Spring Boot's default spring.servlet.multipart.max-file-size (1 MB).
        byte[] large = multipartBody("large.bin", randomBytes(1_572_864));
        try (ConfigurableApplicationContext ctx = start(CaptureApp.class)) {
            assertForwardedVerbatim(post(ctx, "/rust/upload", small, false, "x-test-mode", mode.name()), small, false);
            assertForwardedVerbatim(post(ctx, "/rust/upload", large, false, "x-test-mode", mode.name()), large, false);
            if (mode == DispatchMode.BIDIRECTIONAL_STREAMING) {
                assertForwardedVerbatim(post(ctx, "/rust/upload", large, true, "x-test-mode", mode.name()), large, true);
            }
        }
    }

    @Test
    void springControllersKeepNormalMultipartHandlingAndLimits() throws Exception {
        try (ConfigurableApplicationContext ctx = start(CaptureApp.class)) {
            MultipartResolver resolver = ctx.getBean("multipartResolver", MultipartResolver.class);
            VesperaProxyMultipartResolver wrapped = assertInstanceOf(VesperaProxyMultipartResolver.class, resolver);
            assertInstanceOf(StandardServletMultipartResolver.class, wrapped.delegate());

            HttpResponse<String> ok = post(ctx, "/java/upload", multipartBody("a.txt", randomBytes(1000)), false);
            assertEquals(200, ok.statusCode(), ok.body());
            assertEquals("a.txt:1000", ok.body());
            // spring.servlet.multipart.max-file-size still applies to Spring's own endpoints.
            HttpResponse<String> tooLarge =
                    post(ctx, "/java/upload", multipartBody("b.bin", randomBytes(1_572_864)), false);
            assertEquals(413, tooLarge.statusCode(), tooLarge.body());
        }
    }

    @Test
    void passthroughCanBeDisabled() {
        try (ConfigurableApplicationContext ctx = start(CaptureApp.class,
                "--vespera.bridge.multipart-passthrough=false")) {
            assertInstanceOf(StandardServletMultipartResolver.class,
                    ctx.getBean("multipartResolver", MultipartResolver.class));
        }
    }

    /**
     * The real proxy (native library marked loaded, no cdylib): an intact body
     * reaches the native call ({@code 503}: unlinked), a consumed one is
     * rejected first with a {@code 500}.
     */
    @ParameterizedTest
    @EnumSource(value = DispatchMode.class, names = {"SYNC", "DIRECT", "STREAMING", "BIDIRECTIONAL_STREAMING"})
    void consumedMultipartBodyIsRejectedInsteadOfForwardedEmpty(DispatchMode mode) throws Exception {
        byte[] body = multipartBody("small.bin", randomBytes(2048));
        Field loaded = VesperaBridge.class.getDeclaredField("loaded");
        loaded.setAccessible(true);
        boolean previous = loaded.getBoolean(null);
        try (ConfigurableApplicationContext ctx = start(RealApp.class)) {
            loaded.setBoolean(null, true);
            HttpResponse<String> intact = post(ctx, "/rust/upload", body, false, "x-test-mode", mode.name());
            assertEquals(503, intact.statusCode(), intact.body());

            HttpResponse<String> consumed = post(ctx, "/rust/upload", body, false,
                    "x-test-mode", mode.name(), "x-test-consume", "1");
            assertEquals(500, consumed.statusCode(), consumed.body());
            assertEquals(MultipartBodySupport.CONSUMED_MSG, consumed.body());
        } finally {
            loaded.setBoolean(null, previous);
        }
    }

    @Test
    void withoutPassthroughSpringConsumesTheBodyAndTheProxyFailsLoudly() throws Exception {
        byte[] body = multipartBody("small.bin", randomBytes(2048));
        Field loaded = VesperaBridge.class.getDeclaredField("loaded");
        loaded.setAccessible(true);
        boolean previous = loaded.getBoolean(null);
        try (ConfigurableApplicationContext ctx = start(RealApp.class,
                "--vespera.bridge.multipart-passthrough=false")) {
            loaded.setBoolean(null, true);
            for (DispatchMode mode : List.of(DispatchMode.SYNC, DispatchMode.BIDIRECTIONAL_STREAMING)) {
                HttpResponse<String> response = post(ctx, "/rust/upload", body, false, "x-test-mode", mode.name());
                assertEquals(500, response.statusCode(), response.body());
                assertEquals(MultipartBodySupport.CONSUMED_MSG, response.body());
            }
        } finally {
            loaded.setBoolean(null, previous);
        }
    }

    // ── unit level ──────────────────────────────────────────────────────

    @Test
    void decoratorDelegatesUnlessTheRequestIsProxied() {
        StandardServletMultipartResolver delegate = new StandardServletMultipartResolver();
        org.springframework.mock.web.MockHttpServletRequest request =
                new org.springframework.mock.web.MockHttpServletRequest("POST", "/x");
        request.setContentType(CONTENT_TYPE);
        assertTrue(new VesperaProxyMultipartResolver(delegate, r -> false).isMultipart(request));
        assertFalse(new VesperaProxyMultipartResolver(delegate, r -> true).isMultipart(request));
        request.setContentType("application/json");
        assertFalse(new VesperaProxyMultipartResolver(delegate, r -> false).isMultipart(request));

        VesperaProxyMultipartResolver.Installer installer = new VesperaProxyMultipartResolver.Installer();
        installer.setBeanFactory(new org.springframework.beans.factory.support.DefaultListableBeanFactory());
        Object wrapped = installer.postProcessAfterInitialization(delegate, "multipartResolver");
        assertInstanceOf(VesperaProxyMultipartResolver.class, wrapped);
        assertEquals(wrapped, installer.postProcessAfterInitialization(wrapped, "multipartResolver"));
        assertEquals(delegate, installer.postProcessAfterInitialization(delegate, "otherResolver"));
        // No VesperaProxyPathMappings bean (no proxy controller): never proxied.
        request.setContentType(CONTENT_TYPE);
        assertTrue(((MultipartResolver) wrapped).isMultipart(request));
    }

    @Test
    void consumedDetectionIgnoresIntactAndNonMultipartBodies() throws Exception {
        org.springframework.mock.web.MockHttpServletRequest request =
                new org.springframework.mock.web.MockHttpServletRequest("POST", "/x");
        request.setContentType(" Multipart/form-data; boundary=x");
        request.setContent("--x--\r\n".getBytes(StandardCharsets.US_ASCII));
        RequestShape shape = RequestShape.capture(request);
        assertTrue(MultipartBodySupport.isMultipart(request));
        // Intact: the stream is not finished and the buffered read is complete.
        MultipartBodySupport.rejectIfConsumed(request, shape, request.getInputStream());
        byte[] body = request.getInputStream().readAllBytes();
        MultipartBodySupport.rejectIfConsumed(request, shape, body);
        // Drained: both checks reject.
        org.junit.jupiter.api.Assertions.assertThrows(org.springframework.web.server.ResponseStatusException.class,
                () -> MultipartBodySupport.rejectIfConsumed(request, shape, request.getInputStream()));
        org.junit.jupiter.api.Assertions.assertThrows(org.springframework.web.server.ResponseStatusException.class,
                () -> MultipartBodySupport.rejectIfConsumed(request, shape, new byte[0]));
        org.junit.jupiter.api.Assertions.assertThrows(org.springframework.web.server.ResponseStatusException.class,
                () -> MultipartBodySupport.rejectIfConsumed(request, shape, new byte[3]));
        // A plain InputStream (not a ServletInputStream) cannot be checked.
        MultipartBodySupport.rejectIfConsumed(request, shape, new ByteArrayInputStream(new byte[0]));

        org.springframework.mock.web.MockHttpServletRequest json =
                new org.springframework.mock.web.MockHttpServletRequest("POST", "/x");
        json.setContentType("application/json");
        json.setContent(new byte[] {'{', '}'});
        MultipartBodySupport.rejectIfConsumed(json, RequestShape.capture(json), new byte[0]);

        org.springframework.mock.web.MockHttpServletRequest empty =
                new org.springframework.mock.web.MockHttpServletRequest("POST", "/x");
        empty.setContentType(CONTENT_TYPE);
        empty.setContent(new byte[0]);
        MultipartBodySupport.rejectIfConsumed(empty, RequestShape.capture(empty), new byte[0]);
    }
}
