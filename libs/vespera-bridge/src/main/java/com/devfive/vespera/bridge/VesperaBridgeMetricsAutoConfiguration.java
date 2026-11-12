package com.devfive.vespera.bridge;

import io.micrometer.core.instrument.MeterRegistry;
import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.autoconfigure.condition.ConditionalOnBean;
import org.springframework.boot.autoconfigure.condition.ConditionalOnClass;
import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;
import org.springframework.context.annotation.Bean;

/**
 * Registers {@link MicrometerDispatchObserver} (timer
 * {@value MicrometerDispatchObserver#METER_NAME}) when Micrometer is on the
 * classpath and a {@code MeterRegistry} bean exists (e.g. Spring Boot
 * Actuator). Micrometer is an optional, provided dependency of the bridge:
 * the class-name guards keep this configuration from loading without it.
 * Replace the observer by declaring your own
 * {@code MicrometerDispatchObserver} bean.
 */
@AutoConfiguration(afterName = {
        // Spring Boot 4 / Spring Boot 3 locations of the registry autoconfig.
        "org.springframework.boot.micrometer.metrics.autoconfigure.CompositeMeterRegistryAutoConfiguration",
        "org.springframework.boot.actuate.autoconfigure.metrics.CompositeMeterRegistryAutoConfiguration"})
@ConditionalOnClass(name = "io.micrometer.core.instrument.MeterRegistry")
@ConditionalOnBean(type = "io.micrometer.core.instrument.MeterRegistry")
public class VesperaBridgeMetricsAutoConfiguration {

    @Bean
    @ConditionalOnMissingBean
    public MicrometerDispatchObserver vesperaBridgeMicrometerDispatchObserver(MeterRegistry registry) {
        return new MicrometerDispatchObserver(registry);
    }
}
