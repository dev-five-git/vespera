package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.context.annotation.ComponentScan;

@SpringBootApplication
@ComponentScan(basePackages = {"kr.go.demo", "com.devfive.vespera.bridge"})
public class DemoApplication {

    public static void main(String[] args) {
        VesperaBridge.init("rust_jni_demo");
        SpringApplication.run(DemoApplication.class, args);
    }
}
