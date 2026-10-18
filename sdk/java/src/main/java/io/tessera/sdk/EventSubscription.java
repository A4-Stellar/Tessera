package io.tessera.sdk;

import com.fasterxml.jackson.databind.ObjectMapper;
import io.tessera.sdk.model.TesseraEvent;
import java.io.IOException;
import java.net.URI;
import java.net.http.WebSocket;
import java.nio.ByteBuffer;
import java.util.Collection;
import java.util.LinkedHashSet;
import java.util.Objects;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * A live connection to Tessera's {@code /v1/ws} event stream. Topics can be added and removed
 * while connected; {@link #close()} ends the stream. Implements {@link AutoCloseable} for
 * try-with-resources.
 *
 * <p>Wire protocol (api/src/ws.rs): the client sends one text frame per topic,
 * {@code subscribe:<topic>} or {@code unsubscribe:<topic>}; the server pushes each matching event
 * as a JSON text frame.
 */
public final class EventSubscription implements AutoCloseable {
    /** Upper bound for one event frame; larger messages are reported as errors. */
    static final int MAX_MESSAGE_CHARS = 4 * 1024 * 1024;

    /** RFC 6455 status for a connection that dropped without a close frame. */
    static final int ABNORMAL_CLOSURE = 1006;

    private final WebSocket socket;
    private final Set<String> topics;
    private final CompletableFuture<Void> closed;
    private final AtomicBoolean closing = new AtomicBoolean();

    private EventSubscription(WebSocket socket, Set<String> topics, CompletableFuture<Void> closed) {
        this.socket = socket;
        this.topics = topics;
        this.closed = closed;
    }

    static CompletableFuture<EventSubscription> open(TesseraClient client, Collection<String> topics, TesseraEventListener listener) {
        Objects.requireNonNull(topics, "topics");
        Objects.requireNonNull(listener, "listener");
        Set<String> normalized = new LinkedHashSet<>();
        for (String topic : topics) {
            if (topic != null && !topic.isBlank()) {
                normalized.add(topic.trim());
            }
        }

        URI endpoint = client.config().webSocketUri();
        CompletableFuture<Void> closed = new CompletableFuture<>();
        WebSocket.Builder builder = client.httpClient().newWebSocketBuilder()
            .connectTimeout(client.config().timeout())
            .header("User-Agent", TesseraClient.USER_AGENT);
        client.config().authToken().ifPresent(token -> builder.header("Authorization", "Bearer " + token));
        client.config().headers().forEach(builder::header);

        Set<String> active = java.util.Collections.synchronizedSet(new LinkedHashSet<>(normalized));
        return builder.buildAsync(endpoint, new Listener(client.objectMapper(), listener, closed))
            .handle((socket, error) -> {
                if (error != null) {
                    Throwable cause = error.getCause() != null ? error.getCause() : error;
                    throw new TesseraNetworkException("connect " + endpoint, cause);
                }
                return socket;
            })
            .thenCompose(socket -> {
                EventSubscription subscription = new EventSubscription(socket, active, closed);
                CompletableFuture<?> sent = CompletableFuture.completedFuture(null);
                for (String topic : normalized) {
                    sent = sent.thenCompose(ignored -> socket.sendText("subscribe:" + topic, true));
                }
                return sent.thenApply(ignored -> subscription);
            });
    }

    /** Subscribes to an additional topic on the open connection. */
    public CompletableFuture<Void> subscribe(String topic) {
        String value = requireTopic(topic);
        topics.add(value);
        return socket.sendText("subscribe:" + value, true).thenApply(ignored -> null);
    }

    /** Stops receiving events for a topic. */
    public CompletableFuture<Void> unsubscribe(String topic) {
        String value = requireTopic(topic);
        topics.remove(value);
        return socket.sendText("unsubscribe:" + value, true).thenApply(ignored -> null);
    }

    /** Topics currently subscribed by this client. */
    public Set<String> topics() {
        synchronized (topics) {
            return Set.copyOf(topics);
        }
    }

    /** Completes when the stream has ended. */
    public CompletableFuture<Void> closeFuture() {
        return closed;
    }

    /** Whether the stream is still open. */
    public boolean isOpen() {
        return !closed.isDone();
    }

    /** Sends a normal close frame and waits briefly for the server to acknowledge it. */
    @Override
    public void close() {
        if (closing.compareAndSet(false, true) && !socket.isOutputClosed()) {
            socket.sendClose(WebSocket.NORMAL_CLOSURE, "client closing")
                .orTimeout(2, TimeUnit.SECONDS)
                .exceptionally(error -> {
                    socket.abort();
                    return null;
                });
        }
        try {
            closed.get(2, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            socket.abort();
        } catch (Exception e) {
            socket.abort();
        }
    }

    private static String requireTopic(String topic) {
        if (topic == null || topic.isBlank()) {
            throw new IllegalArgumentException("topic must not be blank");
        }
        return topic.trim();
    }

    /** Reassembles fragmented text frames and decodes them into events. */
    private static final class Listener implements WebSocket.Listener {
        private final ObjectMapper mapper;
        private final TesseraEventListener delegate;
        private final CompletableFuture<Void> closed;
        private final StringBuilder buffer = new StringBuilder();
        private boolean oversized;

        Listener(ObjectMapper mapper, TesseraEventListener delegate, CompletableFuture<Void> closed) {
            this.mapper = mapper;
            this.delegate = delegate;
            this.closed = closed;
        }

        @Override
        public CompletionStage<?> onText(WebSocket webSocket, CharSequence data, boolean last) {
            if (!oversized) {
                if (buffer.length() + data.length() > MAX_MESSAGE_CHARS) {
                    oversized = true;
                    buffer.setLength(0);
                } else {
                    buffer.append(data);
                }
            }
            if (last) {
                if (oversized) {
                    delegate.onError(new TesseraSerializationException("Event frame exceeded " + MAX_MESSAGE_CHARS + " characters", null));
                } else {
                    dispatch(buffer.toString());
                }
                buffer.setLength(0);
                oversized = false;
            }
            webSocket.request(1);
            return null;
        }

        private void dispatch(String frame) {
            if (frame.isEmpty()) {
                return;
            }
            TesseraEvent event;
            try {
                event = mapper.readValue(frame, TesseraEvent.class);
            } catch (IOException e) {
                delegate.onError(new TesseraSerializationException("Could not decode an event frame from the Tessera event stream", e));
                return;
            }
            delegate.onEvent(event);
        }

        @Override
        public CompletionStage<?> onBinary(WebSocket webSocket, ByteBuffer data, boolean last) {
            webSocket.request(1);
            return null;
        }

        @Override
        public CompletionStage<?> onClose(WebSocket webSocket, int statusCode, String reason) {
            if (!webSocket.isOutputClosed()) {
                webSocket.sendClose(WebSocket.NORMAL_CLOSURE, "");
            }
            finish(statusCode, reason);
            return null;
        }

        @Override
        public void onError(WebSocket webSocket, Throwable error) {
            delegate.onError(new TesseraNetworkException("receive", error));
            finish(ABNORMAL_CLOSURE, String.valueOf(error.getMessage()));
        }

        private void finish(int statusCode, String reason) {
            if (closed.complete(null)) {
                delegate.onClose(statusCode, reason);
            }
        }
    }
}
