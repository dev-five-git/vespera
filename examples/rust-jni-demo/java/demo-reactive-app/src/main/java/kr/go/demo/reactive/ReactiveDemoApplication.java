package kr.go.demo.reactive;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import reactor.core.publisher.Mono;

/**
 * Spring WebFlux (reactor-netty) host for the same {@code rust_jni_demo}
 * cdylib as demo-app. Nothing vespera-specific is written here: the native
 * library is loaded by {@code vespera.bridge.library-name}
 * (application.yml) and {@code VesperaBridgeReactiveAutoConfiguration}
 * proxies every path no WebFlux endpoint claims to the Rust router through
 * the non-blocking {@code dispatchAsync} symbol.
 */
@SpringBootApplication
public class ReactiveDemoApplication {

    public static void main(String[] args) {
        SpringApplication.run(ReactiveDemoApplication.class, args);
    }

    /** A regular WebFlux endpoint next to the proxy: it wins over the catch-all. */
    @RestController
    static class JavaController {
        @GetMapping("/java/hello")
        Mono<String> hello() {
            return Mono.just("hello from WebFlux");
        }
    }
}
