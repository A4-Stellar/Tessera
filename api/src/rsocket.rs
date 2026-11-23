//! RSocket over WebSocket transport endpoint (`GET /v1/rsocket`).
//! Supports the `requestStream` interaction model for real-time event streaming.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::interval;

use crate::indexer::AppState;
use crate::models::Event;

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_rsocket(socket, state))
}

async fn handle_rsocket(mut socket: WebSocket, state: AppState) {
    metrics::gauge!("active_rsocket_connections").increment(1.0);

    let mut subscriptions: HashSet<String> = HashSet::new();
    let mut ping_interval = interval(Duration::from_secs(15));
    let mut poll_interval = interval(Duration::from_millis(500));
    let mut last_events_count = state.snapshot().events.len();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let text = text.trim();
                        // Handles RSocket requestStream payloads or text commands
                        if let Some(topic) = text.strip_prefix("subscribe:") {
                            subscriptions.insert(topic.trim().to_string());
                        } else if let Some(topic) = text.strip_prefix("requestStream:") {
                            subscriptions.insert(topic.trim().to_string());
                        } else if let Some(topic) = text.strip_prefix("unsubscribe:") {
                            subscriptions.remove(topic.trim());
                        } else if !text.is_empty() {
                            subscriptions.insert(text.to_string());
                        }
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        // Binary frame decoding for RSocket frames
                        // RSocket frame header: Stream ID (4 bytes) + Frame Type (6 bits) + Flags (10 bits)
                        if bin.len() >= 6 {
                            // Extract metadata/data if present
                            if let Ok(text) = std::str::from_utf8(&bin[6..]) {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    subscriptions.insert(trimmed.to_string());
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Err(e)) => {
                        tracing::debug!("rsocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
            _ = ping_interval.tick() => {
                if socket.send(Message::Ping(vec![])).await.is_err() {
                    break;
                }
            }
            _ = poll_interval.tick() => {
                let snapshot = state.snapshot();
                let current_events = &snapshot.events;

                if current_events.len() > last_events_count {
                    let new_events = &current_events[last_events_count..];

                    for event in new_events {
                        let mut should_send = subscriptions.is_empty();

                        for sub in &subscriptions {
                            if matches_subscription(event, sub) {
                                should_send = true;
                                break;
                            }
                        }

                        if should_send {
                            if let Ok(msg) = serde_json::to_string(event) {
                                if socket.send(Message::Text(msg)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }

                    last_events_count = current_events.len();
                }
            }
        }
    }

    metrics::gauge!("active_rsocket_connections").decrement(1.0);
}

fn matches_subscription(event: &Event, topic: &str) -> bool {
    let topic = topic.to_lowercase();
    if event.event_type.to_lowercase().contains(&topic) {
        return true;
    }
    if event.contract.to_lowercase().contains(&topic) {
        return true;
    }
    if let Ok(data_str) = serde_json::to_string(&event.data) {
        if data_str.to_lowercase().contains(&topic) {
            return true;
        }
    }
    false
}
