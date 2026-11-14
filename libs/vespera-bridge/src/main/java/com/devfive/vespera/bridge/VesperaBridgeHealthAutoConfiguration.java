package com.devfive.vespera.bridge;

import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingClass;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Condition;
import org.springframework.context.annotation.ConditionContext;
import org.springframework.context.annotation.Conditional;
import org.springframework.context.annotation.Configuration;
import org.springframework.core.env.Environment;
import org.springframework.core.type.AnnotatedTypeMetadata;

/**
 * Registers the {@code vespera} Actuator health indicator (bean
 * {@value #BEAN_NAME}): {@code UP}, {@code DOWN} while the native library is
 * not initialized, {@code OUT_OF_SERVICE} after
 * {@link VesperaBridge#shutdown(java.time.Duration)} — see
 * {@link VesperaBridgeHealth}. Only when Spring Boot's health API is on the
 * classpath (Boot 4 {@code spring-boot-health} or Boot 3.5
 * {@code spring-boot-actuator}, both optional dependencies of the bridge);
 * disable with {@code management.health.vespera.enabled=false}, or with
 * {@code management.health.defaults.enabled=false} while
 * {@code management.health.vespera.enabled} is unset (the semantics of Boot's
 * {@code @ConditionalOnEnabledHealthIndicator}: the specific key wins).
 */
@AutoConfiguration
@Conditional(VesperaBridgeHealthAutoConfiguration.HealthIndicatorEnabled.class)
public class VesperaBridgeHealthAutoConfiguration {

    static final String BEAN_NAME = "vesperaHealthIndicator";
    static final String ENABLED_PROPERTY = "management.health.vespera.enabled";
    static final String DEFAULTS_PROPERTY = "management.health.defaults.enabled";

    /**
     * {@code management.health.vespera.enabled} when set, else
     * {@code management.health.defaults.enabled} (default {@code true}).
     */
    static final class HealthIndicatorEnabled implements Condition {
        @Override
        public boolean matches(ConditionContext context, AnnotatedTypeMetadata metadata) {
            Environment env = context.getEnvironment();
            if (env.containsProperty(ENABLED_PROPERTY)) {
                return env.getProperty(ENABLED_PROPERTY, Boolean.class, Boolean.TRUE);
            }
            return env.getProperty(DEFAULTS_PROPERTY, Boolean.class, Boolean.TRUE);
        }
    }

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
