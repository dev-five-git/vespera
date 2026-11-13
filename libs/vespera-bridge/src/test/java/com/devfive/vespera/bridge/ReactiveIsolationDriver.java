package com.devfive.vespera.bridge;

import com.devfive.vespera.bridge.ReactiveTestSupport.FakeTransport;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.Callable;
import java.util.concurrent.atomic.AtomicReference;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.ReactiveWebApplicationContextRunner;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.HttpHeaders;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import org.springframework.test.web.reactive.server.WebTestClient;
import org.springframework.web.reactive.config.EnableWebFlux;

/**
 * Runs inside {@link ReactiveNoServletClasspathTest}'s isolated class loader
 * (no servlet API, no Spring MVC): exercises the reactive proxy's buffered
 * and streaming paths and its autoconfiguration. Must not reference any
 * servlet / Spring MVC type itself.
 */
public final class ReactiveIsolationDriver implements Callable<String> {

    @Configuration(proxyBeanMethods = false)
    @EnableWebFlux
    static class WebFluxConfig {}

    @Override
    public String call() {
        FakeTransport transport = new FakeTransport();
        VesperaReactiveProxyHandler handler = ReactiveTestSupport.handler(transport,
                new ReactiveHeaderAppNameResolver("X-Vespera-App", List.of("admin")),
                new ExchangePrincipalResolver(), 1024, 1024, "X-Request-Id", List.of(event -> { }),
                CorsHandling.RUST);

        MockServerWebExchange buffered = MockServerWebExchange.from(MockServerHttpRequest.post("/x")
                .header("X-Vespera-App", "admin").header("Connection", "close").body("hi"));
        handler.handle(buffered).block(Duration.ofSeconds(5));
        require(buffered.getResponse().getStatusCode().value() == 200, "buffered status");
        require("ok".equals(buffered.getResponse().getBodyAsString().block()), "buffered body");
        require("hi".equals(transport.lastBody()), "request body");

        MockServerWebExchange streamed = MockServerWebExchange.from(MockServerHttpRequest.get("/events")
                .header(HttpHeaders.ACCEPT, "text/event-stream"));
        handler.handle(streamed).block(Duration.ofSeconds(5));
        require("data: a\n\ndata: b\n\n".equals(streamed.getResponse().getBodyAsString().block()),
                "streamed body");

        AtomicReference<String> failure = new AtomicReference<>();
        new ReactiveWebApplicationContextRunner()
                .withConfiguration(AutoConfigurations.of(VesperaBridgeReactiveAutoConfiguration.class))
                .withUserConfiguration(WebFluxConfig.class)
                .run(ctx -> {
                    if (ctx.getStartupFailure() != null) {
                        StringBuilder chain = new StringBuilder();
                        for (Throwable t = ctx.getStartupFailure(); t != null; t = t.getCause()) {
                            chain.append(" | caused by ").append(t);
                        }
                        failure.set(chain.toString());
                        return;
                    }
                    int status = ReactiveTestSupport.call(
                            WebTestClient.bindToApplicationContext(ctx).build().get().uri("/users")).status();
                    if (status != 503) {
                        failure.set("expected 503 without a native library, got " + status);
                    }
                });
        require(failure.get() == null, "autoconfiguration: " + failure.get());
        return "ok";
    }

    private static void require(boolean condition, String what) {
        if (!condition) {
            throw new AssertionError(what);
        }
    }
}
