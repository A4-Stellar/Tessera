//! DNS providers used to re-route API traffic to the new primary.
//!
//! Both providers rewrite a single CNAME (e.g. `db.tessera.example`) that the
//! API uses as its database host. Keep its TTL low (30-60s) so clients pick up
//! the change quickly.

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::failover::DnsUpdater;
use crate::types::{DnsError, Node};

/// Cloudflare DNS: `PUT /zones/{zone}/dns_records/{record}`.
pub struct CloudflareDns {
    client: reqwest::Client,
    api_base: String,
    zone_id: String,
    record_id: String,
    record_name: String,
    api_token: String,
    ttl: u32,
}

impl CloudflareDns {
    pub fn new(
        zone_id: String,
        record_id: String,
        record_name: String,
        api_token: String,
        ttl: u32,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_base: "https://api.cloudflare.com/client/v4".into(),
            zone_id,
            record_id,
            record_name,
            api_token,
            ttl,
        }
    }

    fn body(&self, target: &Node) -> serde_json::Value {
        // Database traffic is not HTTP, so the record must never be proxied.
        json!({
            "type": "CNAME",
            "name": self.record_name,
            "content": target.endpoint,
            "ttl": self.ttl,
            "proxied": false,
            "comment": format!("tessera-orchestrator failover to {} ({})", target.name, target.region),
        })
    }
}

#[async_trait]
impl DnsUpdater for CloudflareDns {
    async fn point_to(&self, target: &Node) -> Result<(), DnsError> {
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            self.api_base, self.zone_id, self.record_id
        );
        let response = self
            .client
            .put(url)
            .bearer_auth(&self.api_token)
            .json(&self.body(target))
            .send()
            .await
            .map_err(|e| DnsError::Request(e.to_string()))?;
        let status = response.status();
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|e| DnsError::Request(e.to_string()))?;
        if status.is_success() && payload["success"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(DnsError::Rejected(format!(
                "{status}: {}",
                payload["errors"]
            )))
        }
    }
}

/// AWS credentials for Route53, normally read from the standard
/// `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` vars.
#[derive(Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Route53: `POST /2013-04-01/hostedzone/{id}/rrset/` with an UPSERT change.
pub struct Route53Dns {
    client: reqwest::Client,
    host: String,
    hosted_zone_id: String,
    record_name: String,
    ttl: u32,
    credentials: AwsCredentials,
}

impl Route53Dns {
    pub fn new(
        hosted_zone_id: &str,
        record_name: String,
        ttl: u32,
        credentials: AwsCredentials,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            host: "route53.amazonaws.com".into(),
            hosted_zone_id: hosted_zone_id
                .trim_start_matches("/hostedzone/")
                .to_string(),
            record_name,
            ttl,
            credentials,
        }
    }

    fn path(&self) -> String {
        format!("/2013-04-01/hostedzone/{}/rrset/", self.hosted_zone_id)
    }

    fn body(&self, target: &Node) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ChangeResourceRecordSetsRequest xmlns="https://route53.amazonaws.com/doc/2013-04-01/">
  <ChangeBatch>
    <Comment>tessera-orchestrator failover to {name} ({region})</Comment>
    <Changes>
      <Change>
        <Action>UPSERT</Action>
        <ResourceRecordSet>
          <Name>{record}</Name>
          <Type>CNAME</Type>
          <TTL>{ttl}</TTL>
          <ResourceRecords>
            <ResourceRecord><Value>{endpoint}</Value></ResourceRecord>
          </ResourceRecords>
        </ResourceRecordSet>
      </Change>
    </Changes>
  </ChangeBatch>
</ChangeResourceRecordSetsRequest>"#,
            name = xml_escape(&target.name),
            region = xml_escape(&target.region),
            record = xml_escape(&self.record_name),
            ttl = self.ttl,
            endpoint = xml_escape(&target.endpoint),
        )
    }
}

#[async_trait]
impl DnsUpdater for Route53Dns {
    async fn point_to(&self, target: &Node) -> Result<(), DnsError> {
        let body = self.body(target);
        let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let mut headers = vec![
            ("content-type".to_string(), "text/xml".to_string()),
            ("host".to_string(), self.host.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        if let Some(token) = &self.credentials.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        let authorization = sigv4_authorization(&SigV4Request {
            method: "POST",
            path: &self.path(),
            query: "",
            headers: &headers,
            payload: body.as_bytes(),
            credentials: &self.credentials,
            region: "us-east-1", // Route53 is a global service signed in us-east-1.
            service: "route53",
            amz_date: &amz_date,
        });

        let mut request = self
            .client
            .post(format!("https://{}{}", self.host, self.path()))
            .header("authorization", authorization)
            .body(body);
        for (name, value) in headers.iter().filter(|(n, _)| n != "host") {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|e| DnsError::Request(e.to_string()))?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let text = response.text().await.unwrap_or_default();
            Err(DnsError::Rejected(format!("{status}: {text}")))
        }
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) struct SigV4Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Already canonical (sorted, URI-encoded) query string.
    pub query: &'a str,
    /// Lower-case header names; must include `host` and `x-amz-date`.
    pub headers: &'a [(String, String)],
    pub payload: &'a [u8],
    pub credentials: &'a AwsCredentials,
    pub region: &'a str,
    pub service: &'a str,
    /// `YYYYMMDDTHHMMSSZ`
    pub amz_date: &'a str,
}

/// AWS Signature Version 4 `Authorization` header value.
pub(crate) fn sigv4_authorization(req: &SigV4Request<'_>) -> String {
    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        req.path,
        req.query,
        canonical_headers,
        signed_headers,
        hex::encode(Sha256::digest(req.payload))
    );
    let date = &req.amz_date[..8];
    let scope = format!("{date}/{}/{}/aws4_request", req.region, req.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        req.amz_date,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let k_date = hmac(
        format!("AWS4{}", req.credentials.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, req.region.as_bytes());
    let k_service = hmac(&k_region, req.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        req.credentials.access_key_id
    )
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Node {
        Node {
            name: "euw1".into(),
            region: "eu-west-1".into(),
            endpoint: "tessera-euw1.cluster.eu-west-1.rds.amazonaws.com".into(),
        }
    }

    /// Reference vector from the AWS SigV4 documentation (IAM ListUsers).
    #[test]
    fn sigv4_matches_aws_reference_vector() {
        let credentials = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let headers = vec![
            (
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded; charset=utf-8".to_string(),
            ),
            ("Host".to_string(), "iam.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let auth = sigv4_authorization(&SigV4Request {
            method: "GET",
            path: "/",
            query: "Action=ListUsers&Version=2010-05-08",
            headers: &headers,
            payload: b"",
            credentials: &credentials,
            region: "us-east-1",
            service: "iam",
            amz_date: "20150830T123600Z",
        });
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    #[test]
    fn route53_change_batch_upserts_cname_to_new_primary() {
        let dns = Route53Dns::new(
            "/hostedzone/Z0123456789",
            "db.tessera.example".into(),
            30,
            AwsCredentials {
                access_key_id: "AKID".into(),
                secret_access_key: "secret".into(),
                session_token: None,
            },
        );
        assert_eq!(dns.path(), "/2013-04-01/hostedzone/Z0123456789/rrset/");
        let body = dns.body(&target());
        assert!(body.contains("<Action>UPSERT</Action>"));
        assert!(body.contains("<Name>db.tessera.example</Name>"));
        assert!(body.contains("<TTL>30</TTL>"));
        assert!(body.contains("<Value>tessera-euw1.cluster.eu-west-1.rds.amazonaws.com</Value>"));
    }

    #[test]
    fn cloudflare_record_is_unproxied_cname() {
        let dns = CloudflareDns::new(
            "zone".into(),
            "rec".into(),
            "db.tessera.example".into(),
            "t".into(),
            60,
        );
        let body = dns.body(&target());
        assert_eq!(body["type"], "CNAME");
        assert_eq!(
            body["content"],
            "tessera-euw1.cluster.eu-west-1.rds.amazonaws.com"
        );
        assert_eq!(body["proxied"], false);
        assert_eq!(body["ttl"], 60);
    }

    #[test]
    fn xml_values_are_escaped() {
        assert_eq!(xml_escape("a<b>&\"'"), "a&lt;b&gt;&amp;&quot;&apos;");
    }
}
