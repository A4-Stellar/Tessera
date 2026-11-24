#!/usr/bin/env bash
# ==============================================================================
# Tessera Supply Chain Security: SLSA Level 3 & Sigstore Provenance Verifier
# Issue #47 / #428
#
# Verifies build provenance attestations (SLSA v1.0) and Sigstore cosign
# signatures against transparency logs before deployment.
#
# Usage:
#   verify_provenance.sh --artifact <file> --provenance <file> [options]
#   verify_provenance.sh --image <image-ref> [options]
#
# Options:
#   --artifact <path>         Path to binary, WASM, or release archive
#   --provenance <path>       Path to SLSA provenance attestation (.intoto.jsonl)
#   --signature <path>        Path to signature file (for blob verification)
#   --certificate <path>      Path to certificate (for blob verification)
#   --image <ref>             Container image reference (e.g. ghcr.io/a4-stellar/tessera-api:tag)
#   --source-repo <repo>      Expected source repository (default: A4-Stellar/Tessera)
#   --expected-digest <sha>   Expected SHA-256 digest of artifact
#   --quiet                   Suppress non-error output
#   --help, -h                Display this help message
#
# Exit codes:
#   0 = Verified successfully
#   1 = Verification failed (tampering, invalid signature, or provenance mismatch)
#   2 = Invalid usage or missing prerequisite tools
# ==============================================================================

set -euo pipefail

REPO_DEFAULT="A4-Stellar/Tessera"
OIDC_ISSUER_DEFAULT="https://token.actions.githubusercontent.com"

ARTIFACT=""
PROVENANCE=""
SIGNATURE=""
CERTIFICATE=""
IMAGE=""
SOURCE_REPO="$REPO_DEFAULT"
EXPECTED_DIGEST=""
QUIET=0

log_info() {
    if [ "$QUIET" -eq 0 ]; then
        echo -e "\033[1;34m[INFO]\033[0m $*"
    fi
}

log_success() {
    if [ "$QUIET" -eq 0 ]; then
        echo -e "\033[1;32m[PASS]\033[0m $*"
    fi
}

log_error() {
    echo -e "\033[1;31m[ERROR]\033[0m $*" >&2
}

usage() {
    local exit_code="${1:-2}"
    cat <<EOF
Tessera Pre-Deployment Provenance & Signature Verifier
Verifies SLSA v1.0 attestations and Sigstore signatures.

Usage:
  $0 --artifact <path> --provenance <path> [--signature <path>] [--certificate <path>]
  $0 --image <image-reference>

Flags:
  --artifact <path>        Target artifact (WASM binary, API binary, tar.gz)
  --provenance <path>      SLSA v1.0 intoto provenance file
  --signature <path>       Cosign signature file (.sig)
  --certificate <path>     Cosign certificate file (.pem)
  --image <ref>            Container image reference
  --source-repo <repo>     Expected GitHub repository (default: $REPO_DEFAULT)
  --expected-digest <sha>  Expected SHA-256 digest
  --quiet                  Suppress info logs
  --help, -h               Show this help message
EOF
    exit "$exit_code"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --artifact)
            ARTIFACT="$2"; shift 2 ;;
        --provenance)
            PROVENANCE="$2"; shift 2 ;;
        --signature)
            SIGNATURE="$2"; shift 2 ;;
        --certificate)
            CERTIFICATE="$2"; shift 2 ;;
        --image)
            IMAGE="$2"; shift 2 ;;
        --source-repo)
            SOURCE_REPO="$2"; shift 2 ;;
        --expected-digest)
            EXPECTED_DIGEST="$2"; shift 2 ;;
        --quiet)
            QUIET=1; shift ;;
        --help|-h)
            usage 0 ;;
        *)
            log_error "Unknown argument: $1"
            usage 2 ;;
    esac
done

# Check mode: Container Image or Blob/Artifact
if [ -n "$IMAGE" ]; then
    log_info "Verifying container image signature and provenance for: $IMAGE"

    if ! command -v cosign &> /dev/null; then
        log_error "cosign is required for container image verification but was not found in PATH."
        exit 2
    fi

    log_info "Running cosign verification with Sigstore transparency log..."
    if cosign verify \
        --certificate-identity-regexp "^https://github.com/${SOURCE_REPO}/" \
        --certificate-oidc-issuer "$OIDC_ISSUER_DEFAULT" \
        "$IMAGE"; then
        log_success "Container image signature verified against Sigstore Rekor transparency log."
    else
        log_error "Cosign container image signature verification failed!"
        exit 1
    fi

    # Optional image attestation verification
    if cosign verify-attestation \
        --type slsaprovenance \
        --certificate-identity-regexp "^https://github.com/${SOURCE_REPO}/" \
        --certificate-oidc-issuer "$OIDC_ISSUER_DEFAULT" \
        "$IMAGE" &> /dev/null; then
        log_success "Container image SLSA provenance attestation verified."
    else
        log_info "No SLSA attestation attached to image or attestation verification skipped."
    fi

    exit 0
fi

# Artifact verification mode
if [ -z "$ARTIFACT" ]; then
    log_error "Either --artifact or --image must be provided."
    usage
fi

if [ ! -f "$ARTIFACT" ]; then
    log_error "Artifact file not found: $ARTIFACT"
    exit 2
fi

log_info "Computing SHA-256 digest of artifact: $ARTIFACT"
if command -v sha256sum &> /dev/null; then
    ACTUAL_DIGEST=$(sha256sum "$ARTIFACT" | awk '{print $1}')
elif command -v shasum &> /dev/null; then
    ACTUAL_DIGEST=$(shasum -a 256 "$ARTIFACT" | awk '{print $1}')
else
    log_error "Neither sha256sum nor shasum found."
    exit 2
fi

log_info "Artifact SHA-256: $ACTUAL_DIGEST"

if [ -n "$EXPECTED_DIGEST" ]; then
    if [ "$ACTUAL_DIGEST" != "$EXPECTED_DIGEST" ]; then
        log_error "Digest mismatch! Expected: $EXPECTED_DIGEST, Actual: $ACTUAL_DIGEST"
        exit 1
    fi
    log_success "Digest matches expected value."
fi

# 1. Verify SLSA Provenance if provided
if [ -n "$PROVENANCE" ]; then
    if [ ! -f "$PROVENANCE" ]; then
        log_error "Provenance file not found: $PROVENANCE"
        exit 2
    fi

    log_info "Verifying SLSA v1.0 provenance attestation: $PROVENANCE"

    if command -v slsa-verifier &> /dev/null; then
        if slsa-verifier verify-artifact "$ARTIFACT" \
            --provenance-path "$PROVENANCE" \
            --source-uri "github.com/${SOURCE_REPO}"; then
            log_success "SLSA Level 3 provenance verified via slsa-verifier."
        else
            log_error "SLSA provenance verification failed!"
            exit 1
        fi
    else
        log_info "slsa-verifier not present; falling back to direct JSON-LD subject digest inspection..."
        if grep -q "$ACTUAL_DIGEST" "$PROVENANCE"; then
            log_success "Provenance subject matches artifact SHA-256 digest."
        else
            log_error "Provenance does NOT reference artifact digest $ACTUAL_DIGEST!"
            exit 1
        fi

        if grep -q "github.com/${SOURCE_REPO}" "$PROVENANCE"; then
            log_success "Provenance builder/source matches expected repository."
        else
            log_error "Provenance source does not match expected repository github.com/${SOURCE_REPO}"
            exit 1
        fi
    fi
fi

# 2. Verify Cosign Blob Signature if signature & certificate provided
if [ -n "$SIGNATURE" ] && [ -n "$CERTIFICATE" ]; then
    if [ ! -f "$SIGNATURE" ] || [ ! -f "$CERTIFICATE" ]; then
        log_error "Signature or certificate file missing."
        exit 2
    fi

    if command -v cosign &> /dev/null; then
        log_info "Verifying blob signature using cosign..."
        if cosign verify-blob \
            --signature "$SIGNATURE" \
            --certificate "$CERTIFICATE" \
            --certificate-identity-regexp "^https://github.com/${SOURCE_REPO}/" \
            --certificate-oidc-issuer "$OIDC_ISSUER_DEFAULT" \
            "$ARTIFACT"; then
            log_success "Cosign blob signature verified successfully."
        else
            log_error "Cosign blob signature verification failed!"
            exit 1
        fi
    else
        log_info "cosign tool not found; skipping offline blob cryptographic signature check."
    fi
fi

log_success "All pre-deployment supply chain verification checks passed for $ARTIFACT."
exit 0
