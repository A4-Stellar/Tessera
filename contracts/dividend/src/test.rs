use crate::{DividendContract, DividendContractClient};
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, Address, Env, IntoVal, Symbol,
    Val, Vec,
};
use tessera_cap_table::{CapTableContract, CapTableContractClient};

const WAD: i128 = 1_000_000_000_000_000_000;
const SECONDS_PER_YEAR: u64 = 31_536_000;

#[derive(Clone)]
#[contracttype]
enum TokenKey {
    Supply,
    Balance(Address),
}

#[contract]
struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn initialize(env: Env, owner: Address, supply: i128, balance: i128) {
        env.storage().instance().set(&TokenKey::Supply, &supply);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(owner), &balance);
    }

    pub fn balance(env: Env, holder: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(holder))
            .unwrap_or(0)
    }

    pub fn total_supply(env: Env) -> i128 {
        env.storage().instance().get(&TokenKey::Supply).unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_balance = Self::balance(env.clone(), from.clone());
        let to_balance = Self::balance(env.clone(), to.clone());
        assert!(amount > 0 && from_balance >= amount);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(from), &(from_balance - amount));
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(to_balance + amount));
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(balance + amount));
    }
}

#[contract]
struct MockAmm;

#[contractimpl]
impl MockAmm {
    pub fn swap_exact_in(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
        payer: Address,
        recipient: Address,
    ) -> i128 {
        let amm = env.current_contract_address();
        let args: Vec<Val> = (payer, amm, amount_in).into_val(&env);
        let _: () = env.invoke_contract(&token_in, &Symbol::new(&env, "transfer"), args);

        let amount_out = amount_in * 2;
        assert!(amount_out >= min_amount_out);
        let args: Vec<Val> = (recipient, amount_out).into_val(&env);
        let _: () = env.invoke_contract(&token_out, &Symbol::new(&env, "mint"), args);
        amount_out
    }
}

#[test]
fn opted_in_claim_swaps_and_records_acquisition() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let holder = Address::generate(&env);
    let stable = env.register(MockToken, ());
    let asset = env.register(MockToken, ());
    let amm = env.register(MockAmm, ());
    let cap_table_id = env.register(CapTableContract, ());
    let cap_table = CapTableContractClient::new(&env, &cap_table_id);

    MockTokenClient::new(&env, &stable).initialize(&admin, &10_000, &10_000);
    MockTokenClient::new(&env, &asset).initialize(&holder, &10_000, &1_000);

    let dividend_id = env.register(DividendContract, ());
    let dividend = DividendContractClient::new(&env, &dividend_id);
    dividend.initialize(&admin);
    cap_table.initialize(&admin);
    cap_table.set_drip_registrar(&admin, &dividend_id);
    dividend.configure_drip(&admin, &amm, &cap_table_id);
    let distribution_id = dividend.create_distribution(&admin, &asset, &stable, &100);
    dividend.set_drip_preference(&holder, &true);

    dividend.claim(&distribution_id, &holder);

    assert_eq!(MockTokenClient::new(&env, &asset).balance(&holder), 1_200);
    assert_eq!(cap_table.drip_acquisitions(&asset, &holder), 200);
    assert!(dividend.has_claimed(&distribution_id, &holder));
    assert_eq!(dividend.get_distribution(&distribution_id).distributed, 100);
}

#[test]
fn preference_is_opt_in_by_default_and_can_be_disabled() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let holder = Address::generate(&env);
    let contract = env.register(DividendContract, ());
    let client = DividendContractClient::new(&env, &contract);
    client.initialize(&admin);

    assert!(!client.get_drip_preference(&holder));
    client.set_drip_preference(&holder, &true);
    assert!(client.get_drip_preference(&holder));
    client.set_drip_preference(&holder, &false);
    assert!(!client.get_drip_preference(&holder));
}

// ---- issue #144: compound yield on unclaimed dividends ----

/// Fixture: one distribution of 1000 stable, one holder owning 100% of the
/// asset supply. Holder starts with zero stable; the contract holds escrow.
struct YieldFixture {
    env: Env,
    contract_id: Address,
    admin: Address,
    holder: Address,
    stable: Address,
    distribution_id: u64,
}

impl YieldFixture {
    fn client(&self) -> DividendContractClient {
        DividendContractClient::new(&self.env, &self.contract_id)
    }

    fn stable_client(&self) -> MockTokenClient {
        MockTokenClient::new(&self.env, &self.stable)
    }

    /// Stable paid to the holder beyond the 1000-unit base share.
    fn yield_paid_to_holder(&self) -> i128 {
        self.stable_client().balance(&self.holder) - 1_000
    }

    /// Stable left in the contract (escrow + yield budget minus payouts).
    fn contract_balance(&self) -> i128 {
        self.stable_client().balance(&self.contract_id)
    }
}

fn yield_setup(env: &Env) -> YieldFixture {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let holder = Address::generate(env);
    let stable = env.register(MockToken, ());
    let asset = env.register(MockToken, ());

    // Admin funds the base escrow (1000 stable); holder owns all asset.
    MockTokenClient::new(env, &stable).initialize(&admin, &10_000, &10_000);
    MockTokenClient::new(env, &asset).initialize(&holder, &10_000, &1_000);

    let contract_id = env.register(DividendContract, ());
    let client = DividendContractClient::new(env, &contract_id);
    client.initialize(&admin);
    let distribution_id = client.create_distribution(&admin, &asset, &stable, &1_000);

    YieldFixture {
        env: env.clone(),
        contract_id,
        admin,
        holder,
        stable,
        distribution_id,
    }
}

#[test]
fn yield_is_zero_until_configured() {
    let env = Env::default();
    let s = yield_setup(&env);

    // No yield configured: info is 0 even with an outstanding claim.
    assert_eq!(s.client().get_yield_info(&s.distribution_id, &s.holder), 0);
}

#[test]
fn claim_after_time_pays_base_share_plus_yield_from_budget() {
    let env = Env::default();
    let s = yield_setup(&env);

    // 10% annual yield, funded with 200 stable (well above the ~105.17
    // interest one year at 10% continuous compounding on the 1000 share).
    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &(WAD / 10));
    s.client().fund_yield(&s.admin, &s.distribution_id, &200);

    // Advance one year.
    s.env.ledger().set_timestamp(SECONDS_PER_YEAR);

    s.client().claim(&s.distribution_id, &s.holder);

    // Base share: 1000. Yield: 1000·(e^0.1−1) ≈ 105.17 → 105 whole units.
    let yield_paid = s.yield_paid_to_holder();
    assert_eq!(yield_paid, 105, "truncated whole-unit yield; got {yield_paid}");
    // Contract holds: escrow 1000 + budget 200 − base 1000 − yield 105.
    // The base escrow is untouched by yield payouts: 200 − 105 remain.
    assert_eq!(s.contract_balance(), 95);
}

#[test]
fn yield_compounds_across_two_years() {
    let env = Env::default();
    let s = yield_setup(&env);

    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &(WAD / 10));
    s.client().fund_yield(&s.admin, &s.distribution_id, &500);

    s.env.ledger().set_timestamp(2 * SECONDS_PER_YEAR);
    s.client().claim(&s.distribution_id, &s.holder);

    // 1000·(e^0.2−1) ≈ 221.40 stable.
    let yield_paid = s.yield_paid_to_holder();
    assert_eq!(yield_paid, 221, "two-year compound yield; got {yield_paid}");
    assert!(yield_paid > 200, "compounding must beat 2×10% simple");
}

#[test]
fn yield_budget_shortfall_keeps_remainder_accrued_and_claimable() {
    let env = Env::default();
    let s = yield_setup(&env);

    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &(WAD / 10));
    // Only 50 of the ~105.17 owed is funded.
    s.client().fund_yield(&s.admin, &s.distribution_id, &50);

    s.env.ledger().set_timestamp(SECONDS_PER_YEAR);
    s.client().claim(&s.distribution_id, &s.holder);

    assert_eq!(s.yield_paid_to_holder(), 50, "paid only what the budget covered");

    // Top up the budget; the remainder (~55.17) is claimable without
    // re-claiming the base share.
    s.client().fund_yield(&s.admin, &s.distribution_id, &100);
    s.client().claim_yield(&s.distribution_id, &s.holder);
    let total_yield = s.yield_paid_to_holder();
    assert_eq!(total_yield, 105, "full yield eventually paid; got {total_yield}");

    // Nothing left accrued afterwards. Contract holds escrow 1000 + funds
    // (50 + 100) − base 1000 − yield (50 + 55) = 45.
    assert_eq!(s.client().get_yield_info(&s.distribution_id, &s.holder), 0);
    assert_eq!(s.contract_balance(), 45);
}

#[test]
fn get_yield_info_estimates_the_claimable_yield_before_claiming() {
    let env = Env::default();
    let s = yield_setup(&env);

    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &(WAD / 10));
    s.client().fund_yield(&s.admin, &s.distribution_id, &200);
    s.env.ledger().set_timestamp(SECONDS_PER_YEAR);

    // Nothing written yet (accrual is lazy): the estimate matches what the
    // later claim pays (whole units, same truncation path).
    let info = s.client().get_yield_info(&s.distribution_id, &s.holder);
    assert_eq!(info, 105, "estimate matches the paid amount; got {info}");
}

#[test]
fn second_claim_of_base_share_is_rejected_and_yields_nothing() {
    let env = Env::default();
    let s = yield_setup(&env);

    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &(WAD / 10));
    s.client().fund_yield(&s.admin, &s.distribution_id, &200);
    s.env.ledger().set_timestamp(SECONDS_PER_YEAR);
    s.client().claim(&s.distribution_id, &s.holder);
    assert_eq!(s.yield_paid_to_holder(), 105);

    // Base share already claimed: second claim must fail.
    let result = s.client().try_claim(&s.distribution_id, &s.holder);
    assert!(matches!(result, Err(Ok(_))), "double claim must fail");

    // And the budget is not double-spent.
    assert_eq!(s.yield_paid_to_holder(), 105);
}

#[test]
fn distribution_without_yield_rate_pays_base_share_only() {
    let env = Env::default();
    let s = yield_setup(&env);

    // No set_distribution_yield call: yield disabled.
    s.env.ledger().set_timestamp(5 * SECONDS_PER_YEAR);
    s.client().claim(&s.distribution_id, &s.holder);

    assert_eq!(s.yield_paid_to_holder(), 0, "no yield without a configured rate");
}

#[test]
fn zero_yield_rate_clears_an_existing_configuration() {
    let env = Env::default();
    let s = yield_setup(&env);

    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &(WAD / 10));
    // Disabling after the fact: claims pay base share only.
    s.client().set_distribution_yield(&s.admin, &s.distribution_id, &0);
    s.env.ledger().set_timestamp(SECONDS_PER_YEAR);
    s.client().claim(&s.distribution_id, &s.holder);

    assert_eq!(s.yield_paid_to_holder(), 0);
}
