//! Tiny hand-written WASM ABI for the token-economics simulator.
//!
//! There is intentionally no `wasm-bindgen` glue: the docs worker writes a
//! `Float64Array` of parameters into linear memory and reads a JSON string
//! back. That keeps the module buildable offline with `cargo build --target
//! wasm32-unknown-unknown` and makes the wire format trivial to test.

mod model;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;

pub use model::{Params, Simulation, SimulationOutput, PARAM_COUNT};

thread_local! {
    static SIMULATIONS: RefCell<HashMap<u32, Simulation>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(1) };
    static OUTPUT: RefCell<String> = RefCell::new(String::new());
}

/// Allocate `len` bytes in the module's linear memory. The caller writes the
/// parameter payload there and passes the pointer to `tessera_sim_new`.
#[no_mangle]
pub extern "C" fn tessera_alloc(len: u32) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len as usize);
    let pointer = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    pointer
}

/// Create a simulation from `PARAM_COUNT` little-endian f64 values.
#[no_mangle]
pub extern "C" fn tessera_sim_new(params_ptr: *const u8, params_len: u32) -> u32 {
    let count = (params_len as usize) / std::mem::size_of::<f64>();
    if params_ptr.is_null() || count == 0 {
        return 0;
    }
    let values = unsafe { std::slice::from_raw_parts(params_ptr as *const f64, count) };
    let sim = Simulation::new(Params::from_f64_slice(values));
    NEXT_HANDLE.with(|next| {
        let mut next = next.borrow_mut();
        let handle = *next;
        *next = next.wrapping_add(1);
        SIMULATIONS.with(|sims| sims.borrow_mut().insert(handle, sim));
        handle
    })
}

/// Advance one step. Returns 1 while work remains, 0 when complete.
#[no_mangle]
pub extern "C" fn tessera_sim_step(handle: u32) -> u32 {
    let done = SIMULATIONS.with(|sims| {
        let mut sims = sims.borrow_mut();
        match sims.get_mut(&handle) {
            Some(sim) => {
                sim.step();
                sim.is_done()
            }
            None => true,
        }
    });
    u32::from(!done)
}

#[no_mangle]
pub extern "C" fn tessera_sim_progress(handle: u32) -> f64 {
    SIMULATIONS.with(|sims| {
        sims.borrow()
            .get(&handle)
            .map(|sim| sim.progress())
            .unwrap_or(1.0)
    })
}

/// Serialise the result to JSON and return `(pointer, len)` via the pair of
/// accessors below. The buffer is replaced on each call, so consumers must
/// copy it before calling again.
#[no_mangle]
pub extern "C" fn tessera_sim_write_output(handle: u32) {
    let json = SIMULATIONS.with(|sims| {
        sims.borrow()
            .get(&handle)
            .map(|sim| to_json(sim.output()))
            .unwrap_or_else(|| "{}".to_owned())
    });
    OUTPUT.with(|output| *output.borrow_mut() = json);
}

#[no_mangle]
pub extern "C" fn tessera_sim_output_ptr() -> *const u8 {
    OUTPUT.with(|output| output.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn tessera_sim_output_len() -> u32 {
    OUTPUT.with(|output| output.borrow().len() as u32)
}

#[no_mangle]
pub extern "C" fn tessera_sim_free(handle: u32) {
    SIMULATIONS.with(|sims| {
        sims.borrow_mut().remove(&handle);
    });
}

/// Manual JSON writer — avoids a serde dependency in the WASM binary.
pub fn to_json(output: &SimulationOutput) -> String {
    fn floats(out: &mut String, values: &[f64]) {
        let _ = out.write_char('[');
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                let _ = out.write_char(',');
            }
            let _ = write!(out, "{value:.8}");
        }
        let _ = out.write_char(']');
    }
    fn matrix_floats(out: &mut String, rows: &[Vec<f64>]) {
        let _ = out.write_char('[');
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                let _ = out.write_char(',');
            }
            floats(out, row);
        }
        let _ = out.write_char(']');
    }
    fn matrix_counts(out: &mut String, rows: &[Vec<u32>]) {
        let _ = out.write_char('[');
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                let _ = out.write_char(',');
            }
            let _ = out.write_char('[');
            for (i, value) in row.iter().enumerate() {
                if i > 0 {
                    let _ = out.write_char(',');
                }
                let _ = write!(out, "{value}");
            }
            let _ = out.write_char(']');
        }
        let _ = out.write_char(']');
    }

    let mut out = String::with_capacity(4096);
    let _ = write!(out, "{{\"steps\":{},\"price_series\":", output.steps);
    floats(&mut out, &output.price_series);
    let _ = out.write_str(",\"oracle_series\":");
    floats(&mut out, &output.oracle_series);
    let _ = out.write_str(",\"holder_distribution\":");
    matrix_counts(&mut out, &output.holder_distribution);
    let _ = out.write_str(",\"dividend_yield\":");
    floats(&mut out, &output.dividend_yield);
    let _ = out.write_str(",\"liquidity_depth\":");
    matrix_floats(&mut out, &output.liquidity_depth);
    let _ = write!(
        out,
        ",\"final_gini\":{:.8},\"max_drawdown\":{:.8}}}",
        output.final_gini, output.max_drawdown
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_runs_a_simulation_and_emits_valid_json() {
        let mut params = vec![0.0f64; PARAM_COUNT];
        params[0] = 7.0; // seed
        params[1] = 40.0; // steps
        params[2] = 30.0; // initial price
        params[5] = 10.0; // institutional
        params[6] = 20.0; // retail
        params[7] = 3.0; // arbitrageurs

        let handle = tessera_sim_new(params.as_ptr() as *const u8, (params.len() * 8) as u32);
        assert!(handle > 0);
        while tessera_sim_step(handle) != 0 {}
        assert_eq!(tessera_sim_progress(handle), 1.0);
        tessera_sim_write_output(handle);
        let json = unsafe {
            let ptr = tessera_sim_output_ptr();
            let len = tessera_sim_output_len() as usize;
            std::str::from_utf8(std::slice::from_raw_parts(ptr, len)).unwrap()
        };
        assert!(json.contains("\"price_series\""));
        assert!(json.contains("\"liquidity_depth\""));
        assert!(json.contains("\"final_gini\""));
        assert_eq!(json.matches("\"steps\":40").count(), 1);
        tessera_sim_free(handle);
    }
}
