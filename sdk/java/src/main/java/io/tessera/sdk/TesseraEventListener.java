package io.tessera.sdk;

import io.tessera.sdk.model.TesseraEvent;

/**
 * Receives events from an {@link EventSubscription}. Callbacks run sequentially on the HTTP
 * client's executor; keep them short or hand work off to your own executor.
 */
@FunctionalInterface
public interface TesseraEventListener {

    /** Called for every event the server pushes for the subscribed topics. */
    void onEvent(TesseraEvent event);

    /**
     * Called when a frame cannot be decoded ({@link TesseraSerializationException}) or the
     * connection fails ({@link TesseraNetworkException}). The default implementation ignores it.
     */
    default void onError(TesseraException error) {
    }

    /** Called once when the stream ends, whether the server or the client closed it. */
    default void onClose(int statusCode, String reason) {
    }
}
