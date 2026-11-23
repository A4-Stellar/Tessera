try:
    import hvac
except ImportError:  # pragma: no cover - optional in test environments without hvac
    hvac = None

try:
    import psycopg2
except ImportError:  # pragma: no cover - optional in test environments without psycopg2
    psycopg2 = None

import logging
import time
import threading
from typing import Any, Dict, Optional, Callable
from dataclasses import dataclass
from datetime import datetime, timedelta
import os
import signal
import sys

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# Issue #423: 1h (3600s) short-lived dynamic secret TTL default
DEFAULT_LEASE_TTL_SECONDS = 3600
DEFAULT_K8S_JWT_PATH = "/var/run/secrets/kubernetes.io/serviceaccount/token"


@dataclass
class VaultConfig:
    """HashiCorp Vault configuration (supports Kubernetes ServiceAccount JWT auth & 1h TTLs)"""
    url: str
    token: Optional[str] = None
    database_mount: str = "database"
    role_name: str = "tessera-api-postgres"
    redis_role_name: str = "tessera-api-redis"
    k8s_auth_mount: str = "kubernetes"
    k8s_role_name: str = "tessera-api"
    k8s_jwt_path: str = DEFAULT_K8S_JWT_PATH
    default_ttl_seconds: int = DEFAULT_LEASE_TTL_SECONDS
    namespace: Optional[str] = None


@dataclass
class DatabaseCredentials:
    """Dynamic database or Redis AUTH credentials from Vault (1h TTL)"""
    username: str
    password: str
    lease_id: str
    lease_duration: int
    renewable: bool
    expires_at: datetime


class DatabaseConnectionPool:
    """Manages database connection pool with dynamic 1h-TTL Vault credentials"""

    def __init__(
        self,
        host: str,
        port: int,
        database: str,
        connect_fn: Optional[Callable[..., Any]] = None,
    ):
        self.host = host
        self.port = port
        self.database = database
        self.connect_fn = connect_fn
        self.credentials: Optional[DatabaseCredentials] = None
        self.connection_pool: list = []
        self.max_connections = 10
        self.is_closed: bool = False
        self.lock = threading.Lock()

    def update_credentials(self, credentials: DatabaseCredentials) -> None:
        """Update connection pool with new credentials"""
        with self.lock:
            logger.info(
                f"Updating connection pool with new credentials for user: {credentials.username}"
            )

            # Close existing connections
            self._close_all_connections()

            # Store new credentials and mark pool open
            self.credentials = credentials
            self.is_closed = False

            # Create new connection pool with new credentials
            self._initialize_pool()

            logger.info("Connection pool updated successfully")

    def close_pool_on_lease_expiry(self) -> None:
        """Explicitly close all active connections when a Vault lease expires."""
        with self.lock:
            self._close_all_connections()
            self.credentials = None
            self.is_closed = True

    def _close_all_connections(self) -> None:
        """Close all existing database connections"""
        for conn in self.connection_pool:
            try:
                conn.close()
            except Exception as e:
                logger.error(f"Error closing connection: {e}")

        self.connection_pool = []

    def _initialize_pool(self) -> None:
        """Initialize connection pool with current credentials"""
        if not self.credentials:
            raise ValueError("No credentials available")

        connector = self.connect_fn or (psycopg2.connect if psycopg2 is not None else None)
        if connector is None:
            return

        for _ in range(self.max_connections):
            try:
                conn = connector(
                    host=self.host,
                    port=self.port,
                    database=self.database,
                    user=self.credentials.username,
                    password=self.credentials.password,
                )
                self.connection_pool.append(conn)
            except Exception as e:
                logger.error(f"Error creating connection: {e}")

    def get_connection(self):
        """Get a connection from the pool"""
        with self.lock:
            if self.is_closed or not self.connection_pool:
                raise RuntimeError("Connection pool is closed or empty")
            return self.connection_pool[0]

    def health_check(self) -> bool:
        """Check if connection pool is healthy"""
        try:
            conn = self.get_connection()
            cursor = conn.cursor()
            cursor.execute("SELECT 1")
            cursor.close()
            return True
        except Exception as e:
            logger.error(f"Health check failed: {e}")
            return False


class VaultRotationEngine:
    """
    Manages dynamic secret rotation using HashiCorp Vault (Issue #423)

    Features:
    - Kubernetes ServiceAccount JWT authentication (POST /v1/auth/kubernetes/login)
    - Short-lived PostgreSQL & Redis AUTH credentials (1h / 3600s TTL)
    - Background lease renewal via PUT /v1/sys/leases/renew before expiration
    - Explicit SQL/Redis pool closure and K8s JWT re-authentication on lease expiry
    """

    def __init__(
        self,
        vault_config: VaultConfig,
        db_host: str,
        db_port: int,
        db_name: str,
        rotation_callback: Optional[Callable[[DatabaseCredentials], None]] = None,
        vault_client: Optional[Any] = None,
        connect_fn: Optional[Callable[..., Any]] = None,
    ):
        self.vault_config = vault_config
        self.db_host = db_host
        self.db_port = db_port
        self.db_name = db_name
        self.rotation_callback = rotation_callback

        if vault_client is not None:
            self.vault_client = vault_client
        else:
            if hvac is None:
                raise RuntimeError("hvac is required when no vault_client is injected")
            self.vault_client = hvac.Client(
                url=vault_config.url,
                token=vault_config.token,
                namespace=vault_config.namespace,
            )

        if not self.vault_config.token and os.path.exists(self.vault_config.k8s_jwt_path):
            self.authenticate_kubernetes()

        if hasattr(self.vault_client, "is_authenticated") and not self.vault_client.is_authenticated():
            raise RuntimeError("Failed to authenticate with Vault")

        logger.info("Connected to HashiCorp Vault successfully")

        # Database connection pool
        self.connection_pool = DatabaseConnectionPool(
            db_host, db_port, db_name, connect_fn=connect_fn
        )

        # Current PostgreSQL and Redis credentials
        self.current_credentials: Optional[DatabaseCredentials] = None
        self.current_redis_credentials: Optional[DatabaseCredentials] = None

        # Rotation thread
        self.rotation_thread: Optional[threading.Thread] = None
        self.stop_rotation = threading.Event()

        # Renewal margin (renew when 75% of 1h lease duration has passed)
        self.renewal_margin = 0.75

        # Telemetry counters
        self.renewals_succeeded = 0
        self.renewals_failed = 0
        self.reauthentications = 0
        self.pool_closures = 0

    def authenticate_kubernetes(self, jwt: Optional[str] = None) -> str:
        """
        Authenticate with Vault using the pod's Kubernetes ServiceAccount JWT
        (`POST /v1/auth/kubernetes/login`).
        """
        token_jwt = jwt
        if token_jwt is None:
            with open(self.vault_config.k8s_jwt_path, "r", encoding="utf-8") as f:
                token_jwt = f.read().strip()

        if not token_jwt:
            raise ValueError("Kubernetes ServiceAccount JWT is empty")

        auth_api = getattr(self.vault_client, "auth", None)
        if auth_api is not None and hasattr(auth_api, "kubernetes"):
            resp = auth_api.kubernetes.login(
                role=self.vault_config.k8s_role_name,
                jwt=token_jwt,
                mount_point=self.vault_config.k8s_auth_mount,
            )
        elif hasattr(self.vault_client, "kubernetes_login"):
            resp = self.vault_client.kubernetes_login(
                role=self.vault_config.k8s_role_name,
                jwt=token_jwt,
                mount_point=self.vault_config.k8s_auth_mount,
            )
        else:
            raise RuntimeError("Vault client does not support Kubernetes auth login")

        client_token = resp["auth"]["client_token"]
        self.vault_config.token = client_token
        self.vault_client.token = client_token
        return client_token

    def generate_credentials(
        self,
        role_name: Optional[str] = None,
        now: Optional[datetime] = None,
    ) -> DatabaseCredentials:
        """
        Generate new dynamic database credentials from Vault with 1-hour (3600s) TTL.
        """
        target_role = role_name or self.vault_config.role_name
        current_time = now or datetime.now()
        logger.info(f"Requesting new dynamic credentials from Vault for role {target_role}")

        response = self.vault_client.secrets.database.generate_credentials(
            name=target_role,
            mount_point=self.vault_config.database_mount,
        )

        data = response["data"]
        lease_duration = int(
            response.get("lease_duration") or self.vault_config.default_ttl_seconds
        )

        credentials = DatabaseCredentials(
            username=data["username"],
            password=data["password"],
            lease_id=response["lease_id"],
            lease_duration=lease_duration,
            renewable=response.get("renewable", True),
            expires_at=current_time + timedelta(seconds=lease_duration),
        )

        logger.info(
            f"Generated 1h-TTL credentials for user: {credentials.username}, "
            f"expires at: {credentials.expires_at}, "
            f"lease_id: {credentials.lease_id}"
        )
        return credentials

    def generate_redis_credentials(
        self, now: Optional[datetime] = None
    ) -> DatabaseCredentials:
        """Generate dynamic 1h-TTL Redis AUTH credentials from Vault."""
        creds = self.generate_credentials(
            role_name=self.vault_config.redis_role_name,
            now=now,
        )
        self.current_redis_credentials = creds
        return creds

    def renew_credentials(
        self,
        credentials: DatabaseCredentials,
        now: Optional[datetime] = None,
    ) -> DatabaseCredentials:
        """
        Renew existing credentials lease via `PUT /v1/sys/leases/renew`.
        """
        if not credentials.renewable:
            logger.warning("Credentials are not renewable, generating new ones")
            return self.generate_credentials(now=now)

        current_time = now or datetime.now()
        logger.info(f"Renewing credentials lease via PUT /v1/sys/leases/renew: {credentials.lease_id}")

        response = self.vault_client.sys.renew_lease(
            lease_id=credentials.lease_id,
            increment=self.vault_config.default_ttl_seconds,
        )

        lease_duration = int(
            response.get("lease_duration") or self.vault_config.default_ttl_seconds
        )
        credentials.lease_duration = lease_duration
        credentials.expires_at = current_time + timedelta(seconds=lease_duration)
        self.renewals_succeeded += 1
        return credentials

    def revoke_credentials(self, credentials: DatabaseCredentials) -> None:
        """Revoke database credentials in Vault."""
        logger.info(f"Revoking credentials lease: {credentials.lease_id}")
        try:
            self.vault_client.sys.revoke_lease(credentials.lease_id)
            logger.info("Credentials revoked successfully")
        except Exception as e:
            logger.error(f"Failed to revoke credentials: {e}")

    def rotate_credentials(
        self,
        grace_period_seconds: float = 0.0,
        now: Optional[datetime] = None,
    ) -> None:
        """
        Perform credential rotation for PostgreSQL and Redis:
        1. Generate new 1h-TTL credentials
        2. Update connection pool
        3. Revoke old credentials
        """
        logger.info("Starting credential rotation")
        old_credentials = self.current_credentials

        new_credentials = self.generate_credentials(now=now)
        self.generate_redis_credentials(now=now)

        self.connection_pool.update_credentials(new_credentials)
        if self.connection_pool.connection_pool and not self.connection_pool.health_check():
            raise RuntimeError("Connection pool health check failed after rotation")

        self.current_credentials = new_credentials

        if self.rotation_callback:
            self.rotation_callback(new_credentials)

        if old_credentials:
            if grace_period_seconds > 0:
                time.sleep(grace_period_seconds)
            self.revoke_credentials(old_credentials)

        logger.info("Credential rotation completed successfully")

    def maintain_leases_once(
        self,
        now: Optional[datetime] = None,
        jwt: Optional[str] = None,
    ) -> str:
        """
        Single maintenance tick for Postgres and Redis dynamic secret leases:
        - Renews active leases via `PUT /v1/sys/leases/renew` when due.
        - If renewal fails or a lease expires while Vault is unreachable:
          1. Immediately closes the active DB connection pool and invalidates Redis AUTH tokens.
          2. Re-authenticates via Kubernetes ServiceAccount JWT and rotates credentials
             as soon as Vault becomes reachable again.
        """
        current_time = now or datetime.now()

        if self.current_credentials is None or self.current_redis_credentials is None:
            self.authenticate_kubernetes(jwt=jwt)
            self.rotate_credentials(now=current_time)
            self.reauthentications += 1
            return "reauthenticated"

        pg_remaining = (self.current_credentials.expires_at - current_time).total_seconds()
        redis_remaining = (
            self.current_redis_credentials.expires_at - current_time
        ).total_seconds()

        # Check if already expired
        if pg_remaining <= 0 or redis_remaining <= 0:
            self.connection_pool.close_pool_on_lease_expiry()
            self.current_credentials = None
            self.current_redis_credentials = None
            self.pool_closures += 1
            try:
                self.authenticate_kubernetes(jwt=jwt)
                self.rotate_credentials(now=current_time)
                self.reauthentications += 1
                return "reauthenticated"
            except Exception as err:
                logger.error(f"Vault unreachable after lease expiry; pool remains closed: {err}")
                return "pool_closed_on_expiry"

        renew_window = self.current_credentials.lease_duration * (1.0 - self.renewal_margin)
        if pg_remaining > renew_window and redis_remaining > renew_window:
            return "no_action"

        try:
            self.current_credentials = self.renew_credentials(
                self.current_credentials, now=current_time
            )
            self.current_redis_credentials = self.renew_credentials(
                self.current_redis_credentials, now=current_time
            )
            return "renewed"
        except Exception as err:
            self.renewals_failed += 1
            logger.warning(f"Lease renewal failed ({err}); closing pool and attempting re-auth")
            self.connection_pool.close_pool_on_lease_expiry()
            self.current_credentials = None
            self.current_redis_credentials = None
            self.pool_closures += 1
            try:
                self.authenticate_kubernetes(jwt=jwt)
                self.rotate_credentials(now=current_time)
                self.reauthentications += 1
                return "reauthenticated"
            except Exception:
                return "pool_closed_on_expiry"

    def _rotation_loop(self) -> None:
        """Background thread that monitors credential expiration and triggers renewal/rotation."""
        logger.info("Starting credential rotation loop")
        while not self.stop_rotation.is_set():
            try:
                self.maintain_leases_once()
                self.stop_rotation.wait(60)
            except Exception as e:
                logger.error(f"Error in rotation loop: {e}")
                self.stop_rotation.wait(30)
        logger.info("Rotation loop stopped")

    def start(self) -> None:
        """Start the credential rotation engine"""
        if self.rotation_thread and self.rotation_thread.is_alive():
            logger.warning("Rotation engine is already running")
            return

        self.stop_rotation.clear()
        self.rotation_thread = threading.Thread(target=self._rotation_loop, daemon=True)
        self.rotation_thread.start()

    def stop(self) -> None:
        """Stop the credential rotation engine"""
        self.stop_rotation.set()
        if self.rotation_thread:
            self.rotation_thread.join(timeout=10)
        if self.current_credentials:
            self.revoke_credentials(self.current_credentials)
        if self.current_redis_credentials:
            self.revoke_credentials(self.current_redis_credentials)
        self.connection_pool._close_all_connections()

    def get_connection(self):
        """Get a database connection from the pool"""
        return self.connection_pool.get_connection()

    def health_check(self) -> Dict[str, Any]:
        """Get health status of the rotation engine"""
        if not self.current_credentials:
            return {"healthy": False, "message": "No credentials available"}

        now = datetime.now()
        time_until_expiry = (self.current_credentials.expires_at - now).total_seconds()
        return {
            "healthy": not self.connection_pool.is_closed,
            "credentials": {
                "username": self.current_credentials.username,
                "expires_at": self.current_credentials.expires_at.isoformat(),
                "time_until_expiry_seconds": time_until_expiry,
                "lease_id": self.current_credentials.lease_id,
            },
            "connection_pool": {
                "size": len(self.connection_pool.connection_pool),
                "max_size": self.connection_pool.max_connections,
                "is_closed": self.connection_pool.is_closed,
            },
        }


def rotation_callback_example(credentials: DatabaseCredentials) -> None:
    logger.info(f"Rotation callback: New credentials for user {credentials.username}")


def main():
    vault_config = VaultConfig(
        url=os.getenv("VAULT_ADDR", "http://localhost:8200"),
        token=os.getenv("VAULT_TOKEN"),
        database_mount=os.getenv("VAULT_DB_MOUNT", "database"),
        role_name=os.getenv("VAULT_ROLE_NAME", "tessera-api-postgres"),
        redis_role_name=os.getenv("VAULT_REDIS_ROLE_NAME", "tessera-api-redis"),
        namespace=os.getenv("VAULT_NAMESPACE"),
    )
    db_host = os.getenv("DB_HOST", "localhost")
    db_port = int(os.getenv("DB_PORT", "5432"))
    db_name = os.getenv("DB_NAME", "tessera")

    engine = VaultRotationEngine(
        vault_config=vault_config,
        db_host=db_host,
        db_port=db_port,
        db_name=db_name,
        rotation_callback=rotation_callback_example,
    )

    def signal_handler(sig, frame):
        engine.stop()
        sys.exit(0)

    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)
    engine.start()

    while True:
        time.sleep(60)
        logger.info(f"Health check: {engine.health_check()}")


if __name__ == "__main__":
    main()
