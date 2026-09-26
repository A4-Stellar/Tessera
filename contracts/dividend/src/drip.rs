//! Dividend Reinvestment Program routing.
//!
//! The configured AMM must expose
//! `swap_exact_in(token_in, token_out, amount_in, min_amount_out, payer, recipient) -> i128`.
//! It pulls `amount_in` from `payer` during that call and sends the acquired
//! asset tokens to `recipient`. The cap-table contract must have
//! this dividend contract configured as its DRIP registrar.

use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contracttype, panic_with_error, symbol_short, Address, Env, IntoVal, Symbol, Val, Vec,
};

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Preference(Address),
}

pub fn set_preference(env: &Env, holder: &Address, enabled: bool) {
    holder.require_auth();
    env.storage()
        .persistent()
        .set(&DataKey::Preference(holder.clone()), &enabled);
    env.events()
        .publish((symbol_short!("drippref"), holder.clone()), enabled);
}

pub fn is_enabled(env: &Env, holder: &Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::Preference(holder.clone()))
        .unwrap_or(false)
}

pub fn execute(
    env: &Env,
    amm: &Address,
    cap_table: &Address,
    payment_token: &Address,
    asset_token: &Address,
    holder: &Address,
    amount: i128,
) -> i128 {
    let contract_address = env.current_contract_address();
    let balance_before = token_balance(env, asset_token, holder);
    let args: Vec<Val> = (
        payment_token.clone(),
        asset_token.clone(),
        amount,
        1i128,
        contract_address.clone(),
        holder.clone(),
    )
        .into_val(env);
    // The router pulls the escrowed stablecoin through token.transfer. Grant
    // it exactly that nested transfer authorization for this invocation.
    env.authorize_as_current_contract(soroban_sdk::vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: amm.clone(),
                fn_name: Symbol::new(env, "swap_exact_in"),
                args: args.clone(),
            },
            sub_invocations: soroban_sdk::vec![
                env,
                InvokerContractAuthEntry::Contract(SubContractInvocation {
                    context: ContractContext {
                        contract: payment_token.clone(),
                        fn_name: Symbol::new(env, "transfer"),
                        args: (contract_address, amm.clone(), amount).into_val(env),
                    },
                    sub_invocations: soroban_sdk::vec![env],
                }),
            ],
        }),
    ]);

    let received: i128 = env.invoke_contract(amm, &Symbol::new(env, "swap_exact_in"), args);
    let balance_after = token_balance(env, asset_token, holder);
    if received <= 0 || balance_after.saturating_sub(balance_before) != received {
        panic_with_error!(env, crate::Error::InvalidSwapOutput);
    }

    let record_args: Vec<Val> = (asset_token.clone(), holder.clone(), received).into_val(env);
    env.authorize_as_current_contract(soroban_sdk::vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: cap_table.clone(),
                fn_name: Symbol::new(env, "record_drip_acquisition"),
                args: record_args.clone(),
            },
            sub_invocations: soroban_sdk::vec![env],
        }),
    ]);
    let _: () = env.invoke_contract(
        cap_table,
        &Symbol::new(env, "record_drip_acquisition"),
        record_args,
    );
    received
}

fn token_balance(env: &Env, token: &Address, holder: &Address) -> i128 {
    let args: Vec<Val> = (holder.clone(),).into_val(env);
    env.invoke_contract(token, &Symbol::new(env, "balance"), args)
}
