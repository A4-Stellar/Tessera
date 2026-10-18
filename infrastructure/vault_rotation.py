import hvac
import psycopg2
import logging
import time
import threading
from typing import Dict, Optional, Callable
from dataclasses import dataclass
from datetime import datetime, timedelta
import os
import signal
import sys

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

@dataclass
class VaultConfig:
    """HashiCorp Vault configuration"""
    url: str
    token: str
    database_mount: str = "database"
    role_name: str = "api-readonly"
    namespace: Optional[str] = None

@dataclass
class DatabaseCredentials:
    """Dynamic database credentials from Vault"""
    username: str
    password: str
    lease_id: str
    lease_duration: int
    renewable: bool
    expires_at: datetime

class DatabaseConnectionPool:
    """Manages database connection pool with dynamic credentials"""

    def __init__(self, host: str, port: int, database: str):
        self.host = host
        self.port = port
        self.database = database
        self.credentials: Optional[DatabaseCredentials] = None
        self.connection_pool: list = []
        self.max_connections = 10
        self.lock = threading.Lock()

    def update_credentials(self, credentials: DatabaseCredentials) -> None:
        """Update connection pool with new credentials"""
        with self.lock:
            logger.info(f"Updating connection pool with new credentials for user: {credentials.username}")

            # Close existing connections
            self._close_all_connections()

            # Store new credentials
            self.credentials = credentials

            # Create new connection pool with new credentials
            self._initialize_pool()

            logger.info("Connection pool updated successfully")

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

        for _ in range(self.max_connections):
            try:
                conn = psycopg2.connect(
                    host=self.host,
                    port=self.port,
                    database=self.database,
                    user=self.credentials.username,
                    password=self.credentials.password
                )
                self.connection_pool.append(conn)
            except Exception as e:
                logger.error(f"Error creating connection: {e}")

    def get_connection(self):
        """Get a connection from the pool"""
        with self.lock:
            if not self.connection_pool:
                raise RuntimeError("Connection pool is empty")
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
    Manages dynamic secret rotation using HashiCorp Vault

    Features:
    - Generate short-lived PostgreSQL credentials (24h TTL)
    - Automatic credential renewal before expiration
    - Graceful connection pool updates
    - Automatic revocation of expired credentials
    """

    def __init__(
        self,
        vault_config: VaultConfig,
        db_host: str,
        db_port: int,
        db_name: str,
        rotation_callback: Optional[Callable[[DatabaseCredentials], None]] = None
    ):
        self.vault_config = vault_config
        self.db_host = db_host
        self.db_port = db_port
        self.db_name = db_name
        self.rotation_callback = rotation_callback

        # Initialize Vault client
        self.vault_client = hvac.Client(
            url=vault_config.url,
            token=vault_config.token,
            namespace=vault_config.namespace
        )

        # Verify Vault connection
        if not self.vault_client.is_authenticated():
            raise RuntimeError("Failed to authenticate with Vault")

        logger.info("Connected to HashiCorp Vault successfully")

        # Database connection pool
        self.connection_pool = DatabaseConnectionPool(db_host, db_port, db_name)

        # Current credentials
        self.current_credentials: Optional[DatabaseCredentials] = None

        # Rotation thread
        self.rotation_thread: Optional[threading.Thread] = None
        self.stop_rotation = threading.Event()

        # Renewal margin (renew when 80% of lease duration has passed)
        self.renewal_margin = 0.8

    def generate_credentials(self) -> DatabaseCredentials:
        """
        Generate new dynamic database credentials from Vault

        Returns:
            DatabaseCredentials with 24-hour TTL
        """
        logger.info("Requesting new database credentials from Vault")

        try:
            # Request dynamic credentials from Vault
            response = self.vault_client.secrets.database.generate_credentials(
                name=self.vault_config.role_name,
                mount_point=self.vault_config.database_mount
            )

            data = response['data']
            lease_duration = response['lease_duration']

            credentials = DatabaseCredentials(
                username=data['username'],
                password=data['password'],
                lease_id=response['lease_id'],
                lease_duration=lease_duration,
                renewable=response.get('renewable', False),
                expires_at=datetime.now() + timedelta(seconds=lease_duration)
            )

            logger.info(
                f"Generated credentials for user: {credentials.username}, "
                f"expires at: {credentials.expires_at}, "
                f"lease_id: {credentials.lease_id}"
            )

            return credentials

        except Exception as e:
            logger.error(f"Failed to generate credentials: {e}")
            raise

    def renew_credentials(self, credentials: DatabaseCredentials) -> DatabaseCredentials:
        """
        Renew existing credentials lease

        Args:
            credentials: Current credentials to renew

        Returns:
            Updated credentials with extended lease
        """
        if not credentials.renewable:
            logger.warning("Credentials are not renewable, generating new ones")
            return self.generate_credentials()

        logger.info(f"Renewing credentials lease: {credentials.lease_id}")

        try:
            response = self.vault_client.sys.renew_lease(
                lease_id=credentials.lease_id
            )

            lease_duration = response['lease_duration']

            # Update credentials with new lease info
            credentials.lease_duration = lease_duration
            credentials.expires_at = datetime.now() + timedelta(seconds=lease_duration)

            logger.info(f"Credentials renewed, new expiration: {credentials.expires_at}")

            return credentials

        except Exception as e:
            logger.error(f"Failed to renew credentials: {e}")
            # Fall back to generating new credentials
            return self.generate_credentials()

    def revoke_credentials(self, credentials: DatabaseCredentials) -> None:
        """
        Revoke database credentials in Vault

        Args:
            credentials: Credentials to revoke
        """
        logger.info(f"Revoking credentials lease: {credentials.lease_id}")

        try:
            self.vault_client.sys.revoke_lease(credentials.lease_id)
            logger.info("Credentials revoked successfully")
        except Exception as e:
            logger.error(f"Failed to revoke credentials: {e}")

    def rotate_credentials(self) -> None:
        """
        Perform credential rotation:
        1. Generate new credentials
        2. Update connection pool
        3. Revoke old credentials
        """
        logger.info("Starting credential rotation")

        # Store old credentials for revocation
        old_credentials = self.current_credentials

        try:
            # Generate new credentials
            new_credentials = self.generate_credentials()

            # Update connection pool with new credentials
            self.connection_pool.update_credentials(new_credentials)

            # Verify connection pool is healthy
            if not self.connection_pool.health_check():
                raise RuntimeError("Connection pool health check failed after rotation")

            # Update current credentials
            self.current_credentials = new_credentials

            # Call rotation callback if provided
            if self.rotation_callback:
                self.rotation_callback(new_credentials)

            # Revoke old credentials (after a grace period)
            if old_credentials:
                # Wait 30 seconds to ensure all active connections are closed
                logger.info("Waiting 30 seconds before revoking old credentials")
                time.sleep(30)
                self.revoke_credentials(old_credentials)

            logger.info("Credential rotation completed successfully")

        except Exception as e:
            logger.error(f"Credential rotation failed: {e}")
            # If rotation fails and we have old credentials, keep using them
            if old_credentials:
                logger.warning("Keeping old credentials after rotation failure")
                self.current_credentials = old_credentials
            raise

    def _rotation_loop(self) -> None:
        """
        Background thread that monitors credential expiration and triggers rotation
        """
        logger.info("Starting credential rotation loop")

        while not self.stop_rotation.is_set():
            try:
                if not self.current_credentials:
                    # Initial credential generation
                    logger.info("No credentials available, generating initial credentials")
                    self.rotate_credentials()
                else:
                    # Check if credentials need renewal
                    now = datetime.now()
                    time_until_expiry = (self.current_credentials.expires_at - now).total_seconds()
                    renewal_threshold = self.current_credentials.lease_duration * self.renewal_margin

                    if time_until_expiry <= (self.current_credentials.lease_duration - renewal_threshold):
                        logger.info(
                            f"Credentials will expire in {time_until_expiry} seconds, "
                            f"triggering rotation"
                        )
                        self.rotate_credentials()
                    else:
                        logger.debug(
                            f"Credentials still valid for {time_until_expiry} seconds"
                        )

                # Sleep for 5 minutes before next check
                self.stop_rotation.wait(300)

            except Exception as e:
                logger.error(f"Error in rotation loop: {e}")
                # Sleep for 1 minute before retrying
                self.stop_rotation.wait(60)

        logger.info("Rotation loop stopped")

    def start(self) -> None:
        """Start the credential rotation engine"""
        if self.rotation_thread and self.rotation_thread.is_alive():
            logger.warning("Rotation engine is already running")
            return

        logger.info("Starting credential rotation engine")

        self.stop_rotation.clear()
        self.rotation_thread = threading.Thread(target=self._rotation_loop, daemon=True)
        self.rotation_thread.start()

        logger.info("Credential rotation engine started")

    def stop(self) -> None:
        """Stop the credential rotation engine"""
        logger.info("Stopping credential rotation engine")

        self.stop_rotation.set()

        if self.rotation_thread:
            self.rotation_thread.join(timeout=10)

        # Revoke current credentials
        if self.current_credentials:
            self.revoke_credentials(self.current_credentials)

        # Close all connections
        self.connection_pool._close_all_connections()

        logger.info("Credential rotation engine stopped")

    def get_connection(self):
        """Get a database connection from the pool"""
        return self.connection_pool.get_connection()

    def health_check(self) -> Dict[str, any]:
        """
        Get health status of the rotation engine

        Returns:
            Dictionary with health status information
        """
        if not self.current_credentials:
            return {
                "healthy": False,
                "message": "No credentials available"
            }

        now = datetime.now()
        time_until_expiry = (self.current_credentials.expires_at - now).total_seconds()

        return {
            "healthy": self.connection_pool.health_check(),
            "credentials": {
                "username": self.current_credentials.username,
                "expires_at": self.current_credentials.expires_at.isoformat(),
                "time_until_expiry_seconds": time_until_expiry,
                "lease_id": self.current_credentials.lease_id
            },
            "connection_pool": {
                "size": len(self.connection_pool.connection_pool),
                "max_size": self.connection_pool.max_connections
            }
        }

def rotation_callback_example(credentials: DatabaseCredentials) -> None:
    """
    Example callback function called after credential rotation

    This can be used to:
    - Update application configuration
    - Trigger application restarts
    - Send notifications
    - Log rotation events
    """
    logger.info(f"Rotation callback: New credentials for user {credentials.username}")

    # Example: Write credentials to application config
    # with open('/etc/app/db_config.json', 'w') as f:
    #     json.dump({
    #         'username': credentials.username,
    #         'password': credentials.password,
    #         'expires_at': credentials.expires_at.isoformat()
    #     }, f)

    # Example: Send SIGHUP to application to reload config
    # os.kill(app_pid, signal.SIGHUP)

def main():
    """
    Example usage of the Vault Rotation Engine
    """
    # Configuration from environment variables
    vault_config = VaultConfig(
        url=os.getenv('VAULT_ADDR', 'http://localhost:8200'),
        token=os.getenv('VAULT_TOKEN'),
        database_mount=os.getenv('VAULT_DB_MOUNT', 'database'),
        role_name=os.getenv('VAULT_ROLE_NAME', 'api-readonly'),
        namespace=os.getenv('VAULT_NAMESPACE')
    )

    db_host = os.getenv('DB_HOST', 'localhost')
    db_port = int(os.getenv('DB_PORT', '5432'))
    db_name = os.getenv('DB_NAME', 'tessera')

    # Create rotation engine
    engine = VaultRotationEngine(
        vault_config=vault_config,
        db_host=db_host,
        db_port=db_port,
        db_name=db_name,
        rotation_callback=rotation_callback_example
    )

    # Handle graceful shutdown
    def signal_handler(sig, frame):
        logger.info("Received shutdown signal")
        engine.stop()
        sys.exit(0)

    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    # Start the engine
    engine.start()

    # Keep main thread alive
    logger.info("Rotation engine is running. Press Ctrl+C to stop.")

    while True:
        time.sleep(60)
        # Print health status every minute
        health = engine.health_check()
        logger.info(f"Health check: {health}")

if __name__ == "__main__":
    main()
