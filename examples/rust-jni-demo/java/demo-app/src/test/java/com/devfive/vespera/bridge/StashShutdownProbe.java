package com.devfive.vespera.bridge;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * Child-JVM probe for {@link DirectStashShutdownE2ETest}: two DIRECT
 * overflows are stashed, the runtime is shut down (irreversible for the
 * process, so not in the shared test JVM), then one entry is fetched and the
 * other streamed. Prints {@code RESULT <shutdown> <fetchStatus> <streamStatus>}.
 */
public final class StashShutdownProbe {

    private StashShutdownProbe() {}

    private static long stash(byte[] request) {
        ByteBuffer in = ByteBuffer.allocateDirect(request.length);
        in.put(request).flip();
        ByteBuffer small = ByteBuffer.allocateDirect(64 * 1024);
        int n = DirectStash.dispatchResumable(in, request.length, small);
        if (n >= 0) {
            throw new AssertionError("expected an overflow, got " + n);
        }
        return DirectStash.token(small);
    }

    public static void main(String[] args) throws Exception {
        VesperaBridge.init("rust_jni_demo");
        byte[] request = VesperaBridge.encodeRequest("GET", "/stream/big", null, Map.of(), null);
        long fetched = stash(request);
        long streamed = stash(request);
        boolean shutdown = VesperaBridge.shutdown(Duration.ofSeconds(2));

        ByteBuffer big = ByteBuffer.allocateDirect(1024 * 1024);
        int m = DirectStash.fetch0(fetched, big);
        int fetchStatus = -1;
        if (m >= 0) {
            byte[] wire = new byte[m];
            big.get(0, wire);
            fetchStatus = VesperaBridge.decodeResponse(wire).status();
        }

        List<byte[]> headers = new ArrayList<>();
        DirectStash.stream0(streamed, headers::add, new ByteArrayOutputStream());
        int streamStatus = headers.size() == 1 ? VesperaBridge.decodeResponse(headers.get(0)).status() : -1;
        System.out.println("RESULT " + shutdown + " " + fetchStatus + " " + streamStatus);
        System.exit(0);
    }
}
