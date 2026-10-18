package io.tessera.sdk;

/** A response body or event frame could not be mapped to the expected model. */
public final class TesseraSerializationException extends TesseraException {
    private static final long serialVersionUID = 1L;

    /** Creates a serialization exception. */
    public TesseraSerializationException(String message, Throwable cause) {
        super(message, cause);
    }
}
