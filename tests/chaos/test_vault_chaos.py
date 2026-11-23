#!/usr/bin/env python3
"""
Chaos & Lifecycle Tests for HashiCorp Vault Dynamic Secrets (Issue #423)

Tests:
1. Kubernetes ServiceAccount JWT authentication (`POST /v1/auth/kubernetes/login`)
   and 1-hour (3600s) TTL dynamic PostgreSQL & Redis AUTH credential acquisition.
2. In-place lease renewal via `PUT /v1/sys/leases/renew` before expiration.
3. Mid-flight Vault network outage chaos test:
   - Blocks Vault access during the renewal window.
   - Verifies that active database pool connections are explicitly closed on
     renewal failure / lease expiry and Redis AUTH tokens are cleared.
   - Restores Vault access and asserts automatic Kubernetes JWT re-authentication
     and fresh connection pool recovery.
4. Zero-etcd-secret invariant check across Kubernetes manifests.
"""

from datetime import datetime, timedelta
from pathlib import Path
import sys
import unittest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "infrastructure"))

from vault_rotation import (  # noqa: E402
    DEFAULT_LEASE_TTL_SECONDS,
    VaultConfig,
    VaultRotationEngine,
)


class FakeDbCursor:
    def execute(self, _query: str) -> None:
        return None

    def close(self) -> None:
        return None


class FakeDbConnection:
    def __init__(self, user: str, password: str) -> None:
        self.user = user
        self.password = password
        self.closed = False

    def cursor(self) -> FakeDbCursor:
        if self.closed:
            raise RuntimeError("Cannot open cursor on closed connection")
        return FakeDbCursor()

    def close(self) -> None:
        self.closed = True


class FaultInjectingVaultClient:
    """In-memory Vault simulator with mid-flight network partition fault injection."""

    def __init__(self, lease_ttl: int = 3600) -> None:
        self.blocked = False
        self.lease_ttl = lease_ttl
        self.token = None
        self.login_calls = 0
        self.pg_generations = 0
        self.redis_generations = 0
        self.renew_calls = 0
        self.revoke_calls = 0

        self.auth = self
        self.kubernetes = self
        self.secrets = self
        self.database = self
        self.sys = self

    def is_authenticated(self) -> bool:
        return True

    def login(self, role: str, jwt: str, mount_point: str = "kubernetes") -> dict:
        if self.blocked:
            raise ConnectionError("Vault unreachable: mid-flight network partition")
        if not jwt or not role:
            raise PermissionError("Missing Kubernetes ServiceAccount JWT or role")
        self.login_calls += 1
        self.token = f"s.k8s-vault-token-v{self.login_calls}"
        return {
            "auth": {
                "client_token": self.token,
                "lease_duration": self.lease_ttl,
                "renewable": True,
            }
        }

    def generate_credentials(self, name: str, mount_point: str = "database") -> dict:
        if self.blocked:
            raise ConnectionError("Vault unreachable during generate_credentials")
        if "redis" in name:
            self.redis_generations += 1
            v = self.redis_generations
            return {
                "lease_id": f"{mount_point}/creds/{name}/redis-lease-v{v}",
                "lease_duration": self.lease_ttl,
                "renewable": True,
                "data": {
                    "username": f"v_k8s_redis_v{v}",
                    "password": f"redis_auth_token_v{v}",
                },
            }

        self.pg_generations += 1
        v = self.pg_generations
        return {
            "lease_id": f"{mount_point}/creds/{name}/pg-lease-v{v}",
            "lease_duration": self.lease_ttl,
            "renewable": True,
            "data": {
                "username": f"v_k8s_pg_v{v}",
                "password": f"pg_pass_v{v}",
            },
        }

    def renew_lease(self, lease_id: str, increment: int = 3600) -> dict:
        if self.blocked:
            raise ConnectionError("Vault network blocked mid-flight during PUT /v1/sys/leases/renew")
        self.renew_calls += 1
        return {
            "lease_id": lease_id,
            "lease_duration": increment,
            "renewable": True,
        }

    def revoke_lease(self, _lease_id: str) -> None:
        self.revoke_calls += 1


class TestVaultDynamicSecretsChaos(unittest.TestCase):
    def setUp(self) -> None:
        self.t0 = datetime(2026, 11, 23, 12, 0, 0)
        self.vault_sim = FaultInjectingVaultClient(lease_ttl=3600)
        self.created_connections: list[FakeDbConnection] = []

        def fake_connect(**kwargs):
            conn = FakeDbConnection(kwargs["user"], kwargs["password"])
            self.created_connections.append(conn)
            return conn

        self.config = VaultConfig(
            url="http://vault.vault.svc.cluster.local:8200",
            token="s.bootstrap",
            database_mount="database",
            role_name="tessera-api-postgres",
            redis_role_name="tessera-api-redis",
            k8s_auth_mount="kubernetes",
            k8s_role_name="tessera-api",
        )
        self.engine = VaultRotationEngine(
            vault_config=self.config,
            db_host="postgres.prod.svc.cluster.local",
            db_port=5432,
            db_name="tessera",
            vault_client=self.vault_sim,
            connect_fn=fake_connect,
        )

    def test_1h_ttl_startup_and_in_place_lease_renewal(self) -> None:
        self.assertEqual(DEFAULT_LEASE_TTL_SECONDS, 3600)

        outcome = self.engine.maintain_leases_once(
            now=self.t0,
            jwt="eyJhbGciOiJSUzI1NiJ9.sa-jwt.sig",
        )
        self.assertEqual(outcome, "reauthenticated")
        self.assertIsNotNone(self.engine.current_credentials)
        self.assertIsNotNone(self.engine.current_redis_credentials)
        self.assertEqual(self.engine.current_credentials.lease_duration, 3600)
        self.assertEqual(self.engine.current_redis_credentials.lease_duration, 3600)
        self.assertEqual(self.engine.current_credentials.username, "v_k8s_pg_v1")
        self.assertEqual(self.engine.current_redis_credentials.username, "v_k8s_redis_v1")

        # 10 minutes elapsed (< 75% of 1h TTL) -> no action needed
        early_outcome = self.engine.maintain_leases_once(
            now=self.t0 + timedelta(minutes=10),
            jwt="eyJhbGciOiJSUzI1NiJ9.sa-jwt.sig",
        )
        self.assertEqual(early_outcome, "no_action")
        self.assertEqual(self.vault_sim.renew_calls, 0)

        # 48 minutes elapsed (80% of 1h TTL) -> renews Postgres and Redis leases via PUT /v1/sys/leases/renew
        renew_time = self.t0 + timedelta(minutes=48)
        renew_outcome = self.engine.maintain_leases_once(
            now=renew_time,
            jwt="eyJhbGciOiJSUzI1NiJ9.sa-jwt.sig",
        )
        self.assertEqual(renew_outcome, "renewed")
        self.assertEqual(self.vault_sim.renew_calls, 2)
        self.assertEqual(self.engine.renewals_succeeded, 2)
        self.assertFalse(self.engine.connection_pool.is_closed)

    def test_chaos_mid_flight_vault_block_closes_pool_and_reauthenticates_on_recovery(self) -> None:
        # 1. Initial authentication & 1h-TTL dynamic credential provisioning
        self.engine.maintain_leases_once(
            now=self.t0,
            jwt="eyJhbGciOiJSUzI1NiJ9.sa-jwt-v1.sig",
        )
        initial_batch = list(self.created_connections)
        self.assertEqual(len(initial_batch), 10)
        self.assertTrue(all(not c.closed for c in initial_batch))

        # 2. Block Vault mid-flight during renewal window (50 minutes elapsed)
        self.vault_sim.blocked = True
        outage_outcome = self.engine.maintain_leases_once(
            now=self.t0 + timedelta(minutes=50),
            jwt="eyJhbGciOiJSUzI1NiJ9.sa-jwt-v1.sig",
        )
        self.assertEqual(outage_outcome, "pool_closed_on_expiry")
        self.assertEqual(self.engine.pool_closures, 1)
        self.assertTrue(self.engine.connection_pool.is_closed)
        self.assertIsNone(self.engine.current_credentials)
        self.assertIsNone(self.engine.current_redis_credentials)
        # Every underlying connection from generation v1 MUST be closed
        self.assertTrue(all(c.closed for c in initial_batch))
        with self.assertRaises(RuntimeError):
            self.engine.get_connection()

        # 3. Unblock Vault after lease expiration (65 minutes elapsed) -> automatic K8s JWT re-auth
        self.vault_sim.blocked = False
        recovery_outcome = self.engine.maintain_leases_once(
            now=self.t0 + timedelta(minutes=65),
            jwt="eyJhbGciOiJSUzI1NiJ9.sa-jwt-v2.sig",
        )
        self.assertEqual(recovery_outcome, "reauthenticated")
        self.assertEqual(self.engine.reauthentications, 2)
        self.assertFalse(self.engine.connection_pool.is_closed)
        self.assertEqual(self.engine.current_credentials.username, "v_k8s_pg_v2")
        self.assertEqual(self.engine.current_redis_credentials.username, "v_k8s_redis_v2")
        active_conn = self.engine.get_connection()
        self.assertFalse(active_conn.closed)
        self.assertEqual(active_conn.user, "v_k8s_pg_v2")

    def test_zero_k8s_secrets_in_deployment_manifests_and_1h_vault_ttl(self) -> None:
        templates_dir = (
            REPO_ROOT
            / "infrastructure"
            / "k8s"
            / "deployments"
            / "tessera-api"
            / "templates"
        )
        for manifest_path in templates_dir.glob("*.yaml"):
            content = manifest_path.read_text(encoding="utf-8")
            self.assertNotIn("kind: Secret", content)
            self.assertNotIn("secretKeyRef", content)

        rollout_content = (templates_dir / "rollout.yaml").read_text(encoding="utf-8")
        self.assertIn("serviceAccountToken:", rollout_content)
        self.assertIn("VAULT_ADDR", rollout_content)

        tf_content = (REPO_ROOT / "infrastructure" / "vault" / "main.tf").read_text(
            encoding="utf-8"
        )
        self.assertIn("default_ttl = 3600", tf_content)
        self.assertIn("NOSUPERUSER NOCREATEDB NOCREATEROLE", tf_content)
        self.assertIn("tessera-api-redis", tf_content)


if __name__ == "__main__":
    unittest.main()
