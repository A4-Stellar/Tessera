package io.tessera.sdk;

/** Base type for every error raised by the Tessera SDK. */
public class TesseraException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    /** Creates an exception with a message. */
    public TesseraException(String message) {
        super(message);
    }

    /** Creates an exception with a message and cause. */
    public TesseraException(String message, Throwable cause) {
        super(message, cause);
    }
}
