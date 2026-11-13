package com.devfive.vespera.bridge;

import static com.devfive.vespera.bridge.ReactiveTestSupport.responseWire;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.Socket;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.springframework.context.annotation.AnnotationConfigApplicationContext;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.server.reactive.ReactorHttpHandlerAdapter;
import org.springframework.web.reactive.config.EnableWebFlux;
import org.springframework.web.server.adapter.WebHttpHandlerBuilder;
import reactor.netty.DisposableServer;
import reactor.netty.http.server.HttpServer;

/**
 * The reactive proxy on a real reactor-netty server (fake native transport):
 * socket-level facts a mock exchange cannot show — the client address, HEAD
 * framing, chunked event streams, and cancel-on-disconnect when the client
 * closes its connection mid-dispatch.
 */
class ReactiveNettyServerTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebFlux
    static class WebFluxConfig {}

    private final FakeTransport transport = new FakeTransport();
    private final List<DispatchEvent> events = new CopyOnWriteArrayList<>();
    private AnnotationConfigApplicationContext ctx;
    private DisposableServer server;
    private final HttpClient http = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();

    @BeforeEach
    void start() {
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1 << 20, 1 << 20,
                "X-Request-Id", List.of(events::add), CorsHandling.RUST);
        ctx = new AnnotationConfigApplicationContext();
        ctx.register(WebFluxConfig.class);
        ctx.registerBean(VesperaReactiveHandlerMapping.class,
                () -> new VesperaReactiveHandlerMapping(handler, ProxyPathPatterns.defaults()));
        ctx.refresh();
        ReactorHttpHandlerAdapter adapter =
                new ReactorHttpHandlerAdapter(WebHttpHandlerBuilder.applicationContext(ctx).build());
        server = HttpServer.create().host("127.0.0.1").port(0).handle(adapter).bindNow();
    }

    @AfterEach
    void stop() {
        server.disposeNow();
        ctx.close();
    }

    private URI uri(String path) {
        return URI.create("http://127.0.0.1:" + server.port() + path);
    }

    @Test
    void forwardsTheClientAddressAndWritesTheResponse() throws Exception {
        HttpResponse<String> response = http.send(
                HttpRequest.newBuilder(uri("/hello?x=1")).POST(HttpRequest.BodyPublishers.ofString("body"))
                        .build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(200, response.statusCode());
        assertEquals("ok", response.body());
        assertEquals("2", response.headers().firstValue("content-length").orElseThrow());
        String remote = transport.lastHeader().get("remote_addr").asText();
        assertTrue(remote.startsWith("127.0.0.1:"), remote);
        assertEquals("http", transport.lastHeader().get("scheme").asText());
        assertEquals("body", transport.lastBody());
    }

    @Test
    void headKeepsTheDeclaredContentLength() throws Exception {
        transport.response = responseWire(200, "{\"content-length\":\"42\"}", "");
        HttpResponse<String> response = http.send(
                HttpRequest.newBuilder(uri("/doc")).method("HEAD", HttpRequest.BodyPublishers.noBody()).build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(200, response.statusCode());
        assertEquals("42", response.headers().firstValue("content-length").orElseThrow());
        assertEquals("", response.body());
    }

    @Test
    void streamsAnEventStream() throws Exception {
        transport.streamChunks = List.of("data: 1\n\n", "data: 2\n\n", "data: 3\n\n");
        HttpResponse<String> response = http.send(
                HttpRequest.newBuilder(uri("/events")).header("Accept", "text/event-stream").build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(200, response.statusCode());
        assertEquals("text/event-stream", response.headers().firstValue("content-type").orElseThrow());
        assertEquals("data: 1\n\ndata: 2\n\ndata: 3\n\n", response.body());
    }

    @Test
    void clientDisconnectCancelsTheNativeDispatch() throws Exception {
        CompletableFuture<byte[]> nativeFuture = new CompletableFuture<>();
        transport.pending = nativeFuture;
        try (Socket socket = new Socket(InetAddress.getLoopbackAddress(), server.port())) {
            OutputStream out = socket.getOutputStream();
            out.write("GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n".getBytes(StandardCharsets.US_ASCII));
            out.flush();
            long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
            while (transport.requests.isEmpty() && System.nanoTime() < deadline) {
                Thread.sleep(5);
            }
            assertEquals(1, transport.requests.size(), "dispatched");
            assertTrue(!nativeFuture.isDone());
        } // client goes away
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        while (!nativeFuture.isCancelled() && System.nanoTime() < deadline) {
            Thread.sleep(5);
        }
        assertTrue(nativeFuture.isCancelled(), "disconnect cancels the native future");
        while (events.isEmpty() && System.nanoTime() < deadline) {
            Thread.sleep(5);
        }
        assertEquals(VesperaReactiveProxyHandler.CLIENT_CLOSED_REQUEST, events.get(0).status());
    }

    @Test
    void completedResponseAfterADisconnectIsHarmless() throws Exception {
        CompletableFuture<byte[]> nativeFuture = new CompletableFuture<>();
        transport.pending = nativeFuture;
        try (Socket socket = new Socket(InetAddress.getLoopbackAddress(), server.port())) {
            socket.getOutputStream().write(
                    "GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n".getBytes(StandardCharsets.US_ASCII));
            long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
            while (transport.requests.isEmpty() && System.nanoTime() < deadline) {
                Thread.sleep(5);
            }
            nativeFuture.complete(responseWire(200, "{}", "late"));
            InputStream in = socket.getInputStream();
            socket.setSoTimeout(5000);
            byte[] head = in.readNBytes(12);
            assertEquals("HTTP/1.1 200", new String(head, StandardCharsets.US_ASCII));
        }
    }
}
