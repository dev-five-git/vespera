package com.devfive.vespera.bridge;

/**
 * Who answers CORS for proxied requests ({@code vespera.bridge.cors}).
 *
 * <p>Spring MVC answers every CORS preflight ({@code OPTIONS} with
 * {@code Origin} and {@code Access-Control-Request-Method}) itself, before any
 * controller runs — so without help a preflight never reaches the Rust router
 * (answered without any {@code Access-Control-*} header when Spring has no CORS configuration),
 * and with a Spring CORS configuration <em>and</em> a Rust CORS layer, actual
 * responses carry duplicate {@code Access-Control-Allow-Origin} headers.
 */
public enum CorsHandling {
    /**
     * Default: Rust owns CORS (e.g. a {@code tower_http::cors::CorsLayer}).
     * Preflights are routed to the proxy by a highest-precedence handler
     * mapping, and a Rust {@code Access-Control-*} header replaces one Spring
     * or a filter already set. Do not configure Spring CORS for proxied paths.
     */
    RUST,
    /**
     * Spring owns CORS ({@code WebMvcConfigurer#addCorsMappings},
     * {@code @CrossOrigin}, Spring Security {@code cors()}): preflights are
     * answered by Spring (the pre-0.6 behaviour), and a Rust
     * {@code Access-Control-*} header is dropped when Spring already set it.
     */
    SPRING
}
