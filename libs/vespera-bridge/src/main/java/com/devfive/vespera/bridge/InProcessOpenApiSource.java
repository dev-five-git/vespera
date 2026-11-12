package com.devfive.vespera.bridge;

import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.Objects;
import java.util.function.BooleanSupplier;
import java.util.function.Function;
import java.util.function.UnaryOperator;

/** {@link VesperaOpenApiSource#inProcess(Function)}: a native {@code GET} of the spec route. */
final class InProcessOpenApiSource implements VesperaOpenApiSource {

    /** Error bodies quoted in exception messages are cut to this many characters. */
    private static final int MAX_ERROR_BODY_CHARS = 200;

    private final Function<String, String> specPathFor;
    private final BooleanSupplier ready;
    private final UnaryOperator<byte[]> dispatch;

    InProcessOpenApiSource(Function<String, String> specPathFor, BooleanSupplier ready,
                           UnaryOperator<byte[]> dispatch) {
        this.specPathFor = Objects.requireNonNull(specPathFor, "specPathFor");
        this.ready = Objects.requireNonNull(ready, "ready");
        this.dispatch = Objects.requireNonNull(dispatch, "dispatch");
    }

    @Override
    public byte[] openApiJson(String app) {
        if (!ready.getAsBoolean()) {
            return null;
        }
        String path = specPathFor.apply(app);
        String target = app == null || AppNameResolver.DEFAULT_APP.equals(app) ? null : app;
        byte[] wire = VesperaBridge.encodeRequest(target, "GET", path, null,
                Map.of("accept", "application/json"), null);
        VesperaBridge.DecodedResponse response = VesperaBridge.decodeResponse(dispatch.apply(wire));
        if (response.status() != 200) {
            String body = new String(response.bodyBytes(), StandardCharsets.UTF_8);
            if (body.length() > MAX_ERROR_BODY_CHARS) {
                body = body.substring(0, MAX_ERROR_BODY_CHARS) + "...";
            }
            throw new IllegalStateException("GET " + path + " on vespera app '" + app + "' answered "
                    + response.status() + (body.isBlank() ? "" : ": " + body)
                    + (response.status() == 404
                            ? " — serve the spec with vespera!(spec_url = \"" + path + "\", ...) or set "
                                    + "vespera.bridge.springdoc.spec-path"
                            : ""));
        }
        return response.bodyBytes();
    }
}
