package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Method;
import org.junit.jupiter.api.Test;

/**
 * C-1 adaptive DIRECT-overflow avoidance: a route that overflowed the pooled
 * direct buffer once is streamed up front thereafter, so a known-large
 * (download) route dispatches ONCE instead of paying the
 * DIRECT-overflow-then-stream double dispatch on every request.
 */
class DirectOverflowMemoryTest {

    @Test
    void emptyMemoryNeverAvoidsDirect() {
        // The hot-path guard: until something overflows, shouldAvoidDirect is a
        // single volatile read that returns false — non-overflowing apps pay
        // nothing per DIRECT request.
        DirectOverflowMemory mem = new DirectOverflowMemory();
        assertFalse(mem.shouldAvoidDirect("GET", "/anything"));
        assertEquals(0, mem.size());
    }

    @Test
    void recordedRouteAvoidsDirectExactlyForThatMethodAndPath() {
        DirectOverflowMemory mem = new DirectOverflowMemory();
        mem.recordOverflow("GET", "/big");

        assertTrue(mem.shouldAvoidDirect("GET", "/big"));
        // A distinct path or method must NOT be downgraded.
        assertFalse(mem.shouldAvoidDirect("GET", "/small"));
        assertFalse(mem.shouldAvoidDirect("POST", "/big"));
        assertEquals(1, mem.size());
    }

    @Test
    void queryStringDoesNotBustOverflowMemoryKey() {
        DirectOverflowMemory mem = new DirectOverflowMemory();
        mem.recordOverflow(null, "GET", "/big", "cacheBust=1");

        assertTrue(mem.shouldAvoidDirect(null, "GET", "/big", "cacheBust=2"));
        assertFalse(mem.shouldAvoidDirect("admin", "GET", "/big", "cacheBust=2"));
    }

    @Test
    void reachingTheCapClearsWholesaleThenKeepsLearning() {
        DirectOverflowMemory mem = new DirectOverflowMemory(2);
        mem.recordOverflow("GET", "/a");
        mem.recordOverflow("GET", "/b");
        assertEquals(2, mem.size());
        assertTrue(mem.shouldAvoidDirect("GET", "/a"));

        // Third insert hits the cap (size >= 2) → wholesale clear, then add.
        mem.recordOverflow("GET", "/c");
        assertEquals(1, mem.size());
        assertTrue(mem.shouldAvoidDirect("GET", "/c"));
        // The cleared entries are forgotten (re-learn on their next overflow).
        assertFalse(mem.shouldAvoidDirect("GET", "/a"));
        assertFalse(mem.shouldAvoidDirect("GET", "/b"));
    }

    @Test
    void constructorClampsCapAndDuplicateRouteUsesNormalizedDefaultAppKey() {
        DirectOverflowMemory mem = new DirectOverflowMemory(0);
        mem.recordOverflow("", "GET", "/same", "ignored=1");
        mem.recordOverflow("   ", "GET", "/same", "ignored=2");

        assertEquals(1, mem.size());
        assertTrue(mem.shouldAvoidDirect(null, "GET", "/same", null));

        mem.recordOverflow("admin", "GET", "/other", null);
        assertEquals(1, mem.size());
        assertTrue(mem.shouldAvoidDirect("admin", "GET", "/other", null));
        assertFalse(mem.shouldAvoidDirect(null, "GET", "/same", null));
    }

    @Test
    void routeKeyEqualityHandlesIdentityForeignTypeAndFieldMismatch() throws Exception {
        Class<?> routeKey = Class.forName(
                "com.devfive.vespera.bridge.DirectOverflowMemory$RouteKey");
        Method of = routeKey.getDeclaredMethod("of", String.class, String.class, String.class);
        of.setAccessible(true);
        Object key = of.invoke(null, null, "GET", "/items");
        Object equal = of.invoke(null, " ", "GET", "/items");
        Object differentApp = of.invoke(null, "admin", "GET", "/items");
        Object differentMethod = of.invoke(null, null, "POST", "/items");
        Object differentPath = of.invoke(null, null, "GET", "/other");

        assertTrue(key.equals(key));
        assertTrue(key.equals(equal));
        assertFalse(key.equals("not a route key"));
        assertFalse(key.equals(differentApp));
        assertFalse(key.equals(differentMethod));
        assertFalse(key.equals(differentPath));
        assertEquals(key.hashCode(), equal.hashCode());
    }
}
