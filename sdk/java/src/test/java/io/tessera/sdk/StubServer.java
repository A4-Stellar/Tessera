package io.tessera.sdk;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.List;
import java.util.Map;

/** In-process HTTP server that records requests and replays queued responses. */
final class StubServer implements AutoCloseable {

    record Recorded(String method, String pathAndQuery, Map<String, List<String>> headers, String body) {
        String header(String name) {
            return headers.entrySet().stream()
                .filter(e -> e.getKey().equalsIgnoreCase(name))
                .map(e -> e.getValue().get(0))
                .findFirst()
                .orElse(null);
        }
    }

    record Reply(int status, String body, Map<String, String> headers) {
    }

    private final HttpServer server;
    private final Deque<Reply> replies = new ArrayDeque<>();
    final List<Recorded> requests = Collections.synchronizedList(new ArrayList<>());

    StubServer() throws IOException {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", this::handle);
        server.start();
    }

    URI uri() {
        return URI.create("http://127.0.0.1:" + server.getAddress().getPort());
    }

    StubServer reply(int status, String body) {
        return reply(status, body, Map.of());
    }

    synchronized StubServer reply(int status, String body, Map<String, String> headers) {
        replies.add(new Reply(status, body, headers));
        return this;
    }

    private void handle(HttpExchange exchange) throws IOException {
        String body = new String(exchange.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
        requests.add(new Recorded(exchange.getRequestMethod(), exchange.getRequestURI().getRawPath()
            + (exchange.getRequestURI().getRawQuery() == null ? "" : "?" + exchange.getRequestURI().getRawQuery()),
            Map.copyOf(exchange.getRequestHeaders()), body));
        Reply reply;
        synchronized (this) {
            reply = replies.isEmpty() ? new Reply(500, "{\"error\":\"unexpected\",\"message\":\"no stub\"}", Map.of()) : replies.poll();
        }
        byte[] bytes = reply.body().getBytes(StandardCharsets.UTF_8);
        exchange.getResponseHeaders().add("Content-Type", "application/json");
        reply.headers().forEach((k, v) -> exchange.getResponseHeaders().add(k, v));
        exchange.sendResponseHeaders(reply.status(), bytes.length == 0 ? -1 : bytes.length);
        try (OutputStream out = exchange.getResponseBody()) {
            out.write(bytes);
        }
    }

    @Override
    public void close() {
        server.stop(0);
    }
}
