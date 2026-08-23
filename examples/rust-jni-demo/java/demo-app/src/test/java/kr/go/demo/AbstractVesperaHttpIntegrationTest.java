package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.web.client.TestRestTemplate;
import org.springframework.http.HttpEntity;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;

abstract class AbstractVesperaHttpIntegrationTest {

    static {
        VesperaBridge.init("rust_jni_demo");
    }

    @Autowired
    protected TestRestTemplate rest;

    protected ResponseEntity<byte[]> exchange(
            HttpMethod method, String path, MediaType contentType, byte[] body) {
        HttpHeaders headers = new HttpHeaders();
        if (contentType != null) {
            headers.setContentType(contentType);
        }
        return rest.exchange(path, method, new HttpEntity<>(body, headers), byte[].class);
    }

    protected ResponseEntity<byte[]> exchangeWithHeaders(
            HttpMethod method, String path, HttpHeaders headers, byte[] body) {
        return rest.exchange(path, method, new HttpEntity<>(body, headers), byte[].class);
    }

    protected static byte[] patternedBytes(int size) {
        byte[] bytes = new byte[size];
        for (int i = 0; i < bytes.length; i++) {
            bytes[i] = (byte) (i * 31 + 7);
        }
        return bytes;
    }
}
