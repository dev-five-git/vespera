package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Per-request servlet metadata snapshot for the proxy hot path.
 *
 * <p>Servlet facades may compute/decode method, content length, protocol, and
 * headers lazily. The Spring proxy and smart resolver need the same values, so
 * capture them once and stash the immutable shape as a request attribute.
 */
final class RequestShape {

    private static final String ATTRIBUTE = RequestShape.class.getName();

    final String method;
    final long contentLength;
    final boolean transferEncodingPresent;
    final boolean definitelyBodyless;
    final boolean currentThreadIsVirtual;

    private RequestShape(
            String method,
            long contentLength,
            boolean transferEncodingPresent,
            boolean definitelyBodyless,
            boolean currentThreadIsVirtual) {
        this.method = method;
        this.contentLength = contentLength;
        this.transferEncodingPresent = transferEncodingPresent;
        this.definitelyBodyless = definitelyBodyless;
        this.currentThreadIsVirtual = currentThreadIsVirtual;
    }

    static RequestShape capture(HttpServletRequest request) {
        Object existing = request.getAttribute(ATTRIBUTE);
        if (existing instanceof RequestShape shape) {
            return shape;
        }
        String method = request.getMethod();
        long contentLength = request.getContentLengthLong();
        boolean transferEncodingPresent = request.getHeader("Transfer-Encoding") != null;
        boolean definitelyBodyless = definitelyBodyless(request, method, contentLength, transferEncodingPresent);
        RequestShape shape = new RequestShape(
                method,
                contentLength,
                transferEncodingPresent,
                definitelyBodyless,
                VesperaBridge.currentThreadIsVirtual());
        request.setAttribute(ATTRIBUTE, shape);
        return shape;
    }

    static RequestShape from(HttpServletRequest request) {
        Object existing = request.getAttribute(ATTRIBUTE);
        return existing instanceof RequestShape shape ? shape : capture(request);
    }

    static boolean definitelyBodyless(HttpServletRequest request) {
        return from(request).definitelyBodyless;
    }

    private static boolean definitelyBodyless(
            HttpServletRequest request,
            String method,
            long contentLength,
            boolean transferEncodingPresent) {
        if (transferEncodingPresent) {
            return false;
        }
        if (contentLength == 0) {
            return true;
        }
        if (contentLength > 0) {
            return false;
        }
        String protocol = request.getProtocol();
        if (protocol == null || !protocol.regionMatches(true, 0, "HTTP/1.", 0, 7)) {
            return false;
        }
        return HttpMethods.isSafe(method);
    }
}
