//! Historical holder state for `GET /assets/:id/holders?at_ledger=N`.
//!
//! Balances are rebuilt by replaying the indexer's `transfer` event deltas for
//! the asset's token contract up to and including the requested ledger.

use std::collections::BTreeMap;

use crate::indexer::{parse_i128, ratio_percent};
use crate::models::{Event, Holder};

/// Holders (balance > 0, sorted by balance descending) as of `at_ledger`.
pub fn holders_at(
    events: &[Event],
    token_contract: &str,
    total_supply: &str,
    at_ledger: u32,
) -> Vec<Holder> {
    let mut balances: BTreeMap<&str, i128> = BTreeMap::new();
    for event in events {
        if event.contract != token_contract
            || event.event_type != "transfer"
            || event.ledger > at_ledger
        {
            continue;
        }
        let amount = event.data["amount"].as_str().map_or(0, parse_i128);
        if amount < 0 {
            continue;
        }
        if let Some(from) = event.data["from"].as_str() {
            let balance = balances.entry(from).or_insert(0);
            *balance = balance.saturating_sub(amount);
        }
        if let Some(to) = event.data["to"].as_str() {
            let balance = balances.entry(to).or_insert(0);
            *balance = balance.saturating_add(amount);
        }
    }

    let supply = parse_i128(total_supply);
    let mut holders: Vec<Holder> = balances
        .into_iter()
        .filter(|(_, balance)| *balance > 0)
        .map(|(address, balance)| Holder {
            address: address.to_string(),
            balance: balance.to_string(),
            share_percent: ratio_percent(balance, supply),
        })
        .collect();
    holders.sort_by_key(|h| std::cmp::Reverse(parse_i128(&h.balance)));
    holders
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::holders_at;
    use crate::models::Event;

    fn transfer(id: u64, ledger: u32, from: Option<&str>, to: &str, amount: &str) -> Event {
        Event {
            id,
            contract: "TOKEN".to_string(),
            event_type: "transfer".to_string(),
            ledger,
            timestamp: None,
            data: json!({ "from": from, "to": to, "amount": amount }),
        }
    }

    fn events() -> Vec<Event> {
        vec![
            transfer(1, 10, None, "A", "100"),
            transfer(2, 20, Some("A"), "B", "40"),
            transfer(3, 30, Some("A"), "B", "60"),
        ]
    }

    #[test]
    fn replays_only_deltas_up_to_the_ledger() {
        let at_20 = holders_at(&events(), "TOKEN", "100", 20);
        assert_eq!(at_20.len(), 2);
        assert_eq!(at_20[0].address, "A");
        assert_eq!(at_20[0].balance, "60");
        assert_eq!(at_20[1].address, "B");
        assert_eq!(at_20[1].balance, "40");
        assert_eq!(at_20[0].share_percent, 60.0);
    }

    #[test]
    fn drops_zero_balances_and_ignores_other_contracts() {
        let at_30 = holders_at(&events(), "TOKEN", "100", 30);
        assert_eq!(at_30.len(), 1);
        assert_eq!(at_30[0].address, "B");
        assert!(holders_at(&events(), "OTHER", "100", 30).is_empty());
        assert!(holders_at(&events(), "TOKEN", "100", 5).is_empty());
    }
}
