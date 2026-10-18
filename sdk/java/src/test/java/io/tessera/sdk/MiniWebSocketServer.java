package io.tessera.sdk;

import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/** Minimal single-connection RFC 6455 server for testing the event stream. */
final class MiniWebSocketServer implements AutoCloseable {
    private final ServerSocket serverSocket;
    private final CompletableFuture<Socket> connection = new CompletableFuture<>();
    final CompletableFuture<Map<String, String>> handshakeHeaders = new CompletableFuture<>();
    final BlockingQueue<String> received = new LinkedBlockingQueue<>();

    MiniWebSocketServer() throws IOException {
        serverSocket = new ServerSocket(0, 1, InetAddress.getLoopbackAddress());
        Thread acceptor = new Thread(this::acceptOne, "mini-ws");
        acceptor.setDaemon(true);
        acceptor.start();
    }

    URI uri() {
        return URI.create("ws://127.0.0.1:" + serverSocket.getLocalPort() + "/v1/ws");
    }

    private void acceptOne() {
        try {
            Socket socket = serverSocket.accept();
            InputStream in = socket.getInputStream();
            Map<String, String> headers = readHeaders(in);
            handshakeHeaders.complete(headers);
            String accept = Base64.getEncoder().encodeToString(MessageDigest.getInstance("SHA-1")
                .digest((headers.get("sec-websocket-key") + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").getBytes(StandardCharsets.US_ASCII)));
            OutputStream out = socket.getOutputStream();
            out.write(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                + "Sec-WebSocket-Accept: " + accept + "\r\n\r\n").getBytes(StandardCharsets.US_ASCII));
            out.flush();
            connection.complete(socket);
            readFrames(new DataInputStream(in));
        } catch (IOException | NoSuchAlgorithmException e) {
            connection.completeExceptionally(e);
        }
    }

    private static Map<String, String> readHeaders(InputStream in) throws IOException {
        ByteArrayOutputStream head = new ByteArrayOutputStream();
        int b;
        while ((b = in.read()) != -1) {
            head.write(b);
            byte[] bytes = head.toByteArray();
            int n = bytes.length;
            if (n >= 4 && bytes[n - 4] == '\r' && bytes[n - 3] == '\n' && bytes[n - 2] == '\r' && bytes[n - 1] == '\n') {
                break;
            }
        }
        Map<String, String> headers = new LinkedHashMap<>();
        String[] lines = head.toString(StandardCharsets.US_ASCII).split("\r\n");
        for (int i = 1; i < lines.length; i++) {
            int colon = lines[i].indexOf(':');
            if (colon > 0) {
                headers.put(lines[i].substring(0, colon).trim().toLowerCase(), lines[i].substring(colon + 1).trim());
            }
        }
        return headers;
    }

    private void readFrames(DataInputStream in) {
        try {
            while (true) {
                int b0 = in.readUnsignedByte();
                int b1 = in.readUnsignedByte();
                int opcode = b0 & 0x0F;
                long length = b1 & 0x7F;
                if (length == 126) {
                    length = in.readUnsignedShort();
                } else if (length == 127) {
                    length = in.readLong();
                }
                byte[] mask = new byte[4];
                if ((b1 & 0x80) != 0) {
                    in.readFully(mask);
                }
                byte[] payload = new byte[(int) length];
                in.readFully(payload);
                for (int i = 0; i < payload.length; i++) {
                    payload[i] ^= mask[i % 4];
                }
                if (opcode == 0x8) {
                    received.add("<close>");
                    return;
                }
                if (opcode == 0x1) {
                    received.add(new String(payload, StandardCharsets.UTF_8));
                }
            }
        } catch (IOException ignored) {
            // Connection closed.
        }
    }

    /** Next text frame (or {@code <close>}) sent by the client, waiting up to five seconds. */
    String awaitFrame() throws InterruptedException {
        return received.poll(5, TimeUnit.SECONDS);
    }

    void sendText(String text, boolean fin) throws Exception {
        sendFrame(fin ? 0x81 : 0x01, text.getBytes(StandardCharsets.UTF_8));
    }

    void sendContinuation(String text, boolean fin) throws Exception {
        sendFrame(fin ? 0x80 : 0x00, text.getBytes(StandardCharsets.UTF_8));
    }

    void sendClose() throws Exception {
        sendFrame(0x88, new byte[] {0x03, (byte) 0xE8});
    }

    private synchronized void sendFrame(int firstByte, byte[] payload) throws Exception {
        OutputStream out = connection.get(5, TimeUnit.SECONDS).getOutputStream();
        out.write(firstByte);
        if (payload.length < 126) {
            out.write(payload.length);
        } else {
            out.write(126);
            out.write((payload.length >> 8) & 0xFF);
            out.write(payload.length & 0xFF);
        }
        out.write(payload);
        out.flush();
    }

    @Override
    public void close() throws IOException {
        serverSocket.close();
        Socket socket = connection.isCompletedExceptionally() ? null : connection.getNow(null);
        if (socket != null) {
            socket.close();
        }
    }
}
