use async_nats::Client;
use serde::Serialize;
use std::error::Error;

/// A producer module that streams decoded Soroban events to an external message bus (NATS JetStream).
pub struct NatsEventProducer {
    client: Client,
    dlq_subject: String,
}

#[derive(Serialize, Debug)]
pub struct SorobanEventPayload {
    pub contract_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
}

impl NatsEventProducer {
    /// Initializes a new NATS producer.
    pub async fn new(nats_url: &str, dlq_subject: &str) -> Result<Self, Box<dyn Error>> {
        let client = async_nats::connect(nats_url).await?;
        Ok(Self {
            client,
            dlq_subject: dlq_subject.to_string(),
        })
    }

    /// Publishes an event partitioned by contract address to guarantee message ordering per RWA asset.
    pub async fn publish_event(&self, event: &SorobanEventPayload) -> Result<(), Box<dyn Error>> {
        let subject = format!("soroban.events.{}", event.contract_id);
        let payload = serde_json::to_vec(event)?;

        match self.client.publish(subject.clone(), payload.into()).await {
            Ok(_) => {
                // Wait for the server to acknowledge the publish if JetStream was used, 
                // but basic publish is also fine for demonstration.
                Ok(())
            },
            Err(e) => {
                // If publishing fails or payload is corrupted/unparseable (in a broader context), 
                // route to the Dead-Letter Queue (DLQ)
                self.send_to_dlq(event, &e.to_string()).await?;
                Err(Box::new(e))
            }
        }
    }

    /// Routes corrupted or failed messages to a Dead-Letter Queue (DLQ).
    pub async fn send_to_dlq(&self, event: &SorobanEventPayload, error_msg: &str) -> Result<(), Box<dyn Error>> {
        let dlq_payload = serde_json::json!({
            "error": error_msg,
            "original_event": event
        });
        
        let payload = serde_json::to_vec(&dlq_payload)?;
        self.client.publish(self.dlq_subject.clone(), payload.into()).await?;
        Ok(())
    }
}
