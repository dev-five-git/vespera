package com.devfive.vespera.bridge;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Full response-header decode ({@link WireHeaderReader#decode}) — status,
 * headers, metadata and {@code validation_errors} materialised into a
 * {@link WireHeaderReader.Decoded}. Split from {@link WireHeaderReader},
 * whose tokenizer it drives, to keep each file within the size cap.
 */
final class WireHeaderDecoder {
    private WireHeaderDecoder() {}

    static WireHeaderReader.Decoded decodeRoot(WireHeaderReader r) {
        WireHeaderReader.Decoded out = new WireHeaderReader.Decoded();
        r.requireObjectStart();
        r.beginObject();
        int seen = 0;
        int key;
        while ((key = r.nextRootKey()) != WireHeaderReader.KEY_END) {
            seen = r.rejectDuplicateRootKey(seen, key);
            switch (key) {
                case WireHeaderReader.KEY_STATUS -> out.status = r.readStatusCode();
                case WireHeaderReader.KEY_HEADERS -> {
                    if (r.isObjectStart()) {
                        r.beginObject();
                        String k;
                        while ((k = r.nextKeyCanonical()) != null) {
                            if (out.headers == null) {
                                // Pre-size for a typical response header
                                // count (content-type, content-length, …).
                                out.headers = new LinkedHashMap<>(8);
                            }
                            if (r.isArrayStart()) {
                                r.beginArray();
                                List<String> list = new ArrayList<>();
                                while (r.hasNextElement()) {
                                    list.add(r.readString());
                                }
                                out.headers.put(k, list);
                            } else {
                                out.headers.put(k, r.readString());
                            }
                        }
                    } else {
                        r.skipValue();
                    }
                }
                case WireHeaderReader.KEY_METADATA -> {
                    if (r.isObjectStart()) {
                        r.beginObject();
                        out.metadata = readStringMap(r);
                    } else {
                        r.skipValue();
                    }
                }
                case WireHeaderReader.KEY_VALIDATION -> {
                    if (r.isArrayStart()) {
                        r.beginArray();
                        out.validationErrors = new ArrayList<>();
                        while (r.hasNextElement()) {
                            if (!r.isObjectStart()) {
                                // Fixed schema is an array of objects; a
                                // non-object element (only on malformed
                                // input) is skipped so the cursor still
                                // reaches the array end cleanly.
                                r.skipValue();
                                continue;
                            }
                            r.beginObject();
                            Map<String, Object> entry = new LinkedHashMap<>(4);
                            String k;
                            while ((k = r.nextKeyCanonical()) != null) {
                                entry.put(k, r.readPrimitiveValue());
                            }
                            out.validationErrors.add(entry);
                        }
                    } else {
                        r.skipValue();
                    }
                }
                // KEY_OTHER: "v" and any unknown field — value skipped,
                // never materialised.
                default -> r.skipValue();
            }
        }
        r.requireFullyConsumed();
        return out;
    }

    /**
     * Read a string→string object (the {@code metadata} shape) into the
     * smallest map: {@link Map#of()} when empty, a single-entry immutable
     * {@link Map#of(Object, Object)} for the overwhelmingly common one-key
     * case ({@code {"version":...}}) — no hash table allocated — and a
     * mutable {@link LinkedHashMap} only for the rare 2+ key case (which
     * also tolerates duplicate keys, last-wins, like the prior map).
     * Assumes the object was already entered ({@link #beginObject}).
     */
    static Map<String, String> readStringMap(WireHeaderReader r) {
        String k0 = r.nextKeyCanonical();
        if (k0 == null) {
            return Map.of();
        }
        String v0 = r.readString();
        String k1 = r.nextKeyCanonical();
        if (k1 == null) {
            return Map.of(k0, v0);
        }
        Map<String, String> m = new LinkedHashMap<>(8);
        m.put(k0, v0);
        m.put(k1, r.readString());
        String k;
        while ((k = r.nextKeyCanonical()) != null) {
            m.put(k, r.readString());
        }
        return m;
    }
}
