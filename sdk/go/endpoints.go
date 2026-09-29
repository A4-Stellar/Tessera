package tessera

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"time"
)

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
