package io.tessera.sdk;

/** The request could not reach the API (DNS, connection, TLS or timeout). */
public final class TesseraNetworkException extends TesseraException {
    private static final long serialVersionUID = 1L;

    private final String operation;

    /** Creates a network exception for an operation such as {@code GET /v1/stats}. */
    public TesseraNetworkException(String operation, Throwable cause) {
        super("Tessera network error during " + operation + ": " + cause, cause);
        this.operation = operation;
    }

    /** The operation that failed. */
    public String operation() {
        return operation;
    }
}
