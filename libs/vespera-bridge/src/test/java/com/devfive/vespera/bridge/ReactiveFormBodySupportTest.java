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
    void anIllegalStateExceptionThatIsNotTheSecondReadRejectionPropagates() {
        // Only reactor-netty's rejection means "no cached form"; any other
        // IllegalStateException is a real failure and must not forward the drained body.
        IllegalStateException boom = new IllegalStateException("boom");
        ServerWebExchange exchange = formReadFails(boom);

        RuntimeException thrown = assertThrows(
                RuntimeException.class,
                () -> ReactiveFormBodySupport.recoverConsumedBody(exchange, DRAINED, 7).block());

        assertSame(boom, Exceptions.unwrap(thrown));
    }

    /** A multipart exchange; the body read itself is not exercised by these tests. */
    private static ServerWebExchange multipartExchange() {
        return MockServerWebExchange.from(
                MockServerHttpRequest.post("/upload")
                        .contentType(MediaType.MULTIPART_FORM_DATA)
                        .body("--x--"));
    }

    @Test
    void aMultipartSecondReadRejectionIsTheConsumedBody() {
        // reactor-netty rejects the second read of a multipart body a filter drained:
        // that is the consumed-body case, answered as an empty body (the 500 comes later).
        byte[] body = ReactiveFormBodySupport.secondMultipartRead(
                multipartExchange(), new IllegalStateException("Rejecting additional inbound receiver"))
                .block();

        assertArrayEquals(VesperaWireCodec.EMPTY_BODY, body);
    }

    @Test
    void anOtherIllegalStateExceptionOnAMultipartReadPropagates() {
        // Any other IllegalStateException on a multipart request is a real read failure:
        // it must not be turned into an empty body (and so into a misleading consumed-body 500).
        IllegalStateException boom = new IllegalStateException("boom");

        RuntimeException thrown = assertThrows(
                RuntimeException.class,
                () -> ReactiveFormBodySupport.secondMultipartRead(multipartExchange(), boom).block());

        assertSame(boom, Exceptions.unwrap(thrown));
    }

    @Test
    void aSecondReadRejectionOnANonMultipartRequestPropagates() {
        // The rejection only means "consumed multipart body" for a multipart request.
        IllegalStateException rejected = new IllegalStateException("Rejecting additional inbound receiver");
        ServerWebExchange form = formReadFails(rejected);

        RuntimeException thrown = assertThrows(
                RuntimeException.class,
                () -> ReactiveFormBodySupport.secondMultipartRead(form, rejected).block());

        assertSame(rejected, Exceptions.unwrap(thrown));
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
