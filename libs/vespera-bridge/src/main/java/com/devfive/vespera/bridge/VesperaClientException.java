package com.devfive.vespera.bridge;

/**
 * A {@link VesperaClient} request could not be dispatched or its wire
 * response could not be read: the native library is not initialised, the
 * native symbol is missing, the response wire is malformed, or a streamed
 * request/response body failed mid-transfer.
 *
 * <p>An HTTP error <em>status</em> is never a transport failure: {@code 4xx},
 * {@code 5xx}, the {@code 503} a shut-down runtime answers, the {@code 500} a
 * Rust panic answers — all come back as a normal {@link VesperaResponse}.
 */
public final class VesperaClientException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    /**
     * @param message what failed
     */
    public VesperaClientException(String message) {
        super(message);
    }

    /**
     * @param message what failed
     * @param cause   the underlying error
     */
    public VesperaClientException(String message, Throwable cause) {
        super(message, cause);
    }
}
