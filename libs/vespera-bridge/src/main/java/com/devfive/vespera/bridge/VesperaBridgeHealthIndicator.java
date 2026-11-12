package com.devfive.vespera.bridge;

import org.springframework.boot.health.contributor.Health;
import org.springframework.boot.health.contributor.HealthIndicator;

/**
 * Spring Boot 4 Actuator health indicator {@code vespera} (see
 * {@link VesperaBridgeHealth}). Registered by
 * {@link VesperaBridgeHealthAutoConfiguration} only when the Boot 4 health
 * API is on the classpath; {@code spring-boot-health} is an optional
 * (compile-only) dependency of the bridge.
 */
public class VesperaBridgeHealthIndicator implements HealthIndicator {

    @Override
    public Health health() {
        Health.Builder builder = switch (VesperaBridgeHealth.status()) {
            case UP -> Health.up();
            case DOWN -> Health.down();
            case OUT_OF_SERVICE -> Health.outOfService();
        };
        return builder.withDetails(VesperaBridgeHealth.details()).build();
    }
}
