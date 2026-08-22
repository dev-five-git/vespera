package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;

/**
 * The bridge is registered by Spring Boot AUTOCONFIGURATION, so it must NOT be
 * component-scanned. Adding {@code com.devfive.vespera.bridge} to
 * {@code @ComponentScan} picks {@code VesperaProxyController} up as a plain
 * {@code @RestController}: it declares several constructors and no default one,
 * so the context fails to start with {@code NoSuchMethodException: <init>()},
 * and the scanned bean would also bypass
 * {@code vespera.bridge.controller-enabled} and every
 * {@code @ConditionalOnMissingBean} override.
 *
 * <p>{@code DemoApplicationContextTest} is the regression guard.
 */
@SpringBootApplication
public class DemoApplication {

    public static void main(String[] args) {
        VesperaBridge.init("rust_jni_demo");
        SpringApplication.run(DemoApplication.class, args);
    }
}
