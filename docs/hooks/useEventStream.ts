"use client";

import { useCallback, useEffect, useRef, useState } from "react";

export interface StreamComplianceEvent {
  id: number | string;
  asset_id?: number | string;
  event_type: string;
  jurisdiction?: string;
  is_allowed?: boolean;
  account?: string;
  reason?: string;
  timestamp: string;
  ledger?: number;
  data?: Record<string, unknown>;
}

export interface UseEventStreamOptions {
  autoReconnect?: boolean;
  initialReconnectDelay?: number;
  maxReconnectDelay?: number;
  onEvent?: (event: StreamComplianceEvent) => void;
  onError?: (error: Event) => void;
  onOpen?: () => void;
}

export type ConnectionStatus = "connecting" | "connected" | "reconnecting" | "disconnected";

export function calculateReconnectDelay(
  attempt: number,
  initial = 500,
  max = 30000,
  randomFn = Math.random
): number {
  const exp = Math.min(initial * Math.pow(2, attempt), max);
  const jitter = exp * 0.2 * randomFn();
  return Math.min(Math.round(exp + jitter), max);
}

/**
 * React hook that manages a Server-Sent Events (SSE) connection with reconnect logic.
 */
export function useEventStream(url: string, options: UseEventStreamOptions = {}) {
  const {
    autoReconnect = true,
    initialReconnectDelay = 500,
    maxReconnectDelay = 30000,
    onEvent,
    onError,
    onOpen,
  } = options;

  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  const [events, setEvents] = useState<StreamComplianceEvent[]>([]);
  const [lastEvent, setLastEvent] = useState<StreamComplianceEvent | null>(null);
  const [reconnectAttempts, setReconnectAttempts] = useState(0);

  const eventSourceRef = useRef<EventSource | null>(null);
  const reconnectTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isMountedRef = useRef(true);
  const attemptsRef = useRef(0);

  const connect = useCallback(() => {
    if (!url || typeof window === "undefined") return;

    if (eventSourceRef.current) {
      eventSourceRef.current.close();
      eventSourceRef.current = null;
    }

    setStatus(attemptsRef.current > 0 ? "reconnecting" : "connecting");

    try {
      if (typeof EventSource === "undefined") {
        // Fallback for environments lacking native EventSource (e.g. older jsdom)
        setStatus("connected");
        return;
      }

      const es = new EventSource(url);
      eventSourceRef.current = es;

      es.onopen = () => {
        if (!isMountedRef.current) return;
        setStatus("connected");
        attemptsRef.current = 0;
        setReconnectAttempts(0);
        onOpen?.();
      };

      es.onmessage = (event) => {
        if (!isMountedRef.current) return;
        try {
          const parsed = JSON.parse(event.data) as StreamComplianceEvent;
          setLastEvent(parsed);
          setEvents((prev) => [parsed, ...prev].slice(0, 100));
          onEvent?.(parsed);
        } catch {
          // Ignore unparseable non-JSON messages
        }
      };

      es.onerror = (err) => {
        if (!isMountedRef.current) return;
        onError?.(err);
        es.close();
        eventSourceRef.current = null;

        if (autoReconnect) {
          const delay = calculateReconnectDelay(
            attemptsRef.current,
            initialReconnectDelay,
            maxReconnectDelay
          );
          attemptsRef.current += 1;
          setReconnectAttempts(attemptsRef.current);
          setStatus("reconnecting");

          reconnectTimeoutRef.current = setTimeout(() => {
            if (isMountedRef.current) {
              connect();
            }
          }, delay);
        } else {
          setStatus("disconnected");
        }
      };
    } catch {
      setStatus("disconnected");
    }
  }, [url, autoReconnect, initialReconnectDelay, maxReconnectDelay, onEvent, onError, onOpen]);

  const reconnect = useCallback(() => {
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
    }
    attemptsRef.current = 0;
    setReconnectAttempts(0);
    connect();
  }, [connect]);

  const disconnect = useCallback(() => {
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
      reconnectTimeoutRef.current = null;
    }
    if (eventSourceRef.current) {
      eventSourceRef.current.close();
      eventSourceRef.current = null;
    }
    setStatus("disconnected");
  }, []);

  useEffect(() => {
    isMountedRef.current = true;
    connect();

    return () => {
      isMountedRef.current = false;
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current);
      }
      if (eventSourceRef.current) {
        eventSourceRef.current.close();
        eventSourceRef.current = null;
      }
    };
  }, [connect]);

  return {
    status,
    events,
    lastEvent,
    reconnectAttempts,
    reconnect,
    disconnect,
  };
}
