package com.devfive.vespera.bridge;

import java.util.UUID;
import java.util.concurrent.ThreadLocalRandom;

/**
 * Request-id resolution shared by the servlet and reactive proxies
 * ({@code vespera.bridge.request-id.*}). Servlet- and WebFlux-free.
 *
 * <p>Package-private: an internal collaborator of the proxies.
 */
final class RequestIds {
    private RequestIds() {}

    /**
     * The client's request id when it is a sane token (1-128 visible ASCII
     * characters), else a fresh random UUID.
     */
    static String resolve(String incoming) {
        if (incoming != null && isValid(incoming)) {
            return incoming;
        }
        ThreadLocalRandom random = ThreadLocalRandom.current();
        // Random (version 4, IETF variant) UUID without SecureRandom's cost:
        // a request id needs uniqueness, not unpredictability.
        long msb = (random.nextLong() & ~0xF000L) | 0x4000L;
        long lsb = (random.nextLong() & ~(0xC000L << 48)) | (0x8000L << 48);
        return new UUID(msb, lsb).toString();
    }

    static boolean isValid(String value) {
        int length = value.length();
        if (length == 0 || length > 128) {
            return false;
        }
        for (int i = 0; i < length; i++) {
            char c = value.charAt(i);
            if (c < 0x21 || c > 0x7e) {
                return false;
            }
        }
        return true;
    }
}
