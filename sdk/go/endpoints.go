package tessera

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"time"
)

func decode[T any](body []byte, name string) (T, error) {
	var value T
	if err := json.Unmarshal(body, &value); err != nil {
		return value, fmt.Errorf("failed to decode %s: %w", name, err)
	}
	return value, nil
}

// GetStats retrieves global platform metrics and TVL.
func (c *Client) GetStats(ctx context.Context) (*Stats, error) {
	bytes, err := c.executeRequest(ctx, http.MethodGet, "/v1/stats", nil, nil)
	if err != nil {
		return nil, err
	}

	var stats Stats
	if err := json.Unmarshal(bytes, &stats); err != nil {
		return nil, fmt.Errorf("failed to decode stats: %w", err)
	}
	return &stats, nil
}

// ListAssets queries indexed tokenized real-world assets.
func (c *Client) ListAssets(ctx context.Context, params *ListAssetsParams) ([]Asset, error) {
	query := url.Values{}
	if params != nil {
		if params.AssetType != nil {
			query.Set("asset_type", *params.AssetType)
		}
		if params.Active != nil {
			query.Set("active", strconv.FormatBool(*params.Active))
		}
		if params.Offset != nil {
			query.Set("offset", strconv.Itoa(*params.Offset))
		}
		if params.Limit != nil {
			query.Set("limit", strconv.Itoa(*params.Limit))
		}
		if params.Fields != "" {
			query.Set("fields", params.Fields)
		}
	}

	bytes, err := c.executeRequest(ctx, http.MethodGet, "/v1/assets", query, nil)
	if err != nil {
		return nil, err
	}

	var assets []Asset
	if err := json.Unmarshal(bytes, &assets); err != nil {
		return nil, fmt.Errorf("failed to decode assets: %w", err)
	}
	return assets, nil
}

// GetAsset fetches a single tokenized asset by ID.
func (c *Client) GetAsset(ctx context.Context, id uint64) (*Asset, error) {
	endpoint := fmt.Sprintf("/v1/assets/%d", id)
	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, nil, nil)
	if err != nil {
		return nil, err
	}

	var asset Asset
	if err := json.Unmarshal(bytes, &asset); err != nil {
		return nil, fmt.Errorf("failed to decode asset: %w", err)
	}
	return &asset, nil
}

// GetAssetWithFields fetches an asset while optionally restricting its fields.
func (c *Client) GetAssetWithFields(ctx context.Context, id uint64, fields string) (*Asset, error) {
	query := url.Values{}
	if fields != "" {
		query.Set("fields", fields)
	}
	bytes, err := c.executeRequest(ctx, http.MethodGet, fmt.Sprintf("/v1/assets/%d", id), query, nil)
	if err != nil {
		return nil, err
	}
	var asset Asset
	if err := json.Unmarshal(bytes, &asset); err != nil {
		return nil, fmt.Errorf("failed to decode asset: %w", err)
	}
	return &asset, nil
}

// GetAssetHolders fetches the holder list for an asset, sorted by balance descending.
func (c *Client) GetAssetHolders(ctx context.Context, id uint64, params *PaginationParams) ([]Holder, error) {
	endpoint := fmt.Sprintf("/v1/assets/%d/holders", id)
	query := url.Values{}
	if params != nil {
		if params.Offset != nil {
			query.Set("offset", strconv.Itoa(*params.Offset))
		}
		if params.Limit != nil {
			query.Set("limit", strconv.Itoa(*params.Limit))
		}
	}

	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, query, nil)
	if err != nil {
		return nil, err
	}

	var holders []Holder
	if err := json.Unmarshal(bytes, &holders); err != nil {
		return nil, fmt.Errorf("failed to decode holders: %w", err)
	}
	return holders, nil
}

// GetAssetCompliance returns aggregate compliance and jurisdiction distribution for an asset.
func (c *Client) GetAssetCompliance(ctx context.Context, id uint64) (*ComplianceSummary, error) {
	endpoint := fmt.Sprintf("/v1/assets/%d/compliance", id)
	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, nil, nil)
	if err != nil {
		return nil, err
	}

	var summary ComplianceSummary
	if err := json.Unmarshal(bytes, &summary); err != nil {
		return nil, fmt.Errorf("failed to decode compliance summary: %w", err)
	}
	return &summary, nil
}

// GetAssetDividends lists all dividend distributions for an asset.
func (c *Client) GetAssetDividends(ctx context.Context, id uint64) ([]Distribution, error) {
	endpoint := fmt.Sprintf("/v1/assets/%d/dividends", id)
	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, nil, nil)
	if err != nil {
		return nil, err
	}

	var distributions []Distribution
	if err := json.Unmarshal(bytes, &distributions); err != nil {
		return nil, fmt.Errorf("failed to decode dividends: %w", err)
	}
	return distributions, nil
}

// GetAssetAnalytics returns holder concentration (Gini, HHI) and 24-hour transaction velocity.
func (c *Client) GetAssetAnalytics(ctx context.Context, id uint64) (*AssetAnalytics, error) {
	endpoint := fmt.Sprintf("/v1/assets/%d/analytics", id)
	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, nil, nil)
	if err != nil {
		return nil, err
	}

	var analytics AssetAnalytics
	if err := json.Unmarshal(bytes, &analytics); err != nil {
		return nil, fmt.Errorf("failed to decode analytics: %w", err)
	}
	return &analytics, nil
}

// GetHolder returns the portfolio of assets held by an address.
func (c *Client) GetHolder(ctx context.Context, address string) ([]AddressHolding, error) {
	endpoint := fmt.Sprintf("/v1/holders/%s", url.PathEscape(address))
	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, nil, nil)
	if err != nil {
		return nil, err
	}

	var holdings []AddressHolding
	if err := json.Unmarshal(bytes, &holdings); err != nil {
		return nil, fmt.Errorf("failed to decode address holdings: %w", err)
	}
	return holdings, nil
}

// GetHolderCompliance returns compliance status for an address across all its held assets.
func (c *Client) GetHolderCompliance(ctx context.Context, address string) ([]AddressCompliance, error) {
	endpoint := fmt.Sprintf("/v1/holders/%s/compliance", url.PathEscape(address))
	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, nil, nil)
	if err != nil {
		return nil, err
	}

	var compliance []AddressCompliance
	if err := json.Unmarshal(bytes, &compliance); err != nil {
		return nil, fmt.Errorf("failed to decode address compliance: %w", err)
	}
	return compliance, nil
}

// GetHolderPortfolio computes and returns historical portfolio performance metrics (TWR, NPV, IRR, time series).
func (c *Client) GetHolderPortfolio(ctx context.Context, address string, params *PortfolioQueryParams) (*HolderPortfolioResponse, error) {
	endpoint := fmt.Sprintf("/v1/holders/%s/portfolio", url.PathEscape(address))
	query := url.Values{}
	if params != nil {
		if params.StartDate != nil {
			query.Set("start_date", params.StartDate.Format(time.RFC3339))
		}
		if params.EndDate != nil {
			query.Set("end_date", params.EndDate.Format(time.RFC3339))
		}
		if params.DiscountRate != nil {
			query.Set("discount_rate", strconv.FormatFloat(*params.DiscountRate, 'f', -1, 64))
		}
	}

	bytes, err := c.executeRequest(ctx, http.MethodGet, endpoint, query, nil)
	if err != nil {
		return nil, err
	}

	var portfolio HolderPortfolioResponse
	if err := json.Unmarshal(bytes, &portfolio); err != nil {
		return nil, fmt.Errorf("failed to decode holder portfolio: %w", err)
	}
	return &portfolio, nil
}

// ListEvents retrieves recent indexed contract events.
func (c *Client) ListEvents(ctx context.Context) ([]Event, error) {
	bytes, err := c.executeRequest(ctx, http.MethodGet, "/v1/events", nil, nil)
	if err != nil {
		return nil, err
	}

	var events []Event
	if err := json.Unmarshal(bytes, &events); err != nil {
		return nil, fmt.Errorf("failed to decode events: %w", err)
	}
	return events, nil
}

// GetAssetEvents returns diagnostic events captured for an asset.
func (c *Client) GetAssetEvents(ctx context.Context, id uint64, includeDiagnostics bool) (*AssetEventsResponse, error) {
	query := url.Values{}
	if includeDiagnostics {
		query.Set("include_diagnostics", "true")
	}
	body, err := c.executeRequest(ctx, http.MethodGet, fmt.Sprintf("/v1/assets/%d/events", id), query, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[AssetEventsResponse](body, "asset events")
	return &result, err
}

// GetAssetMetricsAnalytics returns sliding/tumbling window analytics as JSON.
func (c *Client) GetAssetMetricsAnalytics(ctx context.Context, id uint64, params *AssetAnalyticsQuery) (json.RawMessage, error) {
	query := url.Values{}
	if params != nil {
		if params.Window != "" {
			query.Set("window", params.Window)
		}
		if params.WindowType != "" {
			query.Set("window_type", params.WindowType)
		}
		if params.Limit != nil {
			query.Set("limit", strconv.Itoa(*params.Limit))
		}
	}
	return c.executeRequest(ctx, http.MethodGet, fmt.Sprintf("/v1/assets/%d/metrics/analytics", id), query, nil)
}

// GetAssetDistribution fetches one dividend distribution.
func (c *Client) GetAssetDistribution(ctx context.Context, id, distributionID uint64) (*Distribution, error) {
	body, err := c.executeRequest(ctx, http.MethodGet, fmt.Sprintf("/v1/assets/%d/distributions/%d", id, distributionID), nil, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[Distribution](body, "distribution")
	return &result, err
}

// ExportAsset downloads an asset cap-table in CSV or Parquet format.
func (c *Client) ExportAsset(ctx context.Context, id uint64, format string, ledger *uint32) ([]byte, error) {
	query := url.Values{}
	if format != "" {
		query.Set("format", format)
	}
	if ledger != nil {
		query.Set("ledger", strconv.FormatUint(uint64(*ledger), 10))
	}
	return c.executeRequest(ctx, http.MethodGet, fmt.Sprintf("/v1/assets/%d/export", id), query, nil)
}

// GetCompliance returns compliance records for an address.
func (c *Client) GetCompliance(ctx context.Context, address string) ([]AddressCompliance, error) {
	body, err := c.executeRequest(ctx, http.MethodGet, "/v1/compliance/"+url.PathEscape(address), nil, nil)
	if err != nil {
		return nil, err
	}
	return decode[[]AddressCompliance](body, "compliance")
}

// GetSecurityAnomalies lists active anomaly flags.
func (c *Client) GetSecurityAnomalies(ctx context.Context, account string, minZ *float64) (*AnomaliesResponse, error) {
	query := url.Values{}
	if account != "" {
		query.Set("account", account)
	}
	if minZ != nil {
		query.Set("min_z", strconv.FormatFloat(*minZ, 'f', -1, 64))
	}
	body, err := c.executeRequest(ctx, http.MethodGet, "/v1/security/anomalies", query, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[AnomaliesResponse](body, "security anomalies")
	return &result, err
}

// VerifyAudit verifies the append-only audit log.
func (c *Client) VerifyAudit(ctx context.Context, entrySequence *uint64) (*AuditVerificationResult, error) {
	query := url.Values{}
	if entrySequence != nil {
		query.Set("entry_seq", strconv.FormatUint(*entrySequence, 10))
	}
	body, err := c.executeRequest(ctx, http.MethodGet, "/v1/audit/verify", query, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[AuditVerificationResult](body, "audit verification")
	return &result, err
}

// ListAuditEntries returns a page of audit log entries.
func (c *Client) ListAuditEntries(ctx context.Context, params *PaginationParams) (*PaginatedEntriesResponse, error) {
	query := url.Values{}
	if params != nil {
		if params.Offset != nil {
			query.Set("offset", strconv.Itoa(*params.Offset))
		}
		if params.Limit != nil {
			query.Set("limit", strconv.Itoa(*params.Limit))
		}
	}
	body, err := c.executeRequest(ctx, http.MethodGet, "/v1/audit/entries", query, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[PaginatedEntriesResponse](body, "audit entries")
	return &result, err
}

// GetAuditEntry fetches an audit entry and its inclusion proof.
func (c *Client) GetAuditEntry(ctx context.Context, sequence uint64) (*EntryDetailResponse, error) {
	body, err := c.executeRequest(ctx, http.MethodGet, fmt.Sprintf("/v1/audit/entries/%d", sequence), nil, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[EntryDetailResponse](body, "audit entry")
	return &result, err
}

// ListAuditAnchors returns published audit root anchors.
func (c *Client) ListAuditAnchors(ctx context.Context) ([]AnchorRecord, error) {
	body, err := c.executeRequest(ctx, http.MethodGet, "/v1/audit/anchors", nil, nil)
	if err != nil {
		return nil, err
	}
	return decode[[]AnchorRecord](body, "audit anchors")
}

// CreateAuditEntry appends an administrative action to the audit log.
func (c *Client) CreateAuditEntry(ctx context.Context, request CreateAuditEntryRequest) (*AuditEntry, error) {
	body, err := json.Marshal(request)
	if err != nil {
		return nil, fmt.Errorf("failed to encode audit entry: %w", err)
	}
	response, err := c.executeRequest(ctx, http.MethodPost, "/v1/audit/entries", nil, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	result, err := decode[AuditEntry](response, "audit entry")
	return &result, err
}

// PublishAuditAnchor publishes the current audit root to the Stellar ledger.
func (c *Client) PublishAuditAnchor(ctx context.Context, request PublishAnchorRequest) (*AnchorRecord, error) {
	body, err := json.Marshal(request)
	if err != nil {
		return nil, fmt.Errorf("failed to encode anchor request: %w", err)
	}
	response, err := c.executeRequest(ctx, http.MethodPost, "/v1/audit/anchor", nil, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	result, err := decode[AnchorRecord](response, "audit anchor")
	return &result, err
}

// RetryDeadLetterQueue asks the API to reprocess quarantined events.
func (c *Client) RetryDeadLetterQueue(ctx context.Context) (*RetryReport, error) {
	body, err := c.executeRequest(ctx, http.MethodPost, "/v1/admin/dlq/retry", nil, nil)
	if err != nil {
		return nil, err
	}
	result, err := decode[RetryReport](body, "dead-letter retry report")
	return &result, err
}

// GetIndex returns the service metadata document.
func (c *Client) GetIndex(ctx context.Context) (json.RawMessage, error) {
	return c.executeRequest(ctx, http.MethodGet, "/", nil, nil)
}

// GetVersion returns the service version document.
func (c *Client) GetVersion(ctx context.Context) (json.RawMessage, error) {
	return c.executeRequest(ctx, http.MethodGet, "/version", nil, nil)
}

// Health returns the service health document.
func (c *Client) Health(ctx context.Context) (json.RawMessage, error) {
	return c.executeRequest(ctx, http.MethodGet, "/health", nil, nil)
}

// Metrics returns the Prometheus metrics document.
func (c *Client) Metrics(ctx context.Context) ([]byte, error) {
	return c.executeRequest(ctx, http.MethodGet, "/metrics", nil, nil)
}
