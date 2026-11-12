package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.fail;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import java.util.Random;
import org.junit.jupiter.api.Test;

/**
 * Seeded randomized robustness test for {@link WireHeaderReader} — the
 * Java-side counterpart of the Rust {@code fuzz/} targets.
 *
 * <p>Well-formed response headers (the shapes the Rust side emits) are
 * mutated byte-wise (flip / insert / delete / duplicate a span / truncate)
 * and fed to every entry point — {@code apply} and {@code decode}, each over
 * a heap array, a heap {@link ByteBuffer} and a direct {@link ByteBuffer}.
 * The contract under hostile bytes:
 *
 * <ul>
 *   <li>the only permitted failure is {@link IllegalArgumentException} (the
 *       reader's "malformed wire header" signal) — never an
 *       {@code IndexOutOfBoundsException}, {@code NullPointerException},
 *       {@code StackOverflowError}, … escaping into the proxy;</li>
 *   <li>all backings reach the same outcome: identical status / headers on
 *       success, a rejection on every backing otherwise.</li>
 * </ul>
 *
 * <p>Deterministic: a fixed seed, so a failure reproduces exactly; the
 * failing input is printed as hex. Plain {@link Random}, no extra test
 * dependency.
 */
class WireHeaderReaderFuzzTest {

    private static final long SEED = 0x5EED_CAFE_F00DL;
    private static final int ITERATIONS = 20_000;

    private static final String[] SEEDS = {
        "{\"v\":1,\"status\":200,\"headers\":{\"content-type\":\"application/json\"},"
                + "\"metadata\":{\"version\":\"1\"}}",
        "{\"v\":1,\"status\":201,\"headers\":{\"set-cookie\":[\"a=1\",\"b=2\"],"
                + "\"x-esc\":\"q\\\"b\\\\s\\/\\b\\f\\n\\r\\t\\u00e9\\ud83d\\ude00\"},\"metadata\":{}}",
        "{\"v\":1,\"status\":500,\"headers\":{},\"metadata\":{\"panicked\":\"true\"}}",
        "{\"v\":1,\"status\":422,\"headers\":{\"content-type\":\"application/json\"},"
                + "\"metadata\":{},\"validation_errors\":[{\"path\":\"name\",\"message\":\"too short\"},"
                + "{\"path\":\"\",\"message\":\"x\",\"extra\":[1,-2.5e3,true,false,null,{}]}]}",
        "{\"status\":204,\"v\":1,\"metadata\":{\"a\":\"b\",\"c\":\"d\"},\"unknown\":{\"x\":[[[]]]}}",
    };

    /** Bytes biased toward JSON structure so mutations reach deep states. */
    private static final byte[] ALPHABET = ("{}[]\",:\\u0123456789abcdefnulltrue-+.eE \t\n")
            .getBytes(StandardCharsets.US_ASCII);

    private sealed interface Outcome permits Ok, Rejected {}

    private record Ok(int status, List<String> headers) implements Outcome {}

    private record Rejected() implements Outcome {}

    @Test
    void mutatedHeadersNeverThrowUnexpectedly() {
        Random rnd = new Random(SEED);
        for (int i = 0; i < ITERATIONS; i++) {
            byte[] input = mutate(rnd, SEEDS[rnd.nextInt(SEEDS.length)]
                    .getBytes(StandardCharsets.UTF_8));
            check(i, input);
        }
    }

    @Test
    void randomBytesNeverThrowUnexpectedly() {
        Random rnd = new Random(SEED ^ 0xA5A5A5A5L);
        for (int i = 0; i < ITERATIONS / 4; i++) {
            byte[] input = new byte[rnd.nextInt(64)];
            rnd.nextBytes(input);
            if (input.length > 0 && rnd.nextBoolean()) {
                input[0] = '{';
            }
            check(i, input);
        }
    }

    private static byte[] mutate(Random rnd, byte[] seed) {
        List<Byte> d = new ArrayList<>(seed.length + 8);
        for (byte b : seed) {
            d.add(b);
        }
        int ops = 1 + rnd.nextInt(4);
        for (int k = 0; k < ops; k++) {
            int pos = d.isEmpty() ? 0 : rnd.nextInt(d.size());
            byte b = rnd.nextInt(8) == 0
                    ? (byte) rnd.nextInt(256)
                    : ALPHABET[rnd.nextInt(ALPHABET.length)];
            switch (rnd.nextInt(5)) {
                case 0 -> {
                    if (!d.isEmpty()) {
                        d.set(pos, b);
                    }
                }
                case 1 -> d.add(pos, b);
                case 2 -> {
                    if (!d.isEmpty()) {
                        d.remove(pos);
                    }
                }
                case 3 -> {
                    int endIdx = Math.min(d.size(), pos + rnd.nextInt(16));
                    d.addAll(pos, new ArrayList<>(d.subList(pos, endIdx)));
                }
                default -> {
                    if (!d.isEmpty()) {
                        d.subList(rnd.nextInt(d.size()), d.size()).clear();
                    }
                }
            }
        }
        byte[] out = new byte[d.size()];
        for (int j = 0; j < out.length; j++) {
            out[j] = d.get(j);
        }
        return out;
    }

    private static void check(int iteration, byte[] input) {
        // Offset the payload inside a larger buffer so off/len handling is
        // exercised too (a reader that strays past `len` would see the
        // trailing garbage and diverge from the exact-size array backing).
        byte[] padded = new byte[input.length + 6];
        padded[0] = '{';
        padded[1] = '"';
        System.arraycopy(input, 0, padded, 3, input.length);
        padded[padded.length - 3] = '}';
        padded[padded.length - 1] = ']';
        ByteBuffer direct = ByteBuffer.allocateDirect(padded.length);
        direct.put(padded);

        Outcome array = applyOutcome(iteration, input, () -> applyArray(input, 0, input.length));
        Outcome heapPadded = applyOutcome(iteration, input,
                () -> applyBuffer(ByteBuffer.wrap(padded), 3, input.length));
        Outcome directPadded = applyOutcome(iteration, input,
                () -> applyBuffer(direct, 3, input.length));
        assertEquals(array, heapPadded, () -> "array vs heap apply diverged on " + hex(input));
        assertEquals(array, directPadded, () -> "array vs direct apply diverged on " + hex(input));

        Outcome decodedArray = applyOutcome(iteration, input,
                () -> decoded(WireHeaderReader.decode(input, 0, input.length)));
        Outcome decodedDirect = applyOutcome(iteration, input,
                () -> decoded(WireHeaderReader.decode(direct, 3, input.length)));
        assertEquals(decodedArray, decodedDirect,
                () -> "array vs direct decode diverged on " + hex(input));
    }

    private static Ok applyArray(byte[] buf, int off, int len) {
        List<String> headers = new ArrayList<>();
        int status = WireHeaderReader.apply(buf, off, len, (k, v) -> headers.add(k + "=" + v));
        return new Ok(status, headers);
    }

    private static Ok applyBuffer(ByteBuffer buf, int off, int len) {
        List<String> headers = new ArrayList<>();
        int status = WireHeaderReader.apply(buf, off, len, (k, v) -> headers.add(k + "=" + v));
        return new Ok(status, headers);
    }

    private static Ok decoded(WireHeaderReader.Decoded d) {
        List<String> parts = new ArrayList<>();
        parts.add(String.valueOf(d.headers));
        parts.add(String.valueOf(d.metadata));
        parts.add(String.valueOf(d.validationErrors));
        return new Ok(d.status, parts);
    }

    private interface Parse {
        Ok run();
    }

    private static Outcome applyOutcome(int iteration, byte[] input, Parse parse) {
        try {
            return parse.run();
        } catch (IllegalArgumentException expected) {
            return new Rejected();
        } catch (RuntimeException | StackOverflowError unexpected) {
            fail("iteration " + iteration + ": unexpected " + unexpected + " on " + hex(input),
                    unexpected);
            throw new AssertionError("unreachable");
        }
    }

    private static String hex(byte[] input) {
        return HexFormat.of().formatHex(input)
                + " (" + new String(input, StandardCharsets.UTF_8) + ")";
    }
}
