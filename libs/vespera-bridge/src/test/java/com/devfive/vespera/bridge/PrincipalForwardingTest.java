package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.nio.charset.StandardCharsets;
import java.security.Principal;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.mock.web.MockHttpServletRequest;

/**
 * The trusted {@code principal} wire field: {@link VesperaPrincipal}
 * validation, its {@link VesperaWireCodec} encoding, the default
 * {@link ServletPrincipalResolver} and the autoconfigured
 * {@link PrincipalResolver} bean.
 */
class PrincipalForwardingTest {

    // ── fake Spring Security types (the bridge reads them reflectively) ──

    public interface FakeGrantedAuthority {
        String getAuthority();
    }

    public interface FakeAuthentication extends Principal {
        Object getAuthorities();
    }

    /** No {@code getAuthorities()}: disables authority lookup. */
    public interface NotAnAuthentication {}

    record Auth(String name, Object authorities) implements FakeAuthentication {
        @Override
        public String getName() {
            return name;
        }

        @Override
        public Object getAuthorities() {
            return authorities;
        }
    }

    private static FakeGrantedAuthority granted(String authority) {
        return () -> authority;
    }

    private static final ServletPrincipalResolver SECURITY_AWARE =
            new ServletPrincipalResolver(FakeAuthentication.class, FakeGrantedAuthority.class);

    private static MockHttpServletRequest requestAs(Principal principal) {
        MockHttpServletRequest request = new MockHttpServletRequest("GET", "/me");
        request.setUserPrincipal(principal);
        return request;
    }

    // ── VesperaPrincipal ──

    @Test
    void principalCopiesItsComponentsAndRejectsNulls() {
        List<String> authorities = new ArrayList<>(List.of("A"));
        Map<String, String> attributes = new LinkedHashMap<>();
        attributes.put("z", "1");
        attributes.put("a", "2");
        VesperaPrincipal principal = new VesperaPrincipal("alice", authorities, attributes);
        authorities.add("B");
        attributes.clear();
        assertEquals(List.of("A"), principal.authorities());
        assertEquals(List.of("z", "a"), new ArrayList<>(principal.attributes().keySet()));
        assertThrows(UnsupportedOperationException.class, () -> principal.attributes().put("x", "y"));

        VesperaPrincipal bare = new VesperaPrincipal("bob", null, null);
        assertEquals(List.of(), bare.authorities());
        assertEquals(Map.of(), bare.attributes());
        assertEquals(bare, VesperaPrincipal.of("bob"));

        assertThrows(NullPointerException.class, () -> VesperaPrincipal.of(null));
        assertThrows(NullPointerException.class,
                () -> new VesperaPrincipal("a", Arrays.asList("x", null), null));
        Map<String, String> nullKey = new LinkedHashMap<>();
        nullKey.put(null, "v");
        assertThrows(NullPointerException.class, () -> new VesperaPrincipal("a", null, nullKey));
        Map<String, String> nullValue = new LinkedHashMap<>();
        nullValue.put("k", null);
        assertThrows(NullPointerException.class, () -> new VesperaPrincipal("a", null, nullValue));
    }

    // ── codec ──

    private static String headerJson(VesperaPrincipal principal) {
        VesperaBridge.HeaderSource source = new VesperaBridge.HeaderSource() {
            @Override
            public void writeTo(VesperaBridge.HeaderSink sink) {
                sink.put("accept", "*/*");
            }

            @Override
            public VesperaPrincipal principal() {
                return principal;
            }
        };
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/me", null, source, null);
        int len = VesperaWireCodec.readHeaderLength(wire);
        return new String(wire, 4, len, StandardCharsets.UTF_8);
    }

    @Test
    void codecEncodesTheFullPrincipalAsValidJson() throws Exception {
        Map<String, String> attributes = new LinkedHashMap<>();
        attributes.put("tenant", "acme");
        attributes.put("quote\"key", "line\nbreak");
        String json = headerJson(new VesperaPrincipal(
                "al\"ice", List.of("ROLE_USER", "ROLE_ADMIN"), attributes));
        assertTrue(json.endsWith(",\"principal\":{\"name\":\"al\\\"ice\","
                + "\"authorities\":[\"ROLE_USER\",\"ROLE_ADMIN\"],"
                + "\"attributes\":{\"tenant\":\"acme\",\"quote\\\"key\":\"line\\nbreak\"}}}"), json);

        JsonNode principal = new ObjectMapper().readTree(json).path("principal");
        assertEquals("al\"ice", principal.path("name").asText());
        assertEquals("ROLE_ADMIN", principal.path("authorities").path(1).asText());
        assertEquals("line\nbreak", principal.path("attributes").path("quote\"key").asText());
    }

    @Test
    void codecOmitsEmptyCollectionsAndAnAbsentPrincipal() {
        assertTrue(headerJson(VesperaPrincipal.of("bob")).endsWith(",\"principal\":{\"name\":\"bob\"}}"));
        assertFalse(headerJson(null).contains("principal"));
        // The Map-based encoder (no HeaderSource) never carries a principal.
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/me", null, Map.of(), null);
        assertFalse(new String(wire, StandardCharsets.UTF_8).contains("principal"));
    }

    // ── ServletPrincipalResolver ──

    @Test
    void anonymousRequestsForwardNothing() {
        assertNull(SECURITY_AWARE.resolvePrincipal(new MockHttpServletRequest()));
        assertNull(SECURITY_AWARE.resolvePrincipal(requestAs(() -> null)));
        assertNull(PrincipalResolver.none().resolvePrincipal(requestAs(() -> "alice")));
    }

    @Test
    void containerPrincipalForwardsItsNameOnly() {
        assertEquals(VesperaPrincipal.of("alice"), SECURITY_AWARE.resolvePrincipal(requestAs(() -> "alice")));
    }

    @Test
    void springSecurityAuthoritiesAreForwardedInOrder() {
        List<Object> authorities = Arrays.asList(
                granted("ROLE_USER"), null, "not-an-authority", granted(null), granted("ROLE_ADMIN"));
        assertEquals(VesperaPrincipal.of("alice", "ROLE_USER", "ROLE_ADMIN"),
                SECURITY_AWARE.resolvePrincipal(requestAs(new Auth("alice", authorities))));
        // a non-iterable authorities value forwards none
        assertEquals(VesperaPrincipal.of("carol"),
                SECURITY_AWARE.resolvePrincipal(requestAs(new Auth("carol", 42))));
    }

    @Test
    void failingAuthoritiesSurfaceAsIllegalState() {
        FakeAuthentication broken = new FakeAuthentication() {
            @Override
            public String getName() {
                return "dave";
            }

            @Override
            public Object getAuthorities() {
                throw new IllegalArgumentException("boom");
            }
        };
        IllegalStateException failure = assertThrows(IllegalStateException.class,
                () -> SECURITY_AWARE.resolvePrincipal(requestAs(broken)));
        assertInstanceOf(IllegalArgumentException.class, failure.getCause().getCause());
    }

    @Test
    void withoutSpringSecurityAuthoritiesAreSkipped() {
        // Spring Security is not on this module's test classpath.
        assertNull(ServletPrincipalResolver.load("org.springframework.security.core.Authentication"));
        assertSame(String.class, ServletPrincipalResolver.load("java.lang.String"));
        ServletPrincipalResolver plain = new ServletPrincipalResolver();
        Auth auth = new Auth("erin", List.of(granted("ROLE_USER")));
        assertEquals(VesperaPrincipal.of("erin"), plain.resolvePrincipal(requestAs(auth)));
        // a type without the expected method also disables the lookup
        ServletPrincipalResolver noMethod =
                new ServletPrincipalResolver(NotAnAuthentication.class, FakeGrantedAuthority.class);
        assertEquals(VesperaPrincipal.of("erin"), noMethod.resolvePrincipal(requestAs(auth)));
        assertNull(ServletPrincipalResolver.method(null, "getAuthorities"));
    }

    // ── autoconfiguration ──

    private final WebApplicationContextRunner runner = new WebApplicationContextRunner()
            .withConfiguration(AutoConfigurations.of(VesperaBridgeAutoConfiguration.class));

    @Configuration(proxyBeanMethods = false)
    static class CustomResolver {
        static final PrincipalResolver RESOLVER = request -> VesperaPrincipal.of("custom");

        @Bean
        PrincipalResolver principalResolver() {
            return RESOLVER;
        }
    }

    @Test
    void autoconfigurationForwardsTheServletPrincipalByDefault() {
        runner.run(ctx -> {
            assertInstanceOf(ServletPrincipalResolver.class, ctx.getBean(PrincipalResolver.class));
            assertTrue(ctx.getBean(VesperaBridgeProperties.class).isForwardPrincipal());
        });
        runner.withPropertyValues("vespera.bridge.forward-principal=false").run(ctx -> assertNull(
                ctx.getBean(PrincipalResolver.class).resolvePrincipal(requestAs(() -> "alice"))));
        runner.withUserConfiguration(CustomResolver.class).run(ctx ->
                assertSame(CustomResolver.RESOLVER, ctx.getBean(PrincipalResolver.class)));
    }
}
