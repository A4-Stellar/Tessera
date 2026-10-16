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
	AssetID                  uint64   `json:"asset_id"`
	HolderCount              int      `json:"holder_count"`
	GiniCoefficient          *float64 `json:"gini_coefficient,omitempty"`
	HerfindahlHirschmanIndex *float64 `json:"herfindahl_hirschman_index,omitempty"`
	Volume24h                string   `json:"volume_24h"`
	TotalSupply              string   `json:"total_supply"`
	Velocity24h              *float64 `json:"velocity_24h,omitempty"`
	WindowStart              string   `json:"window_start"`
	WindowEnd                string   `json:"window_end"`
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
	TotalValuationUsd      float64 `json:"total_valuation_usd"`
	TotalAssetsHeld        int     `json:"total_assets_held"`
	TotalCashInflowsUsd    float64 `json:"total_cash_inflows_usd"`
	TotalCashOutflowsUsd   float64 `json:"total_cash_outflows_usd"`
	NetInvestedCapitalUsd  float64 `json:"net_invested_capital_usd"`
	TotalDividendsUsd      float64 `json:"total_dividends_usd"`
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
	DistributionID  uint64  `json:"distribution_id"`
	AssetID         uint64  `json:"asset_id"`
	PaymentToken    string  `json:"payment_token"`
	Amount          string  `json:"amount"`
	AmountUsd       float64 `json:"amount_usd"`
	ClaimedAtLedger uint32  `json:"claimed_at_ledger"`
	Timestamp       *string `json:"timestamp,omitempty"`
}

// PortfolioPerformance represents rate of return metrics.
type PortfolioPerformance struct {
	TimeWeightedReturn      float64  `json:"time_weighted_return"`
	TimeWeightedReturnPct   float64  `json:"time_weighted_return_pct"`
	NetPresentValue         float64  `json:"net_present_value"`
	InternalRateOfReturn    *float64 `json:"internal_rate_of_return,omitempty"`
	InternalRateOfReturnPct *float64 `json:"internal_rate_of_return_pct,omitempty"`
	DiscountRate            float64  `json:"discount_rate"`
}

// PortfolioTimeSeriesPoint represents a chronological point for charting.
type PortfolioTimeSeriesPoint struct {
	Timestamp              string  `json:"timestamp"`
	Ledger                 uint32  `json:"ledger"`
	PortfolioValueUsd      float64 `json:"portfolio_value_usd"`
	CashFlowUsd            float64 `json:"cash_flow_usd"`
	CumulativeInvestedUsd  float64 `json:"cumulative_invested_usd"`
	CumulativeDividendsUsd float64 `json:"cumulative_dividends_usd"`
	PeriodReturn           float64 `json:"period_return"`
	CumulativeTwrPct       float64 `json:"cumulative_twr_pct"`
}

// ListAssetsParams encapsulates query parameters for listing assets.
type ListAssetsParams struct {
	AssetType *string
	Active    *bool
	Offset    *int
	Limit     *int
	Fields    string
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

// AssetEventsResponse is the response from the asset diagnostic event endpoint.
type AssetEventsResponse struct {
	AssetID     uint64            `json:"asset_id"`
	Diagnostics []DiagnosticEvent `json:"diagnostics"`
}

// DiagnosticEvent is a structured Soroban diagnostic event.
type DiagnosticEvent struct {
	Contract                 *string           `json:"contract"`
	EventType                string            `json:"event_type"`
	Topics                   []json.RawMessage `json:"topics"`
	Data                     json.RawMessage   `json:"data"`
	InSuccessfulContractCall bool              `json:"in_successful_contract_call"`
	ErrorCode                *uint32           `json:"error_code"`
}

// AssetAnalyticsQuery controls the pre-computed analytics endpoint.
type AssetAnalyticsQuery struct {
	Window     string
	WindowType string
	Limit      *int
}

type AnomaliesResponse struct {
	Threshold float64    `json:"threshold"`
	Count     int        `json:"count"`
	Flags     []RiskFlag `json:"flags"`
}

type RiskFlag struct {
	Account      string  `json:"account"`
	Metric       string  `json:"metric"`
	PeakZScore   float64 `json:"peak_z_score"`
	LatestZScore float64 `json:"latest_z_score"`
	Observed     float64 `json:"observed"`
	BaselineMean float64 `json:"baseline_mean"`
	BaselineStd  float64 `json:"baseline_std"`
	FirstSeen    uint64  `json:"first_seen"`
	LastSeen     uint64  `json:"last_seen"`
	TriggerCount uint64  `json:"trigger_count"`
}

type PaginatedEntriesResponse struct {
	Entries []AuditEntry `json:"entries"`
	Total   int          `json:"total"`
	Offset  int          `json:"offset"`
	Limit   int          `json:"limit"`
}

type AuditEntry struct {
	Sequence  uint64          `json:"sequence"`
	Timestamp time.Time       `json:"timestamp"`
	Action    json.RawMessage `json:"action"`
	Actor     string          `json:"actor"`
	Target    *string         `json:"target"`
	Payload   json.RawMessage `json:"payload"`
	PrevHash  string          `json:"prev_hash"`
	EntryHash string          `json:"entry_hash"`
}

type EntryDetailResponse struct {
	Entry          AuditEntry   `json:"entry"`
	InclusionProof *MerkleProof `json:"inclusion_proof"`
}

type MerkleProof struct {
	LeafIndex uint64      `json:"leaf_index"`
	LeafHash  string      `json:"leaf_hash"`
	Steps     []ProofStep `json:"steps"`
	RootHash  string      `json:"root_hash"`
}
type ProofStep struct {
	SiblingHash   string `json:"sibling_hash"`
	IsSiblingLeft bool   `json:"is_sibling_left"`
}

type AnchorRecord struct {
	AnchorID           uint64    `json:"anchor_id"`
	RootHash           string    `json:"root_hash"`
	StartSequence      uint64    `json:"start_sequence"`
	EndSequence        uint64    `json:"end_sequence"`
	EntryCount         int       `json:"entry_count"`
	LedgerSequence     uint32    `json:"ledger_sequence"`
	MemoHash           string    `json:"memo_hash"`
	ContractStorageKey *string   `json:"contract_storage_key"`
	TxHash             string    `json:"tx_hash"`
	PublishedAt        time.Time `json:"published_at"`
}

type AuditVerificationResult struct {
	IsValid          bool          `json:"is_valid"`
	TotalEntries     int           `json:"total_entries"`
	GenesisHash      string        `json:"genesis_hash"`
	ChainTipHash     string        `json:"chain_tip_hash"`
	MerkleRoot       string        `json:"merkle_root"`
	AnchoredRootHash *string       `json:"anchored_root_hash"`
	AnchorStatus     string        `json:"anchor_status"`
	LatestAnchor     *AnchorRecord `json:"latest_anchor"`
	FirstTamperIndex *uint64       `json:"first_tamper_index"`
	ErrorDetail      *string       `json:"error_detail"`
}

type CreateAuditEntryRequest struct {
	Action  json.RawMessage `json:"action"`
	Actor   string          `json:"actor"`
	Target  *string         `json:"target,omitempty"`
	Payload json.RawMessage `json:"payload,omitempty"`
}

type PublishAnchorRequest struct {
	LedgerSequence *uint32 `json:"ledger_sequence,omitempty"`
}

type RetryReport struct {
	Retried          int `json:"retried"`
	Resolved         int `json:"resolved"`
	StillQuarantined int `json:"still_quarantined"`
}

// ApiErrorBody represents the JSON error payload from the Tessera API.
type ApiErrorBody struct {
	Error   string `json:"error"`
	Message string `json:"message"`
}
