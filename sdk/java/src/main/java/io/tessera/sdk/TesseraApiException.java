package io.tessera.sdk;

/**
 * The API answered with a non-success status. {@link #errorCode()} carries the machine-readable
 * {@code error} field of the response body (for example {@code not_found}).
 */
public class TesseraApiException extends TesseraException {
    private static final long serialVersionUID = 1L;

    private final int statusCode;
    private final String errorCode;
    private final String apiMessage;

    /** Creates an exception for an error response. */
    public TesseraApiException(int statusCode, String errorCode, String apiMessage) {
        super("Tessera API error (" + statusCode + " " + errorCode + "): " + apiMessage);
        this.statusCode = statusCode;
        this.errorCode = errorCode;
        this.apiMessage = apiMessage;
    }

    /** HTTP status returned by the API. */
    public int statusCode() {
        return statusCode;
    }

    /** The {@code error} code from the response body, or {@code http_<status>} when absent. */
    public String errorCode() {
        return errorCode;
    }

    /** The human-readable {@code message} from the response body, or the raw body. */
    public String apiMessage() {
        return apiMessage;
    }
}
