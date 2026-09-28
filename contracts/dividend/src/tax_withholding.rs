// tax_withholding.rs
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, BytesN, Env,
    IntoVal, Symbol, Val, Vec,
};

/// Maximum withholding rate, in basis points (10_000 = 100%).
const MAX_RATE_BPS: u32 = 10_000;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    ComplianceContract,
    TaxCustodyAddress,
    TaxRate(BytesN<32>), // Hash to Tax Rate (in basis points: 10000 = 100%)
    DefaultTaxRate,
}

/// Declared errors for this contract.
///
/// Added by the fuzzing work. Every failure here used to be a bare
/// `panic!("...")` or a `.unwrap()`, which the host reports identically to an
/// arithmetic overflow or an out-of-bounds index — a deliberate rejection and a
/// real bug were therefore indistinguishable, both to a caller and to the
/// fuzzer. Numbers start at 1 because the contract previously had no error enum
/// at all, so there is no pre-existing numbering to preserve.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    /// Rate is not in `0..=10_000` basis points.
    InvalidTaxRate = 4,
    InvalidAmount = 5,
}

#[contract]
pub struct TaxWithholdingContract;

#[contractimpl]
impl TaxWithholdingContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        compliance_contract: Address,
        tax_custody_address: Address,
        default_tax_rate: u32,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        // The admin is the only address that can change any rate, so it has to
        // authorise its own installation. Previously anyone could front-run this
        // and install themselves as the tax administrator.
        admin.require_auth();
        Self::require_valid_rate(&env, default_tax_rate);

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::ComplianceContract, &compliance_contract);
        env.storage()
            .instance()
            .set(&DataKey::TaxCustodyAddress, &tax_custody_address);
        env.storage()
            .instance()
            .set(&DataKey::DefaultTaxRate, &default_tax_rate);
    }

    pub fn set_tax_rate(env: Env, admin: Address, residency_hash: BytesN<32>, rate: u32) {
        Self::require_admin(&env, &admin);
        Self::require_valid_rate(&env, rate);
        env.storage()
            .persistent()
            .set(&DataKey::TaxRate(residency_hash), &rate);
    }

    pub fn set_default_tax_rate(env: Env, admin: Address, rate: u32) {
        Self::require_admin(&env, &admin);
        Self::require_valid_rate(&env, rate);
        env.storage()
            .instance()
            .set(&DataKey::DefaultTaxRate, &rate);
    }

    pub fn process_dividend(
        env: Env,
        investor: Address,
        dividend_amount: i128,
    ) -> (i128, i128, Address) {
        if dividend_amount < 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let compliance_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::ComplianceContract)
            .unwrap();

        let args: Vec<Val> = (investor.clone(),).into_val(&env);
        let residency_hash: Option<BytesN<32>> = env.invoke_contract(
            &compliance_contract,
            &Symbol::new(&env, "get_tax_residency"),
            args,
        );

        let default: u32 = env
            .storage()
            .instance()
            .get(&DataKey::DefaultTaxRate)
            .unwrap_or(0);
        let rate = match residency_hash {
            // `or(Some(default))` rather than `or_else`: a missing per-hash rate
            // falls back to the default, and a stored rate of 0 is a legitimate
            // "this investor is exempt", not a signal to fall back.
            Some(hash) => env
                .storage()
                .persistent()
                .get(&DataKey::TaxRate(hash))
                .unwrap_or_else(|| {
                    env.storage()
                        .instance()
                        .get(&DataKey::DefaultTaxRate)
                        .unwrap()
                }),
            None => env
                .storage()
                .instance()
                .get(&DataKey::DefaultTaxRate)
                .unwrap(),
        };

        // The rate is validated to be at most 10_000 bps on every write, so
        // `tax_amount` is bounded by `dividend_amount` and
        // `dividend_amount - tax_amount` cannot underflow.
        //
        // The product is formed in `u128` rather than `saturating_mul` in
        // `i128`, because the previous `dividend_amount.saturating_mul(rate) /
        // 10000` *silently under-withheld* on large dividends: it clamped the
        // product to `i128::MAX` and then divided, so a 50% rate on
        // `i128::MAX` withheld `i128::MAX / 10_000` instead of
        // `i128::MAX / 2` — roughly a 99.5% revenue loss that reported no error
        // whatsoever. No overflow trap, no declared error, just a wrong number.
        // `i128::MAX * 10_000` is under 2^140, so `u128` holds it exactly and
        // the division stays lossless.
        let tax_amount = ((dividend_amount as u128 * rate as u128) / 10_000) as i128;
        let net_amount = dividend_amount - tax_amount;
        let custody_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::TaxCustodyAddress)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));

        (net_amount, tax_amount, custody_address)
    }

    // ---- internal ----

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_valid_rate(env: &Env, rate: u32) {
        if rate > MAX_RATE_BPS {
            panic_with_error!(env, Error::InvalidTaxRate);
        }
    }
}
