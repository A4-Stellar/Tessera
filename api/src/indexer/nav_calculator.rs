#[expect(
    dead_code,
    reason = "The NAV placeholder is not wired into the indexer yet."
)]
pub fn calculate_nav(_asset_id: &str) -> f64 {
    100.0 // mock value
}
