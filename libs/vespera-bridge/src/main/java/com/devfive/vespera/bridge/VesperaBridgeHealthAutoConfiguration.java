package com.devfive.vespera.bridge;

import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

/**
 * Registers the {@code vespera} Actuator health indicator (bean
 * {@value #BEAN_NAME}): {@code UP}, {@code DOWN} while the native library is
 * not initialized, {@code OUT_OF_SERVICE} after
 * {@link VesperaBridge#shutdown(java.time.Duration)} — see
 * {@link VesperaBridgeHealth}. Only when Spring Boot's health API is on the
 * classpath (Boot 4 {@code spring-boot-health} or Boot 3.5
 * {@code spring-boot-actuator}, both optional dependencies of the bridge);
 * disable with {@code management.health.vespera.enabled=false}.
 */
@AutoConfiguration
@ConditionalOnProperty(prefix = "management.health.vespera", name = "enabled", matchIfMissing = true)
public class VesperaBridgeHealthAutoConfiguration {

    static final String BEAN_NAME = "vesperaHealthIndicator";

    /** Spring Boot 4: {@code org.springframework.boot.health.contributor}. */
    @Configuration(proxyBeanMethods = false)
    @ConditionalOnClass(name = "org.springframework.boot.health.contributor.HealthIndicator")
    static class Boot4 {
        @Bean(BEAN_NAME)
        @ConditionalOnMissingBean(name = BEAN_NAME)
        VesperaBridgeHealthIndicator vesperaHealthIndicator() {
            return new VesperaBridgeHealthIndicator();
        }
    }

    /** Spring Boot 3.5: {@code org.springframework.boot.actuate.health}. */
    @Configuration(proxyBeanMethods = false)
    @ConditionalOnClass(name = "org.springframework.boot.actuate.health.HealthIndicator")
    @ConditionalOnMissingClass("org.springframework.boot.health.contributor.HealthIndicator")
    static class Boot3 {
        @Bean(BEAN_NAME)
        @ConditionalOnMissingBean(name = BEAN_NAME)
        VesperaBridgeBoot3HealthIndicator vesperaHealthIndicator() {
            return new VesperaBridgeBoot3HealthIndicator();
        }
    }
}
