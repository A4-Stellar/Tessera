//! Issue #475: packet events, volume caps, circuit breaker and a simulated
//! bridge drain. Signature checks are covered by `contracts/fuzz`.

extern crate std;

use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};
use soroban_sdk::{vec, Address, Bytes, BytesN, Env, Vec};

use super::*;

struct Setup<'a> {
    env: Env,
    admin: Address,
    bridge: BridgeProtocolClient<'a>,
    guardians: Vec<Address>,
    reporter: Address,
}

const WINDOW: u32 = 720; // ~1 hour of ledgers
const MAX_VOLUME: i128 = 1_000_000;

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(10_000);
    let id = env.register(BridgeProtocol, ());
    let bridge = BridgeProtocolClient::new(&env, &id);
    let admin = Address::generate(&env);
    bridge.initialize(&admin);
    bridge.configure_circuit_breaker(
        &admin,
        &CircuitConfig {
            window_ledgers: WINDOW,
            max_window_volume: MAX_VOLUME,
            max_window_packets: 50,
            anomaly_threshold_bps: 8_000,
        },
    );
    let guardians: Vec<Address> = Vec::from_array(
        &env,
        core::array::from_fn::<_, 3, _>(|_| Address::generate(&env)),
    );
    bridge.set_guardians(&admin, &guardians, &2);
    let reporter = Address::generate(&env);
    bridge.set_anomaly_reporter(&admin, &reporter);
    Setup {
        env,
        admin,
        bridge,
        guardians,
        reporter,
    }
}

fn send(s: &Setup, caller: &Address, amount: i128) -> Result<u64, Error> {
    let chain = Bytes::from_slice(&s.env, b"ethereum");
    let dest = Bytes::from_slice(&s.env, b"0xabc");
    s.bridge
        .try_lock_and_mint_request(caller, &amount, &chain, &dest)
        .map(|r| r.unwrap())
        .map_err(|e| Error::try_from(e.unwrap()).unwrap_or(Error::Unauthorized))
}

/// First topic of each event from the last invocation.
fn event_names(env: &Env) -> std::vec::Vec<std::string::String> {
    use soroban_sdk::xdr::{ContractEventBody, ScVal};
    env.events()
        .all()
        .events()
        .iter()
        .filter_map(|e| match &e.body {
            ContractEventBody::V0(v0) => match v0.topics.first() {
                Some(ScVal::Symbol(s)) => Some(s.to_utf8_string_lossy()),
                _ => None,
            },
        })
        .collect()
}

#[test]
fn send_packet_events_carry_a_global_sequence() {
    let s = setup();
    let alice = Address::generate(&s.env);
    let bob = Address::generate(&s.env);
    assert_eq!(send(&s, &alice, 10), Ok(0));
    assert_eq!(send(&s, &bob, 20), Ok(0));
    assert_eq!(send(&s, &alice, 30), Ok(1));
    assert_eq!(event_names(&s.env), ["SendPacket"]);
    assert_eq!(s.bridge.get_request(&bob, &0).sequence, 1);
    assert_eq!(s.bridge.get_request(&alice, &1).sequence, 2);
    assert_eq!(send(&s, &alice, 0), Err(Error::InvalidAmount));
}

#[test]
fn accepted_inbound_message_publishes_recv_packet() {
    let s = setup();
    let keys: std::vec::Vec<SigningKey> = (1u8..=2)
        .map(|i| SigningKey::from_bytes(&[i; 32]))
        .collect();
    for k in &keys {
        s.bridge.register_validator(
            &s.admin,
            &BytesN::from_array(&s.env, &k.verifying_key().to_bytes()),
        );
    }
    let msg = [42u8; 32];
    let sigs = Vec::from_iter(
        &s.env,
        keys.iter()
            .map(|k| BytesN::from_array(&s.env, &k.sign(&msg).to_bytes())),
    );
    let pks = Vec::from_iter(
        &s.env,
        keys.iter()
            .map(|k| BytesN::from_array(&s.env, &k.verifying_key().to_bytes())),
    );
    let caller = Address::generate(&s.env);
    let hash = BytesN::from_array(&s.env, &msg);
    s.bridge
        .burn_and_unlock_request(&caller, &500, &hash, &sigs, &pks, &u64::MAX);
    assert_eq!(event_names(&s.env), ["RecvPacket"]);
    assert!(s.bridge.is_processed(&hash));

    // A bad signature under a registered key does not count, and does not trap.
    let other = BytesN::from_array(&s.env, &[43u8; 32]);
    let bad = vec![
        &s.env,
        BytesN::from_array(&s.env, &[0u8; 64]),
        sigs.get(1).unwrap(),
    ];
    let r = s
        .bridge
        .try_burn_and_unlock_request(&caller, &500, &other, &bad, &pks, &u64::MAX);
    assert_eq!(
        r.unwrap_err().unwrap(),
        Error::InsufficientSignatures.into()
    );
}

/// The drain: an attacker floods outbound packets. The window cap stops the
/// flow at `MAX_VOLUME`; the monitor's anomaly score then opens the breaker,
/// which holds across windows until two of three Guardians unpause.
#[test]
fn bridge_drain_is_capped_then_paused_until_guardians_unpause() {
    let s = setup();
    let attacker = Address::generate(&s.env);
    let honest = Address::generate(&s.env);

    let mut drained = 0i128;
    let mut refused = 0;
    for _ in 0..40 {
        match send(&s, &attacker, 100_000) {
            Ok(_) => drained += 100_000,
            Err(Error::WindowCapExceeded) => refused += 1,
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    assert_eq!(drained, MAX_VOLUME, "the window cap bounds the drain");
    assert_eq!(refused, 30);
    assert_eq!(s.bridge.circuit_state().window_volume, MAX_VOLUME);

    // Below the threshold the report is recorded but nothing pauses.
    assert!(!s.bridge.report_anomaly(&s.reporter, &5_000));
    // The monitor scores the burst as a drain: the breaker opens.
    assert!(s.bridge.report_anomaly(&s.reporter, &9_700));
    assert!(s.bridge.is_paused());

    // Even in a fresh window, every send is refused while paused.
    s.env.ledger().set_sequence_number(10_000 + WINDOW + 1);
    assert_eq!(send(&s, &attacker, 1), Err(Error::Paused));
    assert_eq!(send(&s, &honest, 1), Err(Error::Paused));

    // Neither the reporter nor one Guardian can reopen it; the admin has no
    // unpause at all.
    let g = |i| s.guardians.get(i).unwrap();
    let r = s.bridge.try_unpause(&vec![&s.env, g(0)]);
    assert_eq!(r.unwrap_err().unwrap(), Error::InsufficientGuardians.into());
    let r = s.bridge.try_unpause(&vec![&s.env, g(0), g(0)]);
    assert_eq!(r.unwrap_err().unwrap(), Error::InsufficientGuardians.into());
    let r = s
        .bridge
        .try_unpause(&vec![&s.env, g(0), s.reporter.clone()]);
    assert_eq!(r.unwrap_err().unwrap(), Error::NotGuardian.into());
    assert!(s.bridge.is_paused());

    // Two of three Guardians close the breaker and the window restarts.
    s.bridge.unpause(&vec![&s.env, g(0), g(2)]);
    assert!(!s.bridge.is_paused());
    assert_eq!(s.bridge.circuit_state().window_volume, 0);
    assert!(send(&s, &honest, 1_000).is_ok());
}

#[test]
fn only_the_reporter_can_report_and_config_is_validated() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    let r = s.bridge.try_report_anomaly(&stranger, &10_000);
    assert_eq!(r.unwrap_err().unwrap(), Error::Unauthorized.into());
    assert!(!s.bridge.is_paused());

    let bad = CircuitConfig {
        window_ledgers: 0,
        max_window_volume: 1,
        max_window_packets: 1,
        anomaly_threshold_bps: 1,
    };
    let r = s.bridge.try_configure_circuit_breaker(&s.admin, &bad);
    assert_eq!(r.unwrap_err().unwrap(), Error::InvalidCircuitConfig.into());

    let r = s.bridge.try_set_guardians(&s.admin, &s.guardians, &4);
    assert_eq!(r.unwrap_err().unwrap(), Error::InvalidGuardians.into());

    // The admin can stop the bridge in an emergency, but cannot restart it.
    s.bridge.pause(&s.admin);
    assert!(s.bridge.is_paused());
    let r = s.bridge.try_unpause(&vec![&s.env, s.admin.clone()]);
    assert_eq!(r.unwrap_err().unwrap(), Error::NotGuardian.into());
}

#[test]
fn packet_cap_limits_many_small_sends() {
    let s = setup();
    let attacker = Address::generate(&s.env);
    let ok = (0..60).filter(|_| send(&s, &attacker, 1).is_ok()).count();
    assert_eq!(ok, 50);
}
