//! Distributed Cross-Region Database Read-Replica Synchronization System.
//!
//! Provides PostgreSQL logical replication across multi-continent cloud regions
//! (US, Europe, Asia), dynamic client read routing based on GeoIP latency, and
//! automated replication lag detection and alert notifications when lag exceeds 2 seconds.
//! (Issue #209)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// Target continents / geographical regions for read-replica deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContinentRegion {
    NorthAmericaUS,
    EuropeEU,
    AsiaAP,
}

impl ContinentRegion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NorthAmericaUS => "us-east-1",
            Self::EuropeEU => "eu-central-1",
            Self::AsiaAP => "ap-northeast-1",
        }
    }
}

impl fmt::Display for ContinentRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Represents a primary or read-replica database node in a specific region.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionReplicaNode {
    pub id: String,
    pub region: ContinentRegion,
    pub endpoint: String,
    pub port: u16,
    pub is_primary: bool,
    pub publication_name: String,
    pub subscription_name: Option<String>,
}

impl RegionReplicaNode {
    pub fn new_primary(
        id: impl Into<String>,
        region: ContinentRegion,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            region,
            endpoint: endpoint.into(),
            port: 5432,
            is_primary: true,
            publication_name: "tessera_global_pub".into(),
            subscription_name: None,
        }
    }

    pub fn new_replica(
        id: impl Into<String>,
        region: ContinentRegion,
        endpoint: impl Into<String>,
        subscription_name: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            region,
            endpoint: endpoint.into(),
            port: 5432,
            is_primary: false,
            publication_name: "tessera_global_pub".into(),
            subscription_name: Some(subscription_name.into()),
        }
    }

    /// Generates the SQL command to create the logical publication.
    pub fn generate_publication_sql(&self, tables: &[&str]) -> String {
        let table_list = if tables.is_empty() {
            "ALL TABLES".to_string()
        } else {
            format!("TABLE {}", tables.join(", "))
        };
        format!(
            "CREATE PUBLICATION {} FOR {};",
            self.publication_name, table_list
        )
    }

    /// Generates the SQL command to create the logical subscription on this replica.
    pub fn generate_subscription_sql(&self, primary_conn_info: &str) -> Option<String> {
        self.subscription_name.as_ref().map(|sub| {
            format!(
                "CREATE SUBSCRIPTION {} CONNECTION '{}' PUBLICATION {} WITH (copy_data = true, create_slot = true, enabled = true);",
                sub, primary_conn_info, self.publication_name
            )
        })
    }
}

/// GeoIP location representation for client requests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientGeoLocation {
    pub ip_address: String,
    pub continent: ContinentRegion,
    pub estimated_latency_map: HashMap<ContinentRegion, Duration>,
}

impl ClientGeoLocation {
    pub fn new(ip: impl Into<String>, continent: ContinentRegion) -> Self {
        let mut map = HashMap::new();
        // Default baseline latency expectations
        match continent {
            ContinentRegion::NorthAmericaUS => {
                map.insert(ContinentRegion::NorthAmericaUS, Duration::from_millis(15));
                map.insert(ContinentRegion::EuropeEU, Duration::from_millis(85));
                map.insert(ContinentRegion::AsiaAP, Duration::from_millis(160));
            }
            ContinentRegion::EuropeEU => {
                map.insert(ContinentRegion::NorthAmericaUS, Duration::from_millis(85));
                map.insert(ContinentRegion::EuropeEU, Duration::from_millis(20));
                map.insert(ContinentRegion::AsiaAP, Duration::from_millis(210));
            }
            ContinentRegion::AsiaAP => {
                map.insert(ContinentRegion::NorthAmericaUS, Duration::from_millis(160));
                map.insert(ContinentRegion::EuropeEU, Duration::from_millis(210));
                map.insert(ContinentRegion::AsiaAP, Duration::from_millis(25));
            }
        }

        Self {
            ip_address: ip.into(),
            continent,
            estimated_latency_map: map,
        }
    }

    pub fn with_custom_latency(mut self, region: ContinentRegion, latency: Duration) -> Self {
        self.estimated_latency_map.insert(region, latency);
        self
    }
}

/// Dynamic read routing engine based on client GeoIP latency.
pub struct DynamicReadRouter {
    nodes: HashMap<ContinentRegion, RegionReplicaNode>,
    degraded_nodes: HashMap<ContinentRegion, bool>,
}

impl DynamicReadRouter {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            degraded_nodes: HashMap::new(),
        }
    }

    pub fn register_node(&mut self, node: RegionReplicaNode) {
        let region = node.region;
        self.nodes.insert(region, node);
        self.degraded_nodes.insert(region, false);
    }

    pub fn set_node_degraded(&mut self, region: ContinentRegion, degraded: bool) {
        self.degraded_nodes.insert(region, degraded);
    }

    /// Selects the optimal read replica for a given client based on lowest estimated GeoIP latency,
    /// avoiding unhealthy or degraded nodes.
    pub fn route_read(&self, client: &ClientGeoLocation) -> Option<&RegionReplicaNode> {
        let mut best_candidate: Option<(&RegionReplicaNode, Duration)> = None;

        for (region, node) in &self.nodes {
            if *self.degraded_nodes.get(region).unwrap_or(&false) {
                // Skip degraded / high-lag replica
                continue;
            }

            if let Some(&latency) = client.estimated_latency_map.get(region) {
                match best_candidate {
                    None => {
                        best_candidate = Some((node, latency));
                    }
                    Some((_, best_latency)) if latency < best_latency => {
                        best_candidate = Some((node, latency));
                    }
                    _ => {}
                }
            }
        }

        // If all available nodes in latency map are degraded or none matched, fallback to any available healthy node
        if best_candidate.is_none() {
            best_candidate = self
                .nodes
                .iter()
                .filter(|(r, _)| !*self.degraded_nodes.get(r).unwrap_or(&false))
                .map(|(_, n)| (n, Duration::from_secs(999)))
                .next();
        }

        best_candidate.map(|(node, _)| node)
    }
}

impl Default for DynamicReadRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Alert notification fired when cross-region replication lag exceeds the SLA threshold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplicationLagAlert {
    pub node_id: String,
    pub region: ContinentRegion,
    pub lag: Duration,
    pub threshold: Duration,
    pub message: String,
    pub timestamp_epoch_secs: u64,
}

/// Monitor for detecting replication lag across target regions and firing alerts when exceeding 2 seconds.
pub struct ReplicationLagMonitor {
    threshold: Duration,
    active_alerts: Vec<ReplicationLagAlert>,
}

impl ReplicationLagMonitor {
    pub const DEFAULT_ALERT_THRESHOLD: Duration = Duration::from_secs(2);

    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            active_alerts: Vec::new(),
        }
    }

    pub fn threshold(&self) -> Duration {
        self.threshold
    }

    /// Checks the replica node's measured replication lag against the threshold.
    /// If lag exceeds 2 seconds (or configured threshold), generates and logs an alert notification.
    pub fn record_replication_lag(
        &mut self,
        node: &RegionReplicaNode,
        lag: Duration,
        timestamp_epoch_secs: u64,
    ) -> Option<ReplicationLagAlert> {
        if lag > self.threshold {
            let alert = ReplicationLagAlert {
                node_id: node.id.clone(),
                region: node.region,
                lag,
                threshold: self.threshold,
                message: format!(
                    "CRITICAL: Cross-region replication lag for node '{}' ({}) is {:.2}s, exceeding SLA limit of {:.2}s",
                    node.id,
                    node.region,
                    lag.as_secs_f64(),
                    self.threshold.as_secs_f64()
                ),
                timestamp_epoch_secs,
            };
            self.active_alerts.push(alert.clone());
            Some(alert)
        } else {
            None
        }
    }

    pub fn active_alerts(&self) -> &[ReplicationLagAlert] {
        &self.active_alerts
    }

    pub fn clear_alerts(&mut self) {
        self.active_alerts.clear();
    }
}

impl Default for ReplicationLagMonitor {
    fn default() -> Self {
        Self::new(Self::DEFAULT_ALERT_THRESHOLD)
    }
}

/// Top-level coordinator managing the 3-region logical replication setup, dynamic read routing, and lag monitoring.
pub struct CrossRegionSyncCoordinator {
    pub primary_region: ContinentRegion,
    pub router: DynamicReadRouter,
    pub lag_monitor: ReplicationLagMonitor,
}

impl CrossRegionSyncCoordinator {
    pub fn new_three_region_setup(
        primary_endpoint: impl Into<String>,
        eu_endpoint: impl Into<String>,
        ap_endpoint: impl Into<String>,
    ) -> Self {
        let mut router = DynamicReadRouter::new();

        let primary = RegionReplicaNode::new_primary(
            "node-us-primary",
            ContinentRegion::NorthAmericaUS,
            primary_endpoint,
        );
        let eu_replica = RegionReplicaNode::new_replica(
            "node-eu-replica",
            ContinentRegion::EuropeEU,
            eu_endpoint,
            "sub_tessera_eu",
        );
        let ap_replica = RegionReplicaNode::new_replica(
            "node-ap-replica",
            ContinentRegion::AsiaAP,
            ap_endpoint,
            "sub_tessera_ap",
        );

        router.register_node(primary);
        router.register_node(eu_replica);
        router.register_node(ap_replica);

        Self {
            primary_region: ContinentRegion::NorthAmericaUS,
            router,
            lag_monitor: ReplicationLagMonitor::default(),
        }
    }

    /// Process a lag measurement, update degraded status in router if lag > 2s, and return alert if any.
    pub fn process_lag_check(
        &mut self,
        region: ContinentRegion,
        lag: Duration,
        timestamp: u64,
    ) -> Option<ReplicationLagAlert> {
        let node = self.router.nodes.get(&region)?.clone();
        let maybe_alert = self
            .lag_monitor
            .record_replication_lag(&node, lag, timestamp);
        if maybe_alert.is_some() {
            // Degrade routing to this node if it's not the primary
            if !node.is_primary {
                self.router.set_node_degraded(region, true);
            }
        } else {
            self.router.set_node_degraded(region, false);
        }
        maybe_alert
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_three_region_publication_and_subscription_generation() {
        let primary = RegionReplicaNode::new_primary(
            "us-master",
            ContinentRegion::NorthAmericaUS,
            "postgres://us.db.tessera.network:5432/tessera",
        );
        let eu_replica = RegionReplicaNode::new_replica(
            "eu-replica",
            ContinentRegion::EuropeEU,
            "postgres://eu.db.tessera.network:5432/tessera",
            "sub_eu_tessera",
        );
        let ap_replica = RegionReplicaNode::new_replica(
            "ap-replica",
            ContinentRegion::AsiaAP,
            "postgres://ap.db.tessera.network:5432/tessera",
            "sub_ap_tessera",
        );

        let pub_sql = primary.generate_publication_sql(&["accounts", "transactions", "assets"]);
        assert_eq!(
            pub_sql,
            "CREATE PUBLICATION tessera_global_pub FOR TABLE accounts, transactions, assets;"
        );

        let eu_sub_sql = eu_replica
            .generate_subscription_sql("host=us.db.tessera.network dbname=tessera")
            .expect("eu sub sql");
        assert!(eu_sub_sql.contains("CREATE SUBSCRIPTION sub_eu_tessera"));
        assert!(eu_sub_sql.contains("PUBLICATION tessera_global_pub"));

        let ap_sub_sql = ap_replica
            .generate_subscription_sql("host=us.db.tessera.network dbname=tessera")
            .expect("ap sub sql");
        assert!(ap_sub_sql.contains("CREATE SUBSCRIPTION sub_ap_tessera"));
        assert!(ap_sub_sql.contains("PUBLICATION tessera_global_pub"));
    }

    #[test]
    fn test_dynamic_read_routing_by_geoip_latency() {
        let coord = CrossRegionSyncCoordinator::new_three_region_setup(
            "us.db.tessera.network",
            "eu.db.tessera.network",
            "ap.db.tessera.network",
        );

        // US client routes to US
        let client_us = ClientGeoLocation::new("198.51.100.1", ContinentRegion::NorthAmericaUS);
        let routed_us = coord.router.route_read(&client_us).expect("routed node");
        assert_eq!(routed_us.region, ContinentRegion::NorthAmericaUS);

        // EU client routes to EU
        let client_eu = ClientGeoLocation::new("195.130.0.1", ContinentRegion::EuropeEU);
        let routed_eu = coord.router.route_read(&client_eu).expect("routed node");
        assert_eq!(routed_eu.region, ContinentRegion::EuropeEU);

        // AP client routes to AP
        let client_ap = ClientGeoLocation::new("203.0.113.1", ContinentRegion::AsiaAP);
        let routed_ap = coord.router.route_read(&client_ap).expect("routed node");
        assert_eq!(routed_ap.region, ContinentRegion::AsiaAP);
    }

    #[test]
    fn test_automated_lag_detection_and_alert_trigger() {
        let mut coord = CrossRegionSyncCoordinator::new_three_region_setup(
            "us.db.tessera.network",
            "eu.db.tessera.network",
            "ap.db.tessera.network",
        );

        // Normal lag under 2s (e.g. 350ms) -> No alert
        let alert1 = coord.process_lag_check(
            ContinentRegion::EuropeEU,
            Duration::from_millis(350),
            1700000000,
        );
        assert!(alert1.is_none());
        assert_eq!(coord.lag_monitor.active_alerts().len(), 0);

        // Lag exceeds 2 seconds (e.g. 2.45s) -> Triggers alert notification
        let alert2 = coord.process_lag_check(
            ContinentRegion::EuropeEU,
            Duration::from_millis(2450),
            1700000010,
        );
        assert!(alert2.is_some());
        let alert = alert2.unwrap();
        assert_eq!(alert.region, ContinentRegion::EuropeEU);
        assert_eq!(alert.threshold, Duration::from_secs(2));
        assert!(alert.lag > Duration::from_secs(2));
        assert_eq!(coord.lag_monitor.active_alerts().len(), 1);

        // Read routing should now failover from degraded EU node to next lowest latency node
        let client_eu = ClientGeoLocation::new("195.130.0.1", ContinentRegion::EuropeEU);
        let fallback_node = coord.router.route_read(&client_eu).expect("fallback node");
        // Next closest after degraded EU is US (85ms vs AP 210ms)
        assert_eq!(fallback_node.region, ContinentRegion::NorthAmericaUS);

        // When lag recovers (e.g. 150ms), status restores
        let alert3 = coord.process_lag_check(
            ContinentRegion::EuropeEU,
            Duration::from_millis(150),
            1700000020,
        );
        assert!(alert3.is_none());
        let restored_node = coord.router.route_read(&client_eu).expect("restored node");
        assert_eq!(restored_node.region, ContinentRegion::EuropeEU);
    }
}

fn main() {
    println!("Tessera cross-region sync initialized.");
}
