package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
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
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App"), null, 1 << 20, 1 << 20,
                null, List.of(), CorsHandling.RUST);
        ctx = new AnnotationConfigApplicationContext();
        ctx.register(WebFluxConfig.class);
        ctx.registerBean(VesperaReactiveHandlerMapping.class,
                () -> new VesperaReactiveHandlerMapping(handler, ProxyPathPatterns.defaults()));
        if (consumingFilter) {
            ctx.registerBean("csrfLike", WebFilter.class,
                    () -> (exchange, chain) -> exchange.getFormData().then(chain.filter(exchange)));
        }
        ctx.refresh();
        server = HttpServer.create().host("127.0.0.1").port(0)
                .handle(new ReactorHttpHandlerAdapter(WebHttpHandlerBuilder.applicationContext(ctx).build()))
                .bindNow();
    }

    private void post(String contentType, String body) throws Exception {
        HttpResponse<String> response = HttpClient.newHttpClient().send(
                HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + server.port() + "/form?q=1"))
                        .header("Content-Type", contentType)
                        .timeout(Duration.ofSeconds(10))
                        .POST(HttpRequest.BodyPublishers.ofString(body)).build(),
                HttpResponse.BodyHandlers.ofString());
        assertEquals(200, response.statusCode());
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
    void unconsumedFormIsForwardedByteForByte() throws Exception {
        start(false);
        // Interleaving and the original percent-encoding are kept when nothing consumed the body.
        post("application/x-www-form-urlencoded", "a=1&b=x%20y&a=2");
        assertEquals("a=1&b=x%20y&a=2", transport.lastBody());
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
