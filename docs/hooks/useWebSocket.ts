"use client";

import { useCallback, useEffect, useRef, useState } from "react";

/**
 * WebSocket connection states.
 */
export type ConnectionStatus =
  | "connecting"
  | "open"
  | "closing"
  | "closed"
  | "reconnecting";

/**
 * Buffered message stored while the socket is disconnected.
 */
export interface BufferedMessage<T = unknown> {
  /** Original receive timestamp (ms since epoch). */
  receivedAt: number;
  /** Parsed message payload. */
  data: T;
}

/**
 * Configuration for the WebSocket hook.
 */
export interface UseWebSocketConfig {
  /** WebSocket URL (ws:// or wss://). */
  url: string;
  /** Maximum number of buffered messages to retain during disconnection. */
  maxBufferSize?: number;
  /** Initial reconnect delay in ms. */
  initialReconnectDelay?: number;
  /** Maximum reconnect delay in ms. */
  maxReconnectDelay?: number;
  /** Maximum number of reconnect attempts before giving up (0 = infinite). */
  maxReconnectAttempts?: number;
  /** Whether to automatically reconnect on unexpected close. */
  autoReconnect?: boolean;
  /** Callback invoked when a message is received. */
  onMessage?: (data: unknown) => void;
  /** Callback invoked when the connection opens. */
  onOpen?: () => void;
  /** Callback invoked when the connection closes. */
  onClose?: (event: CloseEvent) => void;
  /** Callback invoked on connection error. */
  onError?: (event: Event) => void;
  /** Callback invoked when a reconnect attempt fails. */
  onReconnectAttempt?: (attempt: number, delay: number) => void;
  /** Callback invoked when the max reconnect attempts are exhausted. */
  onReconnectExhausted?: () => void;
}

/**
 * Return type for the WebSocket hook.
 */
export interface UseWebSocketReturn {
  /** Current connection status. */
  status: ConnectionStatus;
  /** Whether the socket is currently connected. */
  isConnected: boolean;
  /** Send a text message through the WebSocket. */
  send: (data: string) => void;
  /** Manually close the connection. */
  close: () => void;
  /** Manually reconnect (closes existing socket and opens a new one). */
  reconnect: () => void;
  /** Messages buffered while disconnected, in FIFO order. */
  bufferedMessages: BufferedMessage[];
  /** Number of reconnect attempts made. */
  reconnectAttempts: number;
  /** Clear the message buffer. */
  clearBuffer: () => void;
}

/**
 * React hook that manages a WebSocket connection with automatic reconnection
 * and message buffering during disconnection periods.
 *
 * Features:
 * - Exponential backoff reconnection (configurable)
 * - Message buffering while disconnected (with configurable max size)
 * - Automatic buffer flush on reconnection
 * - Clean teardown on unmount
 *
 * @example
 * ```tsx
 * const { status, isConnected, send, bufferedMessages } = useWebSocket({
 *   url: "ws://localhost:8080/v1/ws",
 *   onMessage: (data) => console.log("Received:", data),
 * });
 * ```
 */
export function useWebSocket({
  url,
  maxBufferSize = 500,
  initialReconnectDelay = 1_000,
  maxReconnectDelay = 30_000,
  maxReconnectAttempts = 0,
  autoReconnect = true,
  onMessage,
  onOpen,
  onClose,
  onError,
  onReconnectAttempt,
  onReconnectExhausted,
}: UseWebSocketConfig): UseWebSocketReturn {
  const [status, setStatus] = useState<ConnectionStatus>("closed");
  const [bufferedMessages, setBufferedMessages] = useState<BufferedMessage[]>([]);
  const [reconnectAttempts, setReconnectAttempts] = useState(0);

  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reconnectAttemptsRef = useRef(0);
  const isManualCloseRef = useRef(false);
  const isUnmountingRef = useRef(false);
  const statusRef = useRef<ConnectionStatus>("closed");

  // Keep statusRef in sync with state for use in callbacks
  useEffect(() => {
    statusRef.current = status;
  }, [status]);

  // Keep callbacks in refs so they don't trigger re-renders
  const onMessageRef = useRef(onMessage);
  const onOpenRef = useRef(onOpen);
  const onCloseRef = useRef(onClose);
  const onErrorRef = useRef(onError);
  const onReconnectAttemptRef = useRef(onReconnectAttempt);
  const onReconnectExhaustedRef = useRef(onReconnectExhausted);

  useEffect(() => {
    onMessageRef.current = onMessage;
    onOpenRef.current = onOpen;
    onCloseRef.current = onClose;
    onErrorRef.current = onError;
    onReconnectAttemptRef.current = onReconnectAttempt;
    onReconnectExhaustedRef.current = onReconnectExhausted;
  });

  const clearBuffer = useCallback(() => {
    setBufferedMessages([]);
  }, []);

  const connect = useCallback(() => {
    if (isUnmountingRef.current) return;

    // Clean up any existing socket
    if (wsRef.current) {
      wsRef.current.onopen = null;
      wsRef.current.onmessage = null;
      wsRef.current.onclose = null;
      wsRef.current.onerror = null;
      if (
        wsRef.current.readyState === WebSocket.OPEN ||
        wsRef.current.readyState === WebSocket.CONNECTING
      ) {
        wsRef.current.close();
      }
      wsRef.current = null;
    }

    setStatus("connecting");

    try {
      const ws = new WebSocket(url);
      wsRef.current = ws;

      ws.onopen = () => {
        if (isUnmountingRef.current) {
          ws.close();
          return;
        }
        reconnectAttemptsRef.current = 0;
        setReconnectAttempts(0);
        setStatus("open");
        onOpenRef.current?.();
      };

      ws.onmessage = (event: MessageEvent) => {
        if (isUnmountingRef.current) return;

        const raw = event.data;
        let parsed: unknown;

        if (typeof raw === "string") {
          try {
            parsed = JSON.parse(raw);
          } catch {
            parsed = raw;
          }
        } else {
          parsed = raw;
        }

        // If disconnected, buffer the message
        if (statusRef.current !== "open") {
          setBufferedMessages((prev) => {
            const next = [...prev, { receivedAt: Date.now(), data: parsed }];
            // Trim buffer to max size (drop oldest)
            if (next.length > maxBufferSize) {
              return next.slice(next.length - maxBufferSize);
            }
            return next;
          });
        }

        onMessageRef.current?.(parsed);
      };

      ws.onclose = (event: CloseEvent) => {
        if (isUnmountingRef.current) return;

        wsRef.current = null;
        setStatus("closed");
        onCloseRef.current?.(event);

        // Attempt reconnection if not manually closed
        if (!isManualCloseRef.current && autoReconnect) {
          const attempt = reconnectAttemptsRef.current + 1;

          if (maxReconnectAttempts > 0 && attempt > maxReconnectAttempts) {
            onReconnectExhaustedRef.current?.();
            return;
          }

          // Exponential backoff with jitter
          const delay = Math.min(
            initialReconnectDelay * Math.pow(2, attempt - 1) +
              Math.random() * 1_000,
            maxReconnectDelay,
          );

          reconnectAttemptsRef.current = attempt;
          setReconnectAttempts(attempt);
          setStatus("reconnecting");
          onReconnectAttemptRef.current?.(attempt, delay);

          reconnectTimeoutRef.current = setTimeout(() => {
            connect();
          }, delay);
        }
      };

      ws.onerror = (event: Event) => {
        if (isUnmountingRef.current) return;
        onErrorRef.current?.(event);
      };
    } catch {
      // Synchronous errors (e.g., invalid URL) — treat as connection failure
      setStatus("closed");
      if (!isManualCloseRef.current && autoReconnect) {
        const attempt = reconnectAttemptsRef.current + 1;
        if (maxReconnectAttempts > 0 && attempt > maxReconnectAttempts) {
          onReconnectExhaustedRef.current?.();
          return;
        }
        const delay = Math.min(
          initialReconnectDelay * Math.pow(2, attempt - 1),
          maxReconnectDelay,
        );
        reconnectAttemptsRef.current = attempt;
        setReconnectAttempts(attempt);
        setStatus("reconnecting");
        onReconnectAttemptRef.current?.(attempt, delay);
        reconnectTimeoutRef.current = setTimeout(() => {
          connect();
        }, delay);
      }
    }
  }, [
    url,
    maxBufferSize,
    initialReconnectDelay,
    maxReconnectDelay,
    maxReconnectAttempts,
    autoReconnect,
  ]);

  const send = useCallback((data: string) => {
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      wsRef.current.send(data);
    }
  }, []);

  const close = useCallback(() => {
    isManualCloseRef.current = true;
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
      reconnectTimeoutRef.current = null;
    }
    if (wsRef.current) {
      wsRef.current.close();
    }
    setStatus("closing");
  }, []);

  const reconnect = useCallback(() => {
    isManualCloseRef.current = false;
    reconnectAttemptsRef.current = 0;
    setReconnectAttempts(0);
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
      reconnectTimeoutRef.current = null;
    }
    connect();
  }, [connect]);

  // Connect on mount, disconnect on unmount
  useEffect(() => {
    isUnmountingRef.current = false;
    isManualCloseRef.current = false;
    connect();

    return () => {
      isUnmountingRef.current = true;
      isManualCloseRef.current = true;
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current);
        reconnectTimeoutRef.current = null;
      }
      if (wsRef.current) {
        wsRef.current.onopen = null;
        wsRef.current.onmessage = null;
        wsRef.current.onclose = null;
        wsRef.current.onerror = null;
        if (
          wsRef.current.readyState === WebSocket.OPEN ||
          wsRef.current.readyState === WebSocket.CONNECTING
        ) {
          wsRef.current.close();
        }
        wsRef.current = null;
      }
    };
  }, [connect]);

  return {
    status,
    isConnected: status === "open",
    send,
    close,
    reconnect,
    bufferedMessages,
    reconnectAttempts,
    clearBuffer,
  };
}
