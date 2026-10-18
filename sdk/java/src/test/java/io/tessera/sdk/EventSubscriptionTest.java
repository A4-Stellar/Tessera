package io.tessera.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.tessera.sdk.model.TesseraEvent;
import java.net.URI;
import java.time.Duration;
import java.util.List;
import java.util.Set;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class EventSubscriptionTest {
    private MiniWebSocketServer server;
    private final BlockingQueue<TesseraEvent> events = new LinkedBlockingQueue<>();
    private final BlockingQueue<TesseraException> errors = new LinkedBlockingQueue<>();
    private final CompletableFuture<Integer> closeCode = new CompletableFuture<>();

    private final TesseraEventListener listener = new TesseraEventListener() {
        @Override
        public void onEvent(TesseraEvent event) {
            events.add(event);
        }

        @Override
        public void onError(TesseraException error) {
            errors.add(error);
        }

        @Override
        public void onClose(int statusCode, String reason) {
            closeCode.complete(statusCode);
        }
    };

    @BeforeEach
    void start() throws Exception {
        server = new MiniWebSocketServer();
    }

    @AfterEach
    void stop() throws Exception {
        server.close();
    }

    private TesseraClient client() {
        return TesseraClient.create(TesseraClientConfig.builder()
            .webSocketUri(server.uri())
            .authToken("stream-token")
            .timeout(Duration.ofSeconds(5))
            .build());
    }

    @Test
    void subscribesAndDeliversEventsIncludingFragmentedFrames() throws Exception {
        try (EventSubscription subscription = client().subscribeEvents(List.of("transfer", " dividend ", "transfer", ""), listener)) {
            assertEquals("subscribe:transfer", server.awaitFrame());
            assertEquals("subscribe:dividend", server.awaitFrame());
            assertEquals("Bearer stream-token", server.handshakeHeaders.get(5, TimeUnit.SECONDS).get("authorization"));
            assertEquals(Set.of("transfer", "dividend"), subscription.topics());

            server.sendText("{\"id\":1,\"contract\":\"CASSET\",\"event_type\":\"transfer\",\"ledger\":10,\"data\":{\"amount\":\"5\"}}", true);
            server.sendText("{\"id\":2,\"contract\":\"CASSET\",", false);
            server.sendContinuation("\"event_type\":\"dividend_claimed\",\"ledger\":11,\"data\":null}", true);

            TesseraEvent first = events.poll(5, TimeUnit.SECONDS);
            TesseraEvent second = events.poll(5, TimeUnit.SECONDS);
            assertEquals("transfer", first.eventType());
            assertEquals("5", first.data().get("amount").asText());
            assertEquals("dividend_claimed", second.eventType());
            assertEquals(11L, second.ledger());
        }
        assertEquals("<close>", server.awaitFrame());
    }

    @Test
    void topicsCanBeChangedWhileConnected() throws Exception {
        try (EventSubscription subscription = client().subscribeEvents(List.of("transfer"), listener)) {
            assertEquals("subscribe:transfer", server.awaitFrame());

            subscription.subscribe("compliance").get(5, TimeUnit.SECONDS);
            subscription.unsubscribe("transfer").get(5, TimeUnit.SECONDS);

            assertEquals("subscribe:compliance", server.awaitFrame());
            assertEquals("unsubscribe:transfer", server.awaitFrame());
            assertEquals(Set.of("compliance"), subscription.topics());
            assertThrows(IllegalArgumentException.class, () -> subscription.subscribe(" "));
        }
    }

    @Test
    void malformedFramesAreReportedWithoutClosingTheStream() throws Exception {
        try (EventSubscription subscription = client().subscribeEvents(List.of("transfer"), listener)) {
            server.awaitFrame();
            server.sendText("not-json", true);
            server.sendText("{\"id\":3,\"contract\":\"C\",\"event_type\":\"transfer\",\"ledger\":1,\"data\":{}}", true);

            assertInstanceOf(TesseraSerializationException.class, errors.poll(5, TimeUnit.SECONDS));
            assertEquals(3L, events.poll(5, TimeUnit.SECONDS).id());
            assertTrue(subscription.isOpen());
        }
    }

    @Test
    void serverCloseEndsTheSubscription() throws Exception {
        EventSubscription subscription = client().subscribeEvents(List.of("transfer"), listener);
        server.awaitFrame();

        server.sendClose();

        subscription.closeFuture().get(5, TimeUnit.SECONDS);
        assertEquals(1000, closeCode.get(5, TimeUnit.SECONDS));
        assertTrue(!subscription.isOpen());
    }

    @Test
    void connectionFailuresRaiseNetworkErrors() throws Exception {
        int port = server.uri().getPort();
        server.close();
        TesseraClient client = TesseraClient.create(TesseraClientConfig.builder()
            .webSocketUri(URI.create("ws://127.0.0.1:" + port + "/v1/ws"))
            .timeout(Duration.ofSeconds(2))
            .build());

        TesseraNetworkException error = assertThrows(TesseraNetworkException.class, () -> client.subscribeEvents(List.of("x"), listener));
        assertTrue(error.operation().startsWith("connect ws://127.0.0.1"));
    }
}
