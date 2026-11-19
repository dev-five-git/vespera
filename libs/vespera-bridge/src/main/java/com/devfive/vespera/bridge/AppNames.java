package com.devfive.vespera.bridge;

import java.util.regex.Pattern;

/**
 * App-name handling shared by the client, the header selection and the logs.
 *
 * <p>Rust trims an app name with {@code str::trim_ascii} (space, tab, newline,
 * form feed, carriage return only) and accepts ASCII letters, digits, {@code _}
 * and {@code -} up to 64 bytes. The bridge follows the same rules so a name is
 * routed to the same app on both sides of the JNI boundary.
 *
 * <p>Package-private: an internal collaborator of the bridge.
 */
final class AppNames {

    /** The longest name Rust accepts ({@code MAX_APP_NAME_LEN}). */
    static final int MAX_NAME_LENGTH = 64;

    private static final Pattern ACCEPTED =
            Pattern.compile("[A-Za-z0-9_-]{1," + MAX_NAME_LENGTH + "}");

    private AppNames() {
    }

    /**
     * Strip leading and trailing ASCII whitespace the way Rust's
     * {@code str::trim_ascii} does. {@code null} stays {@code null}.
     */
    static String trimAscii(String value) {
        if (value == null) {
            return null;
        }
        int start = 0;
        int end = value.length();
        while (start < end && isAsciiWhitespace(value.charAt(start))) {
            start++;
        }
        while (end > start && isAsciiWhitespace(value.charAt(end - 1))) {
            end--;
        }
        return start == 0 && end == value.length() ? value : value.substring(start, end);
    }

    /**
     * The name as it may appear in a log line: a name Rust accepts verbatim,
     * anything else (control characters, non-ASCII, overlong) replaced by a
     * placeholder that reports only its length.
     */
    static String loggable(String app) {
        if (ACCEPTED.matcher(app).matches()) {
            return app;
        }
        return "<invalid app name of " + app.length() + " chars>";
    }

    private static boolean isAsciiWhitespace(char c) {
        return c == ' ' || c == '\t' || c == '\n' || c == '\f' || c == '\r';
    }
}
