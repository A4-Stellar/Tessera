package tessera

import (
	"encoding/json"
	"time"
)

// Stats represents platform-wide aggregated metrics.
type Stats struct {
	TotalAssets        int     `json:"total_assets"`
	ActiveAssets       int     `json:"active_assets"`
	TvlCents           string  `json:"tvl_cents"`
	TvlUsd             float64 `json:"tvl_usd"`
	TotalHolders       int     `json:"total_holders"`
	TotalDistributions int     `json:"total_distributions"`
	LastIndexedLedger  uint32  `json:"last_indexed_ledger"`
	LastUpdated        *string `json:"last_updated"`
}

// Asset represents a tokenized real-world asset indexed from Stellar.
type Asset struct {
	ID                 uint64  `json:"id"`
	TokenContract      string  `json:"token_contract"`
	Issuer             string  `json:"issuer"`
	Name               string  `json:"name"`
	Symbol             string  `json:"symbol"`
	AssetType          string  `json:"asset_type"`
	Description        string  `json:"description"`
	ValuationCents     string  `json:"valuation_cents"`
	ValuationUsd       float64 `json:"valuation_usd"`
	Decimals           uint32  `json:"decimals"`
	TotalSupply        string  `json:"total_supply"`
	Holders            int     `json:"holders"`
	Active             bool    `json:"active"`
	Paused             bool    `json:"paused"`
	ComplianceContract string  `json:"compliance_contract"`
	CreatedAtLedger    uint32  `json:"created_at_ledger"`
	IndexedAtLedger    uint32  `json:"indexed_at_ledger"`
	IndexError         *string `json:"index_error,omitempty"`
}

// Holder represents a single token holder for an asset.
type Holder struct {
	Address      string  `json:"address"`
	Balance      string  `json:"balance"`
	SharePercent float64 `json:"share_percent"`
}

// AddressHolding represents a single asset held by an investor address.
type AddressHolding struct {
	Address      string  `json:"address"`
	AssetID      uint64  `json:"asset_id"`
	AssetName    string  `json:"asset_name"`
	Symbol       string  `json:"symbol"`
	Balance      string  `json:"balance"`
	SharePercent float64 `json:"share_percent"`
}

// AddressCompliance represents compliance status for an address within an asset.
type AddressCompliance struct {
	Address   string `json:"address"`
	AssetID   uint64 `json:"asset_id"`
	AssetName string `json:"asset_name"`
	Symbol    string `json:"symbol"`
	Balance   string `json:"balance"`
	Status    string `json:"status"`
	Allowed   bool   `json:"allowed"`
}

// ComplianceSummary represents an aggregate summary of an asset's allowlist.
type ComplianceSummary struct {
	TotalRecords  int                 `json:"total_records"`
	Approved      int                 `json:"approved"`
	Suspended     int                 `json:"suspended"`
	Rejected      int                 `json:"rejected"`
	Pending       int                 `json:"pending"`
	WithExpiry    int                 `json:"with_expiry"`
	Jurisdictions []JurisdictionCount `json:"jurisdictions"`
}

// JurisdictionCount tracks record distribution across countries.
type JurisdictionCount struct {
	Jurisdiction string `json:"jurisdiction"`
	Count        int    `json:"count"`
}

// Distribution represents a dividend distribution pool for an asset.
type Distribution struct {
	ID                uint64   `json:"id"`
	AssetToken        string   `json:"asset_token"`
	PaymentToken      string   `json:"payment_token"`
	TotalAmount       string   `json:"total_amount"`
	Distributed       string   `json:"distributed"`
	ClaimedPercent    float64  `json:"claimed_percent"`
	OverflowDetected  bool     `json:"overflow_detected"`
	Completed         bool     `json:"completed"`
	CreatedAtLedger   uint32   `json:"created_at_ledger"`
	FiatEquivalentUsd *float64 `json:"fiat_equivalent_usd,omitempty"`
}

// Event represents an indexed contract event.
type Event struct {
	ID        uint64          `json:"id"`
	Contract  string          `json:"contract"`
	EventType string          `json:"event_type"`
	Ledger    uint32          `json:"ledger"`
	Timestamp *string         `json:"timestamp,omitempty"`
	Data      json.RawMessage `json:"data"`
}

// AssetAnalytics represents holder concentration and transaction velocity metrics.
type AssetAnalytics struct {
	AssetID                    uint64   `json:"asset_id"`
	HolderCount                int      `json:"holder_count"`
	GiniCoefficient            *float64 `json:"gini_coefficient,omitempty"`
	HerfindahlHirschmanIndex   *float64 `json:"herfindahl_hirschman_index,omitempty"`
	Volume24h                  string   `json:"volume_24h"`
	TotalSupply                string   `json:"total_supply"`
	Velocity24h                *float64 `json:"velocity_24h,omitempty"`
	WindowStart                string   `json:"window_start"`
	WindowEnd                  string   `json:"window_end"`
}

// HolderPortfolioResponse represents the portfolio analytics for an investor address.
type HolderPortfolioResponse struct {
	Address     string                     `json:"address"`
	Summary     PortfolioSummary           `json:"summary"`
	Holdings    []PortfolioHoldingDetail   `json:"holdings"`
	Dividends   []PortfolioDividendPayout  `json:"dividends"`
	Performance PortfolioPerformance       `json:"performance"`
	TimeSeries  []PortfolioTimeSeriesPoint `json:"time_series"`
}

// PortfolioSummary represents aggregate valuation and flow metrics.
type PortfolioSummary struct {
	TotalValuationUsd     float64 `json:"total_valuation_usd"`
	TotalAssetsHeld       int     `json:"total_assets_held"`
	TotalCashInflowsUsd   float64 `json:"total_cash_inflows_usd"`
	TotalCashOutflowsUsd  float64 `json:"total_cash_outflows_usd"`
	NetInvestedCapitalUsd float64 `json:"net_invested_capital_usd"`
	TotalDividendsUsd     float64 `json:"total_dividends_usd"`
	TotalUnrealizedGainUsd float64 `json:"total_unrealized_gain_usd"`
}

// PortfolioHoldingDetail represents an asset holding breakdown.
type PortfolioHoldingDetail struct {
	AssetID              uint64  `json:"asset_id"`
	AssetName            string  `json:"asset_name"`
	Symbol               string  `json:"symbol"`
	AssetType            string  `json:"asset_type"`
	Balance              string  `json:"balance"`
	Decimals             uint32  `json:"decimals"`
	SharePercent         float64 `json:"share_percent"`
	UnitPriceUsd         float64 `json:"unit_price_usd"`
	MarketValueUsd       float64 `json:"market_value_usd"`
	DividendsReceivedUsd float64 `json:"dividends_received_usd"`
}

// PortfolioDividendPayout represents an attributed dividend distribution.
type PortfolioDividendPayout struct {
	DistributionID   uint64  `json:"distribution_id"`
	AssetID          uint64  `json:"asset_id"`
	PaymentToken     string  `json:"payment_token"`
	Amount           string  `json:"amount"`
	AmountUsd        float64 `json:"amount_usd"`
	ClaimedAtLedger  uint32  `json:"claimed_at_ledger"`
	Timestamp        *string `json:"timestamp,omitempty"`
}

// PortfolioPerformance represents rate of return metrics.
type PortfolioPerformance struct {
	TimeWeightedReturn       float64  `json:"time_weighted_return"`
	TimeWeightedReturnPct    float64  `json:"time_weighted_return_pct"`
	NetPresentValue          float64  `json:"net_present_value"`
	InternalRateOfReturn     *float64 `json:"internal_rate_of_return,omitempty"`
	InternalRateOfReturnPct  *float64 `json:"internal_rate_of_return_pct,omitempty"`
	DiscountRate             float64  `json:"discount_rate"`
}

// PortfolioTimeSeriesPoint represents a chronological point for charting.
type PortfolioTimeSeriesPoint struct {
	Timestamp             string  `json:"timestamp"`
	Ledger                uint32  `json:"ledger"`
	PortfolioValueUsd     float64 `json:"portfolio_value_usd"`
	CashFlowUsd           float64 `json:"cash_flow_usd"`
	CumulativeInvestedUsd float64 `json:"cumulative_invested_usd"`
	CumulativeDividendsUsd float64 `json:"cumulative_dividends_usd"`
	PeriodReturn          float64 `json:"period_return"`
	CumulativeTwrPct      float64 `json:"cumulative_twr_pct"`
}

// ListAssetsParams encapsulates query parameters for listing assets.
type ListAssetsParams struct {
	AssetType *string
	Active    *bool
	Offset    *int
	Limit     *int
}

// PaginationParams encapsulates standard offset/limit pagination.
type PaginationParams struct {
	Offset *int
	Limit  *int
}

// PortfolioQueryParams encapsulates optional parameters for portfolio calculation.
type PortfolioQueryParams struct {
	StartDate    *time.Time
	EndDate      *time.Time
	DiscountRate *float64
}

// ApiErrorBody represents the JSON error payload from the Tessera API.
type ApiErrorBody struct {
	Error   string `json:"error"`
	Message string `json:"message"`
}
