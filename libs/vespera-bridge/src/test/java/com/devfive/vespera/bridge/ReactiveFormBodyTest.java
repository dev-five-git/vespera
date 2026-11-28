package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
import java.io.IOException;
import java.net.Socket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.context.annotation.AnnotationConfigApplicationContext;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.server.reactive.ReactorHttpHandlerAdapter;
import org.springframework.util.LinkedMultiValueMap;
import org.springframework.util.MultiValueMap;
import org.springframework.web.reactive.config.EnableWebFlux;
import org.springframework.web.server.WebFilter;
import org.springframework.web.server.adapter.WebHttpHandlerBuilder;
import reactor.netty.DisposableServer;
import reactor.netty.http.server.HttpServer;

/**
 * A form body a {@code WebFilter} consumed through
 * {@code exchange.getFormData()} (what Spring Security's
 * {@code CsrfWebFilter} does) on a real reactor-netty server: the body can't
 * be read twice there, so the proxy rebuilds it from the cached form instead
 * of forwarding an empty body (see {@link ReactiveFormBodySupport}).
 */
class ReactiveFormBodyTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebFlux
    static class WebFluxConfig {}

    private final FakeTransport transport = new FakeTransport();
    private AnnotationConfigApplicationContext ctx;
    private DisposableServer server;

    @AfterEach
    void stop() {
        if (server != null) {
            server.disposeNow();
        }
        if (ctx != null) {
            ctx.close();
        }
    }

    private void start(boolean consumingFilter) {
        start(consumingFilter
                ? (exchange, chain) -> exchange.getFormData().then(chain.filter(exchange))
                : null);
    }

    private void start(WebFilter consumingFilter) {
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1 << 20, 1 << 20,
                null, List.of(), CorsHandling.RUST);
        ctx = new AnnotationConfigApplicationContext();
        ctx.register(WebFluxConfig.class);
        ctx.registerBean(VesperaReactiveHandlerMapping.class,
                () -> new VesperaReactiveHandlerMapping(handler, ProxyPathPatterns.defaults()));
        if (consumingFilter != null) {
            ctx.registerBean("csrfLike", WebFilter.class, () -> consumingFilter);
        }
        ctx.refresh();
        server = HttpServer.create().host("127.0.0.1").port(0)
                .handle(new ReactorHttpHandlerAdapter(WebHttpHandlerBuilder.applicationContext(ctx).build()))
                .bindNow();
    }

    private void post(String contentType, String body) throws Exception {
        assertEquals(200, send(contentType, body).statusCode());
    }

    private HttpResponse<String> send(String contentType, String body) throws Exception {
        return HttpClient.newHttpClient().send(
                HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + server.port() + "/form?q=1"))
                        .header("Content-Type", contentType)
                        .timeout(Duration.ofSeconds(10))
                        .POST(HttpRequest.BodyPublishers.ofString(body)).build(),
                HttpResponse.BodyHandlers.ofString());
    }

    private static final String MULTIPART = "multipart/form-data; boundary=XyZ";
    private static final String MULTIPART_BODY = "--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n"
            + "1\r\n--XyZ--\r\n";

    @Test
    void multipartConsumedByAFilterAnswers500InsteadOfAnEmptyBody() throws Exception {
        start((exchange, chain) -> exchange.getMultipartData().then(chain.filter(exchange)));
        HttpResponse<String> response = send(MULTIPART, MULTIPART_BODY);
        assertEquals(500, response.statusCode());
        assertEquals(ReactiveFormBodySupport.MULTIPART_CONSUMED_MSG, response.body());
        assertEquals(0, transport.requests.size(), "never forwarded to Rust");
    }

    @Test
    void unconsumedMultipartIsForwardedByteForByte() throws Exception {
        start(false);
        post(MULTIPART, MULTIPART_BODY);
        assertEquals(MULTIPART_BODY, transport.lastBody());
    }

    /**
     * HTTP/1.1 with neither Content-Length nor Transfer-Encoding: the framing
     * says the body is empty (RFC 9112 6.3), so the empty body is forwarded and
     * Rust answers. It is not reported as a consumed multipart body (500).
     */
    @Test
    void aBodylessHttp1MultipartPostIsForwardedEmpty() throws Exception {
        start(false);
        String raw = rawPostWithoutFraming("/form?q=1", MULTIPART);
        assertTrue(raw.startsWith("HTTP/1.1 200 "), raw);
        assertEquals(1, transport.requests.size(), "forwarded to Rust");
        assertEquals("", transport.lastBody());
    }

    /** Writes a POST with exactly these headers (no Content-Length, no Transfer-Encoding) and no body. */
    private String rawPostWithoutFraming(String target, String contentType) throws IOException {
        try (Socket socket = new Socket("127.0.0.1", server.port())) {
            socket.setSoTimeout(10_000);
            String head = "POST " + target + " HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: " + contentType
                    + "\r\nConnection: close\r\n\r\n";
            socket.getOutputStream().write(head.getBytes(StandardCharsets.US_ASCII));
            socket.getOutputStream().flush();
            return new String(socket.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
        }
    }

    @Test
    void formConsumedByAFilterIsRebuiltFromTheFormData() throws Exception {
        start(true);
        post("application/x-www-form-urlencoded", "a=1&b=x+y&c=%ED%95%9C&flag");
        // Only body parameters: the query string (?q=1) never leaks into the body.
        assertEquals("a=1&b=x+y&c=%ED%95%9C&flag", transport.lastBody());
    }

    @Test
    void rebuiltFormForwardsItsOwnContentLength() throws Exception {
        start(true);
        post("application/x-www-form-urlencoded", "a=1&b=x%20y");
        assertEquals("a=1&b=x+y", transport.lastBody());
        assertEquals("9", transport.lastHeader().get("headers").get("content-length").asText());
    }

    @Test
    void consumedFormHonoursTheDeclaredCharset() throws Exception {
        start(true);
        post("application/x-www-form-urlencoded; charset=ISO-8859-1", "name=%E9t%E9");
        assertEquals("name=%E9t%E9", transport.lastBody());
    }

    @Test
    void consumedFormWithAnUnknownCharsetFallsBackToUtf8() throws Exception {
        start(true);
        post("application/x-www-form-urlencoded; charset=x-vespera-no-such", "a=1&c=%ED%95%9C");
        assertEquals("a=1&c=%ED%95%9C", transport.lastBody());
    }

    @Test
    void unconsumedFormIsForwardedByteForByte() throws Exception {
        start(false);
        // Interleaving and the original percent-encoding are kept when nothing consumed the body.
        post("application/x-www-form-urlencoded", "a=1&b=x%20y&a=2");
        assertEquals("a=1&b=x%20y&a=2", transport.lastBody());
    }

    /**
     * A filter drains a form body without caching its form data, so nothing can
     * rebuild it. The body was consumed server-side: the proxy answers 500 with
     * a WARN, never a 400 that blames the client's short body, and never
     * forwards the empty body to Rust.
     */
    @Test
    void formDrainedByAFilterWithoutFormDataAnswers500NotA400() throws Exception {
        start((exchange, chain) -> exchange.getRequest().getBody().then(chain.filter(exchange)));
        HttpResponse<String> response = send("application/x-www-form-urlencoded", "a=1&b=2");
        assertEquals(500, response.statusCode());
        assertEquals(ReactiveFormBodySupport.FORM_CONSUMED_MSG, response.body());
        assertEquals(0, transport.requests.size(), "never forwarded to Rust");
    }

    @Test
    void otherBodiesAreNeverRebuilt() throws Exception {
        start(true);
        post("application/json", "{\"a\":1}");
        assertEquals("{\"a\":1}", transport.lastBody());
    }

    @Test
    void encodeGroupsValuesPerNameAndKeepsValuelessNames() {
        MultiValueMap<String, String> form = new LinkedMultiValueMap<>();
        form.add("a", "1");
        form.add("b", "x y");
        form.add("a", "3");
        form.add("flag", null);
        form.add("k&=", "v&=");
        assertEquals("a=1&a=3&b=x+y&flag&k%26%3D=v%26%3D",
                new String(ReactiveFormBodySupport.encode(form, StandardCharsets.UTF_8), StandardCharsets.US_ASCII));
    }
}
