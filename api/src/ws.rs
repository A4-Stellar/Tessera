use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::interval;

use crate::indexer::AppState;
use crate::models::Event;

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    metrics::gauge!("active_websocket_connections").increment(1.0);

    let mut subscriptions: HashSet<String> = HashSet::new();
    
    let mut ping_interval = interval(Duration::from_secs(15));
    let mut poll_interval = interval(Duration::from_secs(2));
    
    let mut last_events_count = state.snapshot().events.len();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let text = text.trim();
                        if let Some(topic) = text.strip_prefix("subscribe:") {
                            subscriptions.insert(topic.trim().to_string());
                        } else if let Some(topic) = text.strip_prefix("unsubscribe:") {
                            subscriptions.remove(topic.trim());
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Pong received, nothing to do
                    }
                    Some(Err(e)) => {
                        tracing::debug!("websocket error: {}", e);
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
                        let mut should_send = subscriptions.is_empty(); // If no subscriptions, do we send all? Let's say no, only send if matches. Or send all if empty? Usually send all if no sub. Wait, no, only send what is subscribed.
                        
                        // If there are subscriptions, check if event matches any
                        for sub in &subscriptions {
                            if matches_subscription(event, sub) {
                                should_send = true;
                                break;
                            }
                        }
                        
                        if should_send {
                            if let Ok(msg) = serde_json::to_string(event) {
                                if socket.send(Message::Text(msg)).await.is_err() {
                                    break; // break the loop, socket closed
                                }
                            }
                        }
                    }
                    
                    last_events_count = current_events.len();
                }
            }
        }
    }

    metrics::gauge!("active_websocket_connections").decrement(1.0);
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
