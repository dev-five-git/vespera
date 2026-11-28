package com.devfive.vespera.bridge;

/**
 * Thrown by an {@link AppNameResolver} when a request selects an app that is
 * not exposed through this proxy (see {@code vespera.bridge.exposed-apps}).
 * {@link VesperaProxyController} answers it with {@code 404 Not Found}
 * <em>without</em> dispatching the request to Rust.
 */
public class UnknownAppException extends RuntimeException {

    private static final long serialVersionUID = 1L;

    private final String appName;

    public UnknownAppException(String appName) {
        // The message may reach logs: the raw client value (control characters, a
        // log-forging payload, megabytes) goes through the same loggable form as every
        // other app name in the bridge. appName() keeps the value as selected.
        super("vespera app '" + AppNames.loggable(String.valueOf(appName)) + "' is not exposed by this proxy");
        this.appName = appName;
    }

    /** The rejected app name, as selected by the request. */
    public String appName() {
        return appName;
    }
}
