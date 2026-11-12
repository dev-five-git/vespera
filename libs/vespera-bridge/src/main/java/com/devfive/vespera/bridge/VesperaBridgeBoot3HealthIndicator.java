package com.devfive.vespera.bridge;

import org.springframework.boot.actuate.health.Health;
import org.springframework.boot.actuate.health.HealthIndicator;

/**
 * Spring Boot 3.5 Actuator health indicator {@code vespera} (see
 * {@link VesperaBridgeHealth}); the Boot 4 counterpart is
 * {@link VesperaBridgeHealthIndicator}. Registered by
 * {@link VesperaBridgeHealthAutoConfiguration} only when the Boot 3 health API
 * is on the classpath; {@code spring-boot-actuator} is an optional
 * (compile-only) dependency of the bridge.
 */
public class VesperaBridgeBoot3HealthIndicator implements HealthIndicator {

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
