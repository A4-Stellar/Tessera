use soroban_sdk::{contracttype, panic_with_error, Address, Env, String, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VelocityAccumulator {
    pub used: i128,
    pub window_start: u32,
    pub window_end: u32,
}

#[contracttype]
#[derive(Clone)]
pub(crate) enum VelocityDataKey {
    WindowLedgers,
    DefaultCap,
    JurisdictionCap(String),
    Accumulator(Address, String, u32),
    Windows(Address, String),
    Guard(Address, String),
}

pub(crate) fn window_ledgers(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&VelocityDataKey::WindowLedgers)
        .unwrap_or(0)
}

pub(crate) fn set_window_ledgers(env: &Env, window: u32) {
    if window == 0 {
        panic_with_error!(env, super::Error::VelocityWindowInvalid);
    }
    env.storage()
        .instance()
        .set(&VelocityDataKey::WindowLedgers, &window);
}

pub(crate) fn default_cap(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&VelocityDataKey::DefaultCap)
        .unwrap_or(0)
}

pub(crate) fn set_default_cap(env: &Env, cap: i128) {
    if cap < 0 {
        panic_with_error!(env, super::Error::VelocityCapInvalid);
    }
    env.storage()
        .instance()
        .set(&VelocityDataKey::DefaultCap, &cap);
}

pub(crate) fn set_jurisdiction_cap(env: &Env, jurisdiction: String, cap: i128) {
    if cap < 0 {
        panic_with_error!(env, super::Error::VelocityCapInvalid);
    }
    env.storage()
        .instance()
        .set(&VelocityDataKey::JurisdictionCap(jurisdiction), &cap);
}

pub(crate) fn jurisdiction_cap(env: &Env, jurisdiction: &String) -> i128 {
    env.storage()
        .instance()
        .get(&VelocityDataKey::JurisdictionCap(jurisdiction.clone()))
        .unwrap_or_else(|| default_cap(env))
}

pub(crate) fn check_and_record(
    env: &Env,
    account: &Address,
    amount: i128,
    requested_jurisdiction: &String,
) -> bool {
    let window = window_ledgers(env);
    if window == 0 || amount <= 0 {
        return true;
    }
    let cap = jurisdiction_cap(env, requested_jurisdiction);
    if cap == 0 {
        return true;
    }

    let record = super::ComplianceContract::get_record(env.clone(), account.clone());
    let jurisdiction = if requested_jurisdiction.is_empty() {
        match record.as_ref() {
            Some(record) => record.jurisdiction.clone(),
            None => return false,
        }
    } else {
        match record.as_ref() {
            Some(record) if record.jurisdiction != *requested_jurisdiction => return false,
            Some(_) => requested_jurisdiction.clone(),
            None => requested_jurisdiction.clone(),
        }
    };

    let guard_key = VelocityDataKey::Guard(account.clone(), jurisdiction.clone());
    if env.storage().temporary().has(&guard_key) {
        panic_with_error!(env, super::Error::VelocityReentrant);
    }
    env.storage().temporary().set(&guard_key, &true);

    let sequence = env.ledger().sequence();
    let window_start = (sequence / window) * window;
    purge_expired(env, account, &jurisdiction, window, sequence);

    let key = VelocityDataKey::Accumulator(account.clone(), jurisdiction.clone(), window_start);
    let mut accumulator: VelocityAccumulator =
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(VelocityAccumulator {
                used: 0,
                window_start,
                window_end: window_start.saturating_add(window),
            });

    if sequence >= accumulator.window_end {
        accumulator.used = 0;
        accumulator.window_start = window_start;
        accumulator.window_end = window_start.saturating_add(window);
    }

    let next = match accumulator.used.checked_add(amount) {
        Some(next) => next,
        None => {
            env.storage().temporary().remove(&guard_key);
            return false;
        }
    };
    let allowed = next <= cap;
    if allowed {
        accumulator.used = next;
        env.storage().persistent().set(&key, &accumulator);
        env.storage().persistent().extend_ttl(
            &key,
            window.saturating_add(1),
            window.saturating_add(100),
        );

        let list_key = VelocityDataKey::Windows(account.clone(), jurisdiction.clone());
        let mut windows: Vec<u32> = env
            .storage()
            .persistent()
            .get(&list_key)
            .unwrap_or_else(|| Vec::new(env));
        if !windows.iter().any(|start| start == window_start) {
            windows.push_back(window_start);
            env.storage().persistent().set(&list_key, &windows);
            env.storage().persistent().extend_ttl(
                &list_key,
                window.saturating_add(1),
                window.saturating_add(100),
            );
        }
    }

    env.storage().temporary().remove(&guard_key);
    allowed
}

pub(crate) fn state(
    env: &Env,
    account: &Address,
    jurisdiction: &String,
) -> Option<VelocityAccumulator> {
    let window = window_ledgers(env);
    if window == 0 {
        return None;
    }
    let window_start = (env.ledger().sequence() / window) * window;
    env.storage()
        .persistent()
        .get(&VelocityDataKey::Accumulator(
            account.clone(),
            jurisdiction.clone(),
            window_start,
        ))
}

fn purge_expired(env: &Env, account: &Address, jurisdiction: &String, window: u32, sequence: u32) {
    let list_key = VelocityDataKey::Windows(account.clone(), jurisdiction.clone());
    let windows: Vec<u32> = env
        .storage()
        .persistent()
        .get(&list_key)
        .unwrap_or_else(|| Vec::new(env));
    let mut retained = Vec::new(env);
    for start in windows.iter() {
        if start.saturating_add(window) <= sequence {
            env.storage()
                .persistent()
                .remove(&VelocityDataKey::Accumulator(
                    account.clone(),
                    jurisdiction.clone(),
                    start,
                ));
        } else {
            retained.push_back(start);
        }
    }
    if retained.len() != windows.len() {
        if retained.is_empty() {
            env.storage().persistent().remove(&list_key);
        } else {
            env.storage().persistent().set(&list_key, &retained);
        }
    }
}
