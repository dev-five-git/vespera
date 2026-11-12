package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

/** Without a native library both routes reach their (missing) native symbol. */
class ParkingDispatchTest {

    @Test
    void virtualThreadsTakeTheAsyncRoutePlatformThreadsTheSyncOne() {
        UnsatisfiedLinkError async = assertThrows(UnsatisfiedLinkError.class,
                () -> ParkingDispatch.dispatchBytes(new byte[4], true));
        org.junit.jupiter.api.Assertions.assertTrue(
                async.getMessage().contains("dispatchAsync"), async.getMessage());
        UnsatisfiedLinkError sync = assertThrows(UnsatisfiedLinkError.class,
                () -> ParkingDispatch.dispatchBytes(new byte[4], false));
        org.junit.jupiter.api.Assertions.assertTrue(
                sync.getMessage().contains("dispatchBytes"), sync.getMessage());
    }
}
