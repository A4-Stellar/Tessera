// tax_withholding.rs

use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env, IntoVal, Symbol, Val, Vec};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    ComplianceContract,
    TaxCustodyAddress,
    TaxRate(BytesN<32>), // Hash to Tax Rate (in basis points: 10000 = 100%)
    DefaultTaxRate,
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
            panic!("already initialized");
        }
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
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        env.storage()
            .persistent()
            .set(&DataKey::TaxRate(residency_hash), &rate);
    }

    pub fn set_default_tax_rate(env: Env, admin: Address, rate: u32) {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        env.storage()
            .instance()
            .set(&DataKey::DefaultTaxRate, &rate);
    }

    pub fn process_dividend(
        env: Env,
        investor: Address,
        dividend_amount: i128,
    ) -> (i128, i128, Address) {
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

        let rate = match residency_hash {
            Some(hash) => env
                .storage()
                .persistent()
                .get(&DataKey::TaxRate(hash))
                .unwrap_or_else(|| env.storage().instance().get(&DataKey::DefaultTaxRate).unwrap()),
            None => env
                .storage()
                .instance()
                .get(&DataKey::DefaultTaxRate)
                .unwrap(),
        };

        let tax_amount = (dividend_amount * (rate as i128)) / 10000;
        let net_amount = dividend_amount - tax_amount;
        let custody_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::TaxCustodyAddress)
            .unwrap();

        (net_amount, tax_amount, custody_address)
    }
}
