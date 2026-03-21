package com.devfive.vespera.bridge;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import jakarta.servlet.http.HttpServletRequest;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.*;

import java.io.IOException;
import java.util.*;

/**
 * Catch-all proxy controller — auto-configured by Spring when
 * {@code com.devfive.vespera.bridge} is on the classpath.
 *
 * <p>Forwards every HTTP request to Rust via JNI and returns the
 * response verbatim (status, headers, body).
 *
 * <p>The user's Spring Boot app needs zero controller code:
 * <pre>{@code
 * @SpringBootApplication
 * public class MyApp {
 *     public static void main(String[] args) {
 *         VesperaBridge.init("my_rust_lib");
 *         SpringApplication.run(MyApp.class, args);
 *     }
 * }
 * }</pre>
 */
@RestController
public class VesperaProxyController {

    private static final Logger log = LoggerFactory.getLogger(VesperaProxyController.class);
    private final ObjectMapper mapper;

    public VesperaProxyController(ObjectMapper mapper) {
        this.mapper = mapper;
    }

    @RequestMapping("/**")
    public ResponseEntity<String> proxy(
            HttpServletRequest request,
            @RequestBody(required = false) String body
    ) throws IOException {

        ObjectNode envelope = mapper.createObjectNode();
        envelope.put("method", request.getMethod());
        envelope.put("path", request.getRequestURI());
        envelope.put("query", Objects.toString(request.getQueryString(), ""));

        ObjectNode headers = mapper.createObjectNode();
        Enumeration<String> names = request.getHeaderNames();
        while (names.hasMoreElements()) {
            String name = names.nextElement();
            headers.put(name.toLowerCase(Locale.ROOT), request.getHeader(name));
        }
        envelope.set("headers", headers);
        envelope.put("body", body != null ? body : "");

        String reqJson = mapper.writeValueAsString(envelope);
        log.debug("-> Rust  {}", reqJson);

        String respJson = VesperaBridge.dispatch(reqJson);
        log.debug("<- Rust  {}", respJson);

        JsonNode resp = mapper.readTree(respJson);
        int status = resp.path("status").asInt(500);
        String respBody = resp.path("body").asText("");

        HttpHeaders httpHeaders = new HttpHeaders();
        JsonNode respHeaders = resp.path("headers");
        if (respHeaders.isObject()) {
            Iterator<Map.Entry<String, JsonNode>> fields = respHeaders.fields();
            while (fields.hasNext()) {
                Map.Entry<String, JsonNode> entry = fields.next();
                JsonNode val = entry.getValue();
                if (val.isArray()) {
                    for (JsonNode v : val) {
                        httpHeaders.add(entry.getKey(), v.asText());
                    }
                } else {
                    httpHeaders.set(entry.getKey(), val.asText());
                }
            }
        }

        return new ResponseEntity<>(respBody, httpHeaders, HttpStatus.valueOf(status));
    }
}
