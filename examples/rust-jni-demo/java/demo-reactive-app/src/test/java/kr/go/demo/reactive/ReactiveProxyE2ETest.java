package kr.go.demo.reactive;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import com.devfive.vespera.bridge.VesperaReactiveProxyHandler;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.Socket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.security.Principal;
import java.time.Duration;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.beans.factory.annotation.Value;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.context.TestConfiguration;
import org.springframework.context.ApplicationContext;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Import;
import org.springframework.web.server.WebFilter;
import reactor.core.publisher.Mono;

/**
 * The WebFlux proxy end to end: a real reactor-netty server
 * ({@code RANDOM_PORT}), the autoconfigured
 * {@link VesperaReactiveProxyHandler}, and the real {@code rust_jni_demo}
 * cdylib — on a classpath with WebFlux but no servlet API / Spring MVC.
 */
@SpringBootTest(
        webEnvironment = SpringBootTest.WebEnvironment.RANDOM_PORT,
        properties = "vespera.bridge.max-buffered-request-bytes=65536")
@Import(ReactiveProxyE2ETest.TrustedPrincipal.class)
class ReactiveProxyE2ETest {

    /**
     * Stands in for Spring Security WebFlux: authenticates
     * requests carrying {@code ?session=login} as {@code reactive-user} on the exchange
     * (server-side state, not a client header).
     */
    @TestConfiguration
    static class TrustedPrincipal {
        @Bean
        WebFilter authenticatePrincipalRoute() {
            Principal user = () -> "reactive-user";
            return (exchange, chain) -> "login".equals(exchange.getRequest().getQueryParams().getFirst("session"))
                    ? chain.filter(exchange.mutate().principal(Mono.just(user)).build())
                    : chain.filter(exchange);
        }
    }

    @Value("${local.server.port}")
    int port;

    @Autowired
    ApplicationContext context;

    private final HttpClient http = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();

    private HttpResponse<String> get(String path, String... headers) throws Exception {
        HttpRequest.Builder request = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + port + path));
        if (headers.length > 0) {
            request.headers(headers);
        }
        return http.send(request.build(), HttpResponse.BodyHandlers.ofString());
    }

    private long runs(String key) throws Exception {
        return Long.parseLong(get("/diag/runs/" + key).body());
    }

    private static void await(String what, ThrowingSupplier condition) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(15).toNanos();
        while (System.nanoTime() < deadline) {
            if (condition.getAsBoolean()) {
                return;
            }
            Thread.sleep(20);
        }
        throw new AssertionError("timed out waiting for " + what);
    }

    @FunctionalInterface
    interface ThrowingSupplier {
        boolean getAsBoolean() throws Exception;
    }

    @Test
    void servletApiAndSpringMvcAreAbsent() {
        assertThrowsClassNotFound("jakarta.servlet.http.HttpServletRequest");
        assertThrowsClassNotFound("org.springframework.web.servlet.DispatcherServlet");
        assertTrue(VesperaBridge.isInitialized(), "vespera.bridge.library-name loaded the cdylib");
        assertEquals(1, context.getBeansOfType(VesperaReactiveProxyHandler.class).size());
    }

    private static void assertThrowsClassNotFound(String name) {
        try {
            Class.forName(name);
            throw new AssertionError(name + " must not be on the WebFlux demo classpath");
        } catch (ClassNotFoundException expected) {
            // ok
        }
    }

    @Test
    void proxiesBufferedRequestsThroughDispatchAsync() throws Exception {
        HttpResponse<String> health = get("/health");
        assertEquals(200, health.statusCode());
        assertEquals("ok", health.body());
        assertTrue(health.headers().firstValue("x-request-id").isPresent());

        HttpResponse<String> echo = http.send(HttpRequest.newBuilder(
                        URI.create("http://127.0.0.1:" + port + "/echo"))
                .header("Content-Type", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString("{\"hello\":\"webflux\"}")).build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(200, echo.statusCode());
        assertEquals("{\"hello\":\"webflux\"}", echo.body());
        assertEquals("application/json", echo.headers().firstValue("content-type").orElseThrow());
    }

    @Test
    void oversizedRequestBodyIs413() throws Exception {
        HttpResponse<String> response = http.send(HttpRequest.newBuilder(
                        URI.create("http://127.0.0.1:" + port + "/echo"))
                .POST(HttpRequest.BodyPublishers.ofString("x".repeat(70_000))).build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(413, response.statusCode());
    }

    @Test
    void forwardsClientAddressPrincipalAndAppSelection() throws Exception {
        String client = get("/headers/client").body();
        assertTrue(client.startsWith("http 127.0.0.1:"), client);

        assertEquals("reactive-user [] ", get("/headers/principal?session=login").body());
        assertEquals(401, get("/headers/principal").statusCode(), "anonymous exchange: no principal forwarded");

        HttpResponse<String> admin = get("/dashboard", "X-Vespera-App", "admin");
        assertEquals(200, admin.statusCode(), admin.body());
        assertEquals(404, get("/dashboard", "X-Vespera-App", "not-exposed").statusCode());

        String names = get("/headers", "X-Vespera-App", "_default", "X-Custom", "1").body();
        assertTrue(names.contains("x-custom"), names);
        assertFalse(names.contains("x-vespera-app"), "selection header never reaches Rust: " + names);
    }

    @Test
    void webFluxEndpointsWinOverTheCatchAll() throws Exception {
        assertEquals("hello from WebFlux", get("/java/hello").body());
    }

    @Test
    void eventStreamArrivesIncrementally() throws Exception {
        HttpResponse<InputStream> response = http.send(
                HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + port + "/stream/events"))
                        .header("Accept", "text/event-stream").build(),
                HttpResponse.BodyHandlers.ofInputStream());
        assertEquals(200, response.statusCode());
        assertEquals("text/event-stream", response.headers().firstValue("content-type").orElseThrow());
        try (BufferedReader lines = new BufferedReader(
                new InputStreamReader(response.body(), StandardCharsets.UTF_8))) {
            assertEquals("data: event-0", lines.readLine());
            // Measured from the first event, not from the request: a cold runner may take
            // long to start the stream, but a buffered response would deliver all three
            // events together, while the handler spaces them 300 ms apart (~600 ms from event-0 to event-2).
            long firstEvent = System.nanoTime();
            assertEquals("", lines.readLine());
            assertEquals("data: event-1", lines.readLine());
            assertEquals("", lines.readLine());
            assertEquals("data: event-2", lines.readLine());
            long spreadMillis = Duration.ofNanos(System.nanoTime() - firstEvent).toMillis();
            assertTrue(spreadMillis >= 300, "events arrived together (" + spreadMillis + " ms apart)");
        }
    }

    private static Socket rawGet(int port, String path, String... headerLines) throws IOException {
        Socket socket = new Socket(InetAddress.getLoopbackAddress(), port);
        StringBuilder request = new StringBuilder("GET " + path + " HTTP/1.1\r\nHost: localhost\r\n");
        for (String line : headerLines) {
            request.append(line).append("\r\n");
        }
        OutputStream out = socket.getOutputStream();
        out.write(request.append("\r\n").toString().getBytes(StandardCharsets.US_ASCII));
        out.flush();
        return socket;
    }

    @Test
    void clientDisconnectCancelsTheRustHandler() throws Exception {
        String key = "reactive-cancel-" + System.nanoTime();
        try (Socket socket = rawGet(port, "/diag/sleep/" + key + "/30000")) {
            await(key + " started", () -> runs(key + ":started") == 1);
        } // the client goes away mid-dispatch
        await(key + " dropped", () -> runs(key + ":dropped") == 1);
        assertEquals(0, runs(key), "the handler never ran to completion");
    }

    @Test
    void clientDisconnectStopsAStreamedResponse() throws Exception {
        String key = "reactive-drip-" + System.nanoTime();
        try (Socket socket = rawGet(port, "/diag/drip/" + key + "/10000/20", "Accept: text/event-stream")) {
            socket.setSoTimeout(10_000);
            byte[] head = socket.getInputStream().readNBytes(12);
            assertEquals("HTTP/1.1 200", new String(head, StandardCharsets.US_ASCII));
        }
        await(key + " dropped", () -> runs(key + ":dropped") == 1);
    }
}
