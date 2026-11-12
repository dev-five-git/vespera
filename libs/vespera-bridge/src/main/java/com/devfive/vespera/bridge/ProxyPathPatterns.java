package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import org.springframework.http.server.PathContainer;
import org.springframework.web.servlet.mvc.condition.AbstractRequestCondition;
import org.springframework.web.util.ServletRequestPathUtils;
import org.springframework.web.util.UrlPathHelper;
import org.springframework.web.util.pattern.PathPattern;
import org.springframework.web.util.pattern.PathPatternParser;
import org.springframework.web.util.pattern.PatternParseException;

import java.util.ArrayList;
import java.util.Collection;
import java.util.List;

/**
 * The URL space the proxy owns: {@code vespera.bridge.include-patterns}
 * (default {@code /**}) minus {@code vespera.bridge.exclude-patterns}
 * (default none), both in Spring's {@link PathPattern} syntax and matched
 * against the path within the application (context path and a prefix
 * {@code DispatcherServlet} mapping removed — the same path Spring MVC's
 * request mappings see).
 *
 * <p>A request outside that space is not mapped to the proxy at all, so it
 * falls through to the rest of Spring MVC (other handler mappings, static
 * resources, then {@code 404}); see {@link VesperaProxyPathMappings}.
 */
public final class ProxyPathPatterns {

    /** The default include list: the whole URL space. */
    public static final List<String> DEFAULT_INCLUDE = List.of("/**");

    private static final ProxyPathPatterns DEFAULT = new ProxyPathPatterns(DEFAULT_INCLUDE, List.of());

    private final List<String> include;
    private final List<String> exclude;
    private final List<PathPattern> includePatterns;
    private final List<PathPattern> excludePatterns;

    /**
     * @param include include patterns; {@code null} or empty means {@link #DEFAULT_INCLUDE}
     * @param exclude exclude patterns; {@code null} means none
     * @throws IllegalArgumentException when a pattern is not a valid {@link PathPattern}
     */
    public ProxyPathPatterns(Collection<String> include, Collection<String> exclude) {
        List<String> in = normalize(include);
        this.include = in.isEmpty() ? DEFAULT_INCLUDE : in;
        this.exclude = normalize(exclude);
        this.includePatterns = parse(this.include, "vespera.bridge.include-patterns");
        this.excludePatterns = parse(this.exclude, "vespera.bridge.exclude-patterns");
    }

    /** {@code /**} with no exclusions — the historical catch-all. */
    public static ProxyPathPatterns defaults() {
        return DEFAULT;
    }

    public List<String> include() {
        return include;
    }

    public List<String> exclude() {
        return exclude;
    }

    /** Whether this is the catch-all {@code /**} with no exclusions. */
    public boolean isDefault() {
        return exclude.isEmpty() && include.equals(DEFAULT_INCLUDE);
    }

    /** Whether the proxy owns {@code request}'s path. */
    public boolean matches(HttpServletRequest request) {
        if (isDefault()) {
            return true;
        }
        PathContainer path = lookupPath(request);
        return anyMatch(includePatterns, path) && !anyMatch(excludePatterns, path);
    }

    /**
     * Whether the proxy owns {@code path} (a path within the application,
     * e.g. an OpenAPI path template such as {@code /users/{id}}).
     */
    public boolean matchesPath(String path) {
        if (isDefault()) {
            return true;
        }
        PathContainer container = PathContainer.parsePath(path);
        return anyMatch(includePatterns, container) && !anyMatch(excludePatterns, container);
    }

    /** Whether {@code request}'s path matches an exclude pattern. */
    boolean excluded(HttpServletRequest request) {
        return !excludePatterns.isEmpty() && anyMatch(excludePatterns, lookupPath(request));
    }

    /**
     * A request condition that rejects excluded paths, or {@code null} when
     * nothing is excluded.  Added to the proxy's {@code RequestMappingInfo}.
     */
    ExcludeCondition excludeCondition() {
        return exclude.isEmpty() ? null : new ExcludeCondition(this);
    }

    private static boolean anyMatch(List<PathPattern> patterns, PathContainer path) {
        for (PathPattern pattern : patterns) {
            if (pattern.matches(path)) {
                return true;
            }
        }
        return false;
    }

    /**
     * The path within the application: the one {@code DispatcherServlet}
     * parsed and cached for path-pattern handler mappings, or — when none is
     * cached (e.g. {@code ant_path_matcher} everywhere) — the raw
     * (undecoded) lookup path.
     */
    static PathContainer lookupPath(HttpServletRequest request) {
        if (ServletRequestPathUtils.hasParsedRequestPath(request)) {
            return ServletRequestPathUtils.getParsedRequestPath(request).pathWithinApplication();
        }
        return PathContainer.parsePath(UrlPathHelper.rawPathInstance.getLookupPathForRequest(request));
    }

    private static List<String> normalize(Collection<String> patterns) {
        List<String> out = new ArrayList<>();
        if (patterns != null) {
            for (String pattern : patterns) {
                if (pattern != null && !pattern.isBlank()) {
                    out.add(pattern.trim());
                }
            }
        }
        return List.copyOf(out);
    }

    private static List<PathPattern> parse(List<String> patterns, String property) {
        List<PathPattern> parsed = new ArrayList<>(patterns.size());
        for (String pattern : patterns) {
            try {
                parsed.add(PathPatternParser.defaultInstance.parse(pattern));
            } catch (PatternParseException invalid) {
                throw new IllegalArgumentException(
                        "Invalid " + property + " entry '" + pattern + "': " + invalid.getMessage(), invalid);
            }
        }
        return List.copyOf(parsed);
    }

    @Override
    public String toString() {
        return "include=" + include + ", exclude=" + exclude;
    }

    /** Request-mapping condition that does not match an excluded path. */
    static final class ExcludeCondition extends AbstractRequestCondition<ExcludeCondition> {

        private final ProxyPathPatterns patterns;

        ExcludeCondition(ProxyPathPatterns patterns) {
            this.patterns = patterns;
        }

        @Override
        protected Collection<String> getContent() {
            return patterns.exclude;
        }

        @Override
        protected String getToStringInfix() {
            return " && !";
        }

        @Override
        public ExcludeCondition combine(ExcludeCondition other) {
            return other;
        }

        @Override
        public ExcludeCondition getMatchingCondition(HttpServletRequest request) {
            return patterns.excluded(request) ? null : this;
        }

        @Override
        public int compareTo(ExcludeCondition other, HttpServletRequest request) {
            return 0;
        }
    }
}
