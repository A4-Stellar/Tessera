#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: Vec<u8>| {
    let mut supply: i64 = 0;
    let mut locked: i64 = 0;
    for byte in input {
        let amount = i64::from(byte % 17);
        match byte % 5 {
            0 => supply = supply.saturating_add(amount),
            1 => locked = locked.saturating_add(amount).min(supply),
            2 => supply = supply.saturating_sub(amount),
            3 => { let _ = supply / (amount + 1); },
            _ => supply = supply.saturating_sub(amount).max(locked),
        }
        assert!(supply >= locked && locked >= 0);
    }
});
