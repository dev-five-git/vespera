package com.devfive.vespera.bridge;

import java.util.List;

/**
 * Web-free home of the proxy path defaults. {@link VesperaBridgeProperties}
 * is bound in every application (the core autoconfiguration is active in
 * non-web apps too), so its field initialisers must not touch
 * {@link ProxyPathPatterns}, whose class initialisation needs spring-web.
 */
final class ProxyPathDefaults {

    /** The default include list: the whole URL space. */
    static final List<String> INCLUDE = List.of("/**");

    private ProxyPathDefaults() {}
}
