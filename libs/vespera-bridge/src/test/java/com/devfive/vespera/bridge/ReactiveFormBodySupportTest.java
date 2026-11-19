package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.io.IOException;
import org.junit.jupiter.api.Test;
import org.springframework.http.MediaType;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import org.springframework.util.MultiValueMap;
import org.springframework.web.server.ServerWebExchange;
import org.springframework.web.server.ServerWebExchangeDecorator;
import reactor.core.Exceptions;
import reactor.core.publisher.Mono;

class ReactiveFormBodySupportTest {

    /** The body a filter drained, as the proxy reads it. */
    private static final byte[] DRAINED = new byte[0];

    /** A form exchange whose cached form read ({@code getFormData()}) fails with {@code failure}. */
    private static ServerWebExchange formReadFails(Throwable failure) {
        ServerWebExchange base = MockServerWebExchange.from(
                MockServerHttpRequest.post("/form")
                        .contentType(MediaType.APPLICATION_FORM_URLENCODED)
                        .body("a=1&b=2"));
        return new ServerWebExchangeDecorator(base) {
            @Override
            public Mono<MultiValueMap<String, String>> getFormData() {
                return Mono.error(failure);
            }
        };
    }

    @Test
    void aDrainedBodyWithNoCachedFormIsForwardedAsItCame() {
        // reactor-netty refuses the second read of a body no form parse cached.
        ServerWebExchange exchange =
                formReadFails(new IllegalStateException("Rejecting additional inbound receiver"));

        byte[] body = ReactiveFormBodySupport.recoverConsumedBody(exchange, DRAINED, 7).block();

        assertArrayEquals(DRAINED, body);
    }

    @Test
    void aFormReadFailureOtherThanAConsumedBodyPropagates() {
        // A read error is not "nothing to recover": forwarding the truncated body would
        // hand Rust a silently shortened form, so the failure must reach the caller.
        IOException reset = new IOException("connection reset");
        ServerWebExchange exchange = formReadFails(reset);

        RuntimeException thrown = assertThrows(
                RuntimeException.class,
                () -> ReactiveFormBodySupport.recoverConsumedBody(exchange, DRAINED, 7).block());

        assertSame(reset, Exceptions.unwrap(thrown));
    }
}
