#!/usr/bin/env bash
# =============================================================================
# Tessera — Encrypted, immutable PostgreSQL backup to an air-gapped vault
# Issue #259: airgap_backup.sh
#
# Pipeline (plaintext never touches disk):
#
#   pg_dump -Fc ──▶ gpg --encrypt (public key only) ──▶ <db>-<UTC>.dump.gpg
#        └─ sha256 sidecar ──▶ upload with SSE-KMS / CMEK ──▶ WORM bucket
#           (S3 Object Lock COMPLIANCE or GCS locked Bucket Lock, >= 7 years)
#
# The host holds only the GPG *public* key, so a compromised backup host can
# neither read old backups nor delete them. The private key lives offline,
# KMS-wrapped, and is only needed by dr_test.sh at restore time. Objects are
# written in the format dr_test.sh restores (*.dump.gpg + .sha256 sidecar).
#
# Run daily, e.g. cron:  15 2 * * *  /opt/tessera/airgap_backup.sh
#
# Environment:
#   BACKUP_PROVIDER            s3 | gcs                                (default: s3)
#   BACKUP_BUCKET              vault bucket in the isolated account     (required)
#   BACKUP_KEY_PREFIX          object prefix                            (default: backups/postgres)
#   KMS_KEY_ID                 s3:  KMS key ARN for SSE-KMS             (required)
#                              gcs: projects/../cryptoKeys/.. (CMEK)
#   GPG_PUBLIC_KEY_FILE        armored/binary public key to encrypt to  (required)
#   GPG_RECIPIENT_FINGERPRINT  40-hex fingerprint pinned for that key   (required)
#   PGHOST PGPORT PGUSER PGDATABASE PGPASSWORD|PGPASSFILE  standard libpq variables
#                              (PGDATABASE required)
#   BACKUP_WORK_DIR            scratch space for the encrypted dump     (default: $TMPDIR)
#
# Exit codes: 0 backup stored and verified immutable; 1 any failure.
# Dependencies: pg_dump, gpg, jq, sha256sum, and aws-cli v2 or gcloud.
# =============================================================================

set -euo pipefail

readonly SCRIPT_NAME="${BASH_SOURCE[0]##*/}"
readonly MIN_RETENTION_DAYS=2555                    # 7 years x 365 days
readonly MIN_RETENTION_SECONDS=$(( MIN_RETENTION_DAYS * 86400 ))

BACKUP_PROVIDER="${BACKUP_PROVIDER:-s3}"
BACKUP_BUCKET="${BACKUP_BUCKET:-}"
BACKUP_KEY_PREFIX="${BACKUP_KEY_PREFIX:-backups/postgres}"
KMS_KEY_ID="${KMS_KEY_ID:-}"
GPG_PUBLIC_KEY_FILE="${GPG_PUBLIC_KEY_FILE:-}"
GPG_RECIPIENT_FINGERPRINT="${GPG_RECIPIENT_FINGERPRINT:-}"
BACKUP_WORK_DIR="${BACKUP_WORK_DIR:-${TMPDIR:-/tmp}}"

log() {
  local level="$1"; shift
  printf '[%s] [%s] %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$level" "$SCRIPT_NAME" "$*" >&2
}
die() { log ERROR "$*"; exit 1; }

# ---------------------------------------------------------------------------
# Preconditions
# ---------------------------------------------------------------------------
check_config() {
  local cmd missing=()
  for cmd in pg_dump gpg jq sha256sum; do
    command -v "$cmd" >/dev/null || missing+=("$cmd")
  done
  case "$BACKUP_PROVIDER" in
    s3)  command -v aws    >/dev/null || missing+=("aws-cli") ;;
    gcs) command -v gcloud >/dev/null || missing+=("gcloud") ;;
    *)   die "BACKUP_PROVIDER must be s3 or gcs (got '${BACKUP_PROVIDER}')" ;;
  esac
  (( ${#missing[@]} == 0 )) || die "missing required tools: ${missing[*]}"

  [[ -n "$BACKUP_BUCKET" ]]       || die "BACKUP_BUCKET is required"
  [[ -n "$KMS_KEY_ID" ]]          || die "KMS_KEY_ID is required"
  [[ -n "${PGDATABASE:-}" ]]      || die "PGDATABASE is required"
  [[ -r "$GPG_PUBLIC_KEY_FILE" ]] || die "GPG_PUBLIC_KEY_FILE is not readable"
  GPG_RECIPIENT_FINGERPRINT="${GPG_RECIPIENT_FINGERPRINT^^}"
  [[ "$GPG_RECIPIENT_FINGERPRINT" =~ ^[0-9A-F]{40}$ ]] \
    || die "GPG_RECIPIENT_FINGERPRINT must be a 40-hex-digit fingerprint"
}

# Refuse to write anywhere that is not WORM-locked for >= 7 years: a backup
# in a mutable bucket gives a false sense of safety.
verify_vault_lock() {
  local cfg
  case "$BACKUP_PROVIDER" in
    s3)
      cfg="$(aws s3api get-object-lock-configuration --bucket "$BACKUP_BUCKET" --output json)" \
        || die "cannot read Object Lock configuration of s3://${BACKUP_BUCKET}"
      jq -e --argjson min "$MIN_RETENTION_DAYS" '
        .ObjectLockConfiguration as $c
        | $c.ObjectLockEnabled == "Enabled"
          and $c.Rule.DefaultRetention.Mode == "COMPLIANCE"
          and ((($c.Rule.DefaultRetention.Years // 0) * 365)
               + ($c.Rule.DefaultRetention.Days // 0)) >= $min' <<<"$cfg" >/dev/null \
        || die "s3://${BACKUP_BUCKET} must have Object Lock default retention COMPLIANCE >= 7 years"
      ;;
    gcs)
      cfg="$(gcloud storage buckets describe "gs://${BACKUP_BUCKET}" --format=json)" \
        || die "cannot describe gs://${BACKUP_BUCKET}"
      jq -e --argjson min "$MIN_RETENTION_SECONDS" '
        (.retention_policy // .retentionPolicy // {}) as $p
        | $p.isLocked == true and (($p.retentionPeriod // "0") | tonumber) >= $min' \
        <<<"$cfg" >/dev/null \
        || die "gs://${BACKUP_BUCKET} must have a LOCKED retention policy >= 7 years"
      ;;
  esac
  log INFO "vault ${BACKUP_PROVIDER}://${BACKUP_BUCKET} is WORM-locked for >= ${MIN_RETENTION_DAYS} days"
}

# Ephemeral keyring holding only the pinned public key; a swapped key file
# fails the fingerprint check instead of silently encrypting to an attacker.
prepare_keyring() {
  export GNUPGHOME="${WORK_DIR}/gnupg"
  mkdir -m 700 "$GNUPGHOME"
  gpg --batch --quiet --import "$GPG_PUBLIC_KEY_FILE" 2>/dev/null \
    || die "failed to import ${GPG_PUBLIC_KEY_FILE}"
  gpg --batch --with-colons --fingerprint "$GPG_RECIPIENT_FINGERPRINT" 2>/dev/null \
    | awk -F: '$1 == "fpr" { print $10 }' | grep -qx "$GPG_RECIPIENT_FINGERPRINT" \
    || die "GPG_PUBLIC_KEY_FILE does not contain key ${GPG_RECIPIENT_FINGERPRINT}"
}

# ---------------------------------------------------------------------------
# Dump + encrypt (streamed; O(1) memory, O(n) disk for the ciphertext only)
# ---------------------------------------------------------------------------
dump_and_encrypt() {
  # -Fc is already compressed, so GPG compression would only burn CPU.
  pg_dump --format=custom --no-password \
    | gpg --batch --yes --no-tty --trust-model always \
          --recipient "$GPG_RECIPIENT_FINGERPRINT" --compress-algo none \
          --output "$ARCHIVE" --encrypt \
    || die "pg_dump | gpg pipeline failed"
  ( cd "$WORK_DIR" && sha256sum "$ARCHIVE_NAME" > "${ARCHIVE_NAME}.sha256" )
  log INFO "encrypted dump ${ARCHIVE_NAME} ($(du -h "$ARCHIVE" | cut -f1)), sha256 $(cut -d' ' -f1 "${ARCHIVE}.sha256")"
}

# ---------------------------------------------------------------------------
# Upload + immutability verification
# ---------------------------------------------------------------------------
upload() {
  local file name
  for file in "$ARCHIVE" "${ARCHIVE}.sha256"; do
    name="${OBJECT_KEY%/*}/$(basename "$file")"
    case "$BACKUP_PROVIDER" in
      s3)
        # Object Lock buckets require an integrity checksum on every PUT.
        aws s3 cp "$file" "s3://${BACKUP_BUCKET}/${name}" --no-progress \
          --sse aws:kms --sse-kms-key-id "$KMS_KEY_ID" --checksum-algorithm SHA256 >/dev/null
        ;;
      gcs)
        gcloud storage cp "$file" "gs://${BACKUP_BUCKET}/${name}" \
          --encryption-key="$KMS_KEY_ID" --no-clobber --quiet
        ;;
    esac || die "upload of ${name} failed"
  done
  log INFO "uploaded ${BACKUP_PROVIDER}://${BACKUP_BUCKET}/${OBJECT_KEY}"
}

# S3 applies the bucket default per object version; confirm it actually took
# effect. GCS enforces the locked bucket policy on every object by design.
verify_object_lock() {
  [[ "$BACKUP_PROVIDER" == "s3" ]] || return 0
  local head min_until
  head="$(aws s3api head-object --bucket "$BACKUP_BUCKET" --key "$OBJECT_KEY" --output json)" \
    || die "head-object failed for ${OBJECT_KEY}"
  min_until="$(( $(date -u +%s) + MIN_RETENTION_SECONDS - 86400 ))"   # 1 day clock slack
  jq -e --argjson min "$min_until" '
    .ObjectLockMode == "COMPLIANCE"
    and .ServerSideEncryption == "aws:kms"
    and ((.ObjectLockRetainUntilDate | sub("\\.[0-9]+"; "") | sub("\\+00:00$"; "Z")
          | fromdateiso8601) >= $min)' <<<"$head" >/dev/null \
    || die "${OBJECT_KEY} is not COMPLIANCE-locked with SSE-KMS for >= 7 years: $(jq -c \
         '{ObjectLockMode, ObjectLockRetainUntilDate, ServerSideEncryption}' <<<"$head")"
  log INFO "verified COMPLIANCE lock until $(jq -r .ObjectLockRetainUntilDate <<<"$head")"
}

main() {
  check_config
  verify_vault_lock

  WORK_DIR="$(mktemp -d "${BACKUP_WORK_DIR%/}/tessera-backup-XXXXXX")"
  trap 'rm -rf "$WORK_DIR"' EXIT
  ARCHIVE_NAME="${PGDATABASE}-$(date -u +%Y%m%dT%H%M%SZ).dump.gpg"
  ARCHIVE="${WORK_DIR}/${ARCHIVE_NAME}"
  OBJECT_KEY="${BACKUP_KEY_PREFIX%/}/${ARCHIVE_NAME}"

  prepare_keyring
  dump_and_encrypt
  upload
  verify_object_lock
  log INFO "backup complete: ${BACKUP_PROVIDER}://${BACKUP_BUCKET}/${OBJECT_KEY}"
}

main "$@"
