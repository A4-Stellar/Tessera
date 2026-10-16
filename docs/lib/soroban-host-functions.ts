/**
 * Soroban host-function import table.
 *
 * Soroban contracts import every host function under a one-character module
 * name and a short export name (e.g. `(import "x" "_")`). This table maps
 * those pairs back to the canonical function name and signature so the WASM
 * disassembler can show what a contract actually calls.
 *
 * Source: stellar/rs-soroban-env v29.0.2, `soroban-env-common/env.json`
 * (protocol 29). Regenerate from that file when the host interface changes.
 */

/** Canonical host function name and its `(args) -> return` signature. */
export type HostFunction = readonly [name: string, signature: string];

export interface HostModule {
  /** Human-readable module name from env.json, e.g. `context`. */
  name: string;
  /** Functions keyed by their WASM export name. */
  functions: Readonly<Record<string, HostFunction>>;
}

export const SOROBAN_HOST_MODULES: Readonly<Record<string, HostModule>> = {
  "x": {
    name: "context",
    functions: {
      "_": ["log_from_linear_memory", "(msg_pos: U32Val, msg_len: U32Val, vals_pos: U32Val, vals_len: U32Val) -> Void"],
      "0": ["obj_cmp", "(a: Val, b: Val) -> i64"],
      "1": ["contract_event", "(topics: VecObject, data: Val) -> Void"],
      "2": ["get_ledger_version", "() -> U32Val"],
      "3": ["get_ledger_sequence", "() -> U32Val"],
      "4": ["get_ledger_timestamp", "() -> U64Val"],
      "5": ["fail_with_error", "(error: Error) -> Void"],
      "6": ["get_ledger_network_id", "() -> BytesObject"],
      "7": ["get_current_contract_address", "() -> AddressObject"],
      "8": ["get_max_live_until_ledger", "() -> U32Val"],
    },
  },
  "i": {
    name: "int",
    functions: {
      "_": ["obj_from_u64", "(v: u64) -> U64Object"],
      "0": ["obj_to_u64", "(obj: U64Object) -> u64"],
      "1": ["obj_from_i64", "(v: i64) -> I64Object"],
      "2": ["obj_to_i64", "(obj: I64Object) -> i64"],
      "3": ["obj_from_u128_pieces", "(hi: u64, lo: u64) -> U128Object"],
      "4": ["obj_to_u128_lo64", "(obj: U128Object) -> u64"],
      "5": ["obj_to_u128_hi64", "(obj: U128Object) -> u64"],
      "6": ["obj_from_i128_pieces", "(hi: i64, lo: u64) -> I128Object"],
      "7": ["obj_to_i128_lo64", "(obj: I128Object) -> u64"],
      "8": ["obj_to_i128_hi64", "(obj: I128Object) -> i64"],
      "9": ["obj_from_u256_pieces", "(hi_hi: u64, hi_lo: u64, lo_hi: u64, lo_lo: u64) -> U256Object"],
      "a": ["u256_val_from_be_bytes", "(bytes: BytesObject) -> U256Val"],
      "b": ["u256_val_to_be_bytes", "(val: U256Val) -> BytesObject"],
      "c": ["obj_to_u256_hi_hi", "(obj: U256Object) -> u64"],
      "d": ["obj_to_u256_hi_lo", "(obj: U256Object) -> u64"],
      "e": ["obj_to_u256_lo_hi", "(obj: U256Object) -> u64"],
      "f": ["obj_to_u256_lo_lo", "(obj: U256Object) -> u64"],
      "g": ["obj_from_i256_pieces", "(hi_hi: i64, hi_lo: u64, lo_hi: u64, lo_lo: u64) -> I256Object"],
      "h": ["i256_val_from_be_bytes", "(bytes: BytesObject) -> I256Val"],
      "i": ["i256_val_to_be_bytes", "(val: I256Val) -> BytesObject"],
      "j": ["obj_to_i256_hi_hi", "(obj: I256Object) -> i64"],
      "k": ["obj_to_i256_hi_lo", "(obj: I256Object) -> u64"],
      "l": ["obj_to_i256_lo_hi", "(obj: I256Object) -> u64"],
      "m": ["obj_to_i256_lo_lo", "(obj: I256Object) -> u64"],
      "n": ["u256_add", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "o": ["u256_sub", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "p": ["u256_mul", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "q": ["u256_div", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "r": ["u256_rem_euclid", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "s": ["u256_pow", "(lhs: U256Val, rhs: U32Val) -> U256Val"],
      "t": ["u256_shl", "(lhs: U256Val, rhs: U32Val) -> U256Val"],
      "u": ["u256_shr", "(lhs: U256Val, rhs: U32Val) -> U256Val"],
      "v": ["i256_add", "(lhs: I256Val, rhs: I256Val) -> I256Val"],
      "w": ["i256_sub", "(lhs: I256Val, rhs: I256Val) -> I256Val"],
      "x": ["i256_mul", "(lhs: I256Val, rhs: I256Val) -> I256Val"],
      "y": ["i256_div", "(lhs: I256Val, rhs: I256Val) -> I256Val"],
      "z": ["i256_rem_euclid", "(lhs: I256Val, rhs: I256Val) -> I256Val"],
      "A": ["i256_pow", "(lhs: I256Val, rhs: U32Val) -> I256Val"],
      "B": ["i256_shl", "(lhs: I256Val, rhs: U32Val) -> I256Val"],
      "C": ["i256_shr", "(lhs: I256Val, rhs: U32Val) -> I256Val"],
      "D": ["timepoint_obj_from_u64", "(v: u64) -> TimepointObject"],
      "E": ["timepoint_obj_to_u64", "(obj: TimepointObject) -> u64"],
      "F": ["duration_obj_from_u64", "(v: u64) -> DurationObject"],
      "G": ["duration_obj_to_u64", "(obj: DurationObject) -> u64"],
      "H": ["u256_checked_add", "(lhs: U256Val, rhs: U256Val) -> Val"],
      "I": ["u256_checked_sub", "(lhs: U256Val, rhs: U256Val) -> Val"],
      "J": ["u256_checked_mul", "(lhs: U256Val, rhs: U256Val) -> Val"],
      "K": ["u256_checked_pow", "(lhs: U256Val, rhs: U32Val) -> Val"],
      "L": ["i256_checked_add", "(lhs: I256Val, rhs: I256Val) -> Val"],
      "M": ["i256_checked_sub", "(lhs: I256Val, rhs: I256Val) -> Val"],
      "N": ["i256_checked_mul", "(lhs: I256Val, rhs: I256Val) -> Val"],
      "O": ["i256_checked_pow", "(lhs: I256Val, rhs: U32Val) -> Val"],
    },
  },
  "m": {
    name: "map",
    functions: {
      "_": ["map_new", "() -> MapObject"],
      "0": ["map_put", "(m: MapObject, k: Val, v: Val) -> MapObject"],
      "1": ["map_get", "(m: MapObject, k: Val) -> Val"],
      "2": ["map_del", "(m: MapObject, k: Val) -> MapObject"],
      "3": ["map_len", "(m: MapObject) -> U32Val"],
      "4": ["map_has", "(m: MapObject, k: Val) -> Bool"],
      "5": ["map_key_by_pos", "(m: MapObject, i: U32Val) -> Val"],
      "6": ["map_val_by_pos", "(m: MapObject, i: U32Val) -> Val"],
      "7": ["map_keys", "(m: MapObject) -> VecObject"],
      "8": ["map_values", "(m: MapObject) -> VecObject"],
      "9": ["map_new_from_linear_memory", "(keys_pos: U32Val, vals_pos: U32Val, len: U32Val) -> MapObject"],
      "a": ["map_unpack_to_linear_memory", "(map: MapObject, keys_pos: U32Val, vals_pos: U32Val, len: U32Val) -> Void"],
      "b": ["sparse_map_new_from_linear_memory", "(keys_pos: U32Val, vals_pos: U32Val, len: U32Val) -> MapObject"],
      "c": ["sparse_map_unpack_to_linear_memory", "(map: MapObject, keys_pos: U32Val, vals_pos: U32Val, len: U32Val) -> Void"],
    },
  },
  "v": {
    name: "vec",
    functions: {
      "_": ["vec_new", "() -> VecObject"],
      "0": ["vec_put", "(v: VecObject, i: U32Val, x: Val) -> VecObject"],
      "1": ["vec_get", "(v: VecObject, i: U32Val) -> Val"],
      "2": ["vec_del", "(v: VecObject, i: U32Val) -> VecObject"],
      "3": ["vec_len", "(v: VecObject) -> U32Val"],
      "4": ["vec_push_front", "(v: VecObject, x: Val) -> VecObject"],
      "5": ["vec_pop_front", "(v: VecObject) -> VecObject"],
      "6": ["vec_push_back", "(v: VecObject, x: Val) -> VecObject"],
      "7": ["vec_pop_back", "(v: VecObject) -> VecObject"],
      "8": ["vec_front", "(v: VecObject) -> Val"],
      "9": ["vec_back", "(v: VecObject) -> Val"],
      "a": ["vec_insert", "(v: VecObject, i: U32Val, x: Val) -> VecObject"],
      "b": ["vec_append", "(v1: VecObject, v2: VecObject) -> VecObject"],
      "c": ["vec_slice", "(v: VecObject, start: U32Val, end: U32Val) -> VecObject"],
      "d": ["vec_first_index_of", "(v: VecObject, x: Val) -> Val"],
      "e": ["vec_last_index_of", "(v: VecObject, x: Val) -> Val"],
      "f": ["vec_binary_search", "(v: VecObject, x: Val) -> u64"],
      "g": ["vec_new_from_linear_memory", "(vals_pos: U32Val, len: U32Val) -> VecObject"],
      "h": ["vec_unpack_to_linear_memory", "(vec: VecObject, vals_pos: U32Val, len: U32Val) -> Void"],
    },
  },
  "l": {
    name: "ledger",
    functions: {
      "_": ["put_contract_data", "(k: Val, v: Val, t: StorageType) -> Void"],
      "0": ["has_contract_data", "(k: Val, t: StorageType) -> Bool"],
      "1": ["get_contract_data", "(k: Val, t: StorageType) -> Val"],
      "2": ["del_contract_data", "(k: Val, t: StorageType) -> Void"],
      "3": ["create_contract", "(deployer: AddressObject, wasm_hash: BytesObject, salt: BytesObject) -> AddressObject"],
      "4": ["create_asset_contract", "(serialized_asset: BytesObject) -> AddressObject"],
      "5": ["upload_wasm", "(wasm: BytesObject) -> BytesObject"],
      "6": ["update_current_contract_wasm", "(hash: BytesObject) -> Void"],
      "7": ["extend_contract_data_ttl", "(k: Val, t: StorageType, threshold: U32Val, extend_to: U32Val) -> Void"],
      "8": ["extend_current_contract_instance_and_code_ttl", "(threshold: U32Val, extend_to: U32Val) -> Void"],
      "9": ["extend_contract_instance_and_code_ttl", "(contract: AddressObject, threshold: U32Val, extend_to: U32Val) -> Void"],
      "a": ["get_contract_id", "(deployer: AddressObject, salt: BytesObject) -> AddressObject"],
      "b": ["get_asset_contract_id", "(serialized_asset: BytesObject) -> AddressObject"],
      "c": ["extend_contract_instance_ttl", "(contract: AddressObject, threshold: U32Val, extend_to: U32Val) -> Void"],
      "d": ["extend_contract_code_ttl", "(contract: AddressObject, threshold: U32Val, extend_to: U32Val) -> Void"],
      "e": ["create_contract_with_constructor", "(deployer: AddressObject, wasm_hash: BytesObject, salt: BytesObject, constructor_args: VecObject) -> AddressObject"],
      "f": ["extend_contract_data_ttl_v2", "(k: Val, t: StorageType, extend_to: U32Val, min_extension: U32Val, max_extension: U32Val) -> Void"],
      "g": ["extend_contract_instance_and_code_ttl_v2", "(contract: AddressObject, extension_scope: ContractTtlExtension, extend_to: U32Val, min_extension: U32Val, max_extension: U32Val) -> Void"],
      "h": ["create_executable_tag", "(tag_string: StringObject) -> ExecutableTagObject"],
      "i": ["create_external_ref_contract", "(deployer: AddressObject, executable_owner: AddressObject, tag: ExecutableTagObject, salt: BytesObject, constructor_args: VecObject) -> AddressObject"],
      "j": ["update_current_contract_executable_ref", "(executable_owner: AddressObject, tag: ExecutableTagObject) -> Void"],
    },
  },
  "d": {
    name: "call",
    functions: {
      "_": ["call", "(contract: AddressObject, func: Symbol, args: VecObject) -> Val"],
      "0": ["try_call", "(contract: AddressObject, func: Symbol, args: VecObject) -> Val"],
    },
  },
  "b": {
    name: "buf",
    functions: {
      "_": ["serialize_to_bytes", "(v: Val) -> BytesObject"],
      "0": ["deserialize_from_bytes", "(b: BytesObject) -> Val"],
      "1": ["bytes_copy_to_linear_memory", "(b: BytesObject, b_pos: U32Val, lm_pos: U32Val, len: U32Val) -> Void"],
      "2": ["bytes_copy_from_linear_memory", "(b: BytesObject, b_pos: U32Val, lm_pos: U32Val, len: U32Val) -> BytesObject"],
      "3": ["bytes_new_from_linear_memory", "(lm_pos: U32Val, len: U32Val) -> BytesObject"],
      "4": ["bytes_new", "() -> BytesObject"],
      "5": ["bytes_put", "(b: BytesObject, i: U32Val, u: U32Val) -> BytesObject"],
      "6": ["bytes_get", "(b: BytesObject, i: U32Val) -> U32Val"],
      "7": ["bytes_del", "(b: BytesObject, i: U32Val) -> BytesObject"],
      "8": ["bytes_len", "(b: BytesObject) -> U32Val"],
      "9": ["bytes_push", "(b: BytesObject, u: U32Val) -> BytesObject"],
      "a": ["bytes_pop", "(b: BytesObject) -> BytesObject"],
      "b": ["bytes_front", "(b: BytesObject) -> U32Val"],
      "c": ["bytes_back", "(b: BytesObject) -> U32Val"],
      "d": ["bytes_insert", "(b: BytesObject, i: U32Val, u: U32Val) -> BytesObject"],
      "e": ["bytes_append", "(b1: BytesObject, b2: BytesObject) -> BytesObject"],
      "f": ["bytes_slice", "(b: BytesObject, start: U32Val, end: U32Val) -> BytesObject"],
      "g": ["string_copy_to_linear_memory", "(s: StringObject, s_pos: U32Val, lm_pos: U32Val, len: U32Val) -> Void"],
      "h": ["symbol_copy_to_linear_memory", "(s: SymbolObject, s_pos: U32Val, lm_pos: U32Val, len: U32Val) -> Void"],
      "i": ["string_new_from_linear_memory", "(lm_pos: U32Val, len: U32Val) -> StringObject"],
      "j": ["symbol_new_from_linear_memory", "(lm_pos: U32Val, len: U32Val) -> SymbolObject"],
      "k": ["string_len", "(s: StringObject) -> U32Val"],
      "l": ["symbol_len", "(s: SymbolObject) -> U32Val"],
      "m": ["symbol_index_in_linear_memory", "(sym: Symbol, slices_pos: U32Val, len: U32Val) -> U32Val"],
      "n": ["string_to_bytes", "(str: StringObject) -> BytesObject"],
      "o": ["bytes_to_string", "(bytes: BytesObject) -> StringObject"],
    },
  },
  "c": {
    name: "crypto",
    functions: {
      "_": ["compute_hash_sha256", "(x: BytesObject) -> BytesObject"],
      "0": ["verify_sig_ed25519", "(k: BytesObject, x: BytesObject, s: BytesObject) -> Void"],
      "1": ["compute_hash_keccak256", "(x: BytesObject) -> BytesObject"],
      "2": ["recover_key_ecdsa_secp256k1", "(msg_digest: BytesObject, signature: BytesObject, recovery_id: U32Val) -> BytesObject"],
      "3": ["verify_sig_ecdsa_secp256r1", "(public_key: BytesObject, msg_digest: BytesObject, signature: BytesObject) -> Void"],
      "4": ["bls12_381_check_g1_is_in_subgroup", "(point: BytesObject) -> Bool"],
      "5": ["bls12_381_g1_add", "(point1: BytesObject, point2: BytesObject) -> BytesObject"],
      "6": ["bls12_381_g1_mul", "(point: BytesObject, scalar: U256Val) -> BytesObject"],
      "7": ["bls12_381_g1_msm", "(vp: VecObject, vs: VecObject) -> BytesObject"],
      "8": ["bls12_381_map_fp_to_g1", "(fp: BytesObject) -> BytesObject"],
      "9": ["bls12_381_hash_to_g1", "(msg: BytesObject, dst: BytesObject) -> BytesObject"],
      "a": ["bls12_381_check_g2_is_in_subgroup", "(point: BytesObject) -> Bool"],
      "b": ["bls12_381_g2_add", "(point1: BytesObject, point2: BytesObject) -> BytesObject"],
      "c": ["bls12_381_g2_mul", "(point: BytesObject, scalar: U256Val) -> BytesObject"],
      "d": ["bls12_381_g2_msm", "(vp: VecObject, vs: VecObject) -> BytesObject"],
      "e": ["bls12_381_map_fp2_to_g2", "(fp2: BytesObject) -> BytesObject"],
      "f": ["bls12_381_hash_to_g2", "(msg: BytesObject, dst: BytesObject) -> BytesObject"],
      "g": ["bls12_381_multi_pairing_check", "(vp1: VecObject, vp2: VecObject) -> Bool"],
      "h": ["bls12_381_fr_add", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "i": ["bls12_381_fr_sub", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "j": ["bls12_381_fr_mul", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "k": ["bls12_381_fr_pow", "(lhs: U256Val, rhs: U64Val) -> U256Val"],
      "l": ["bls12_381_fr_inv", "(lhs: U256Val) -> U256Val"],
      "m": ["bn254_g1_add", "(point1: BytesObject, point2: BytesObject) -> BytesObject"],
      "n": ["bn254_g1_mul", "(point: BytesObject, scalar: U256Val) -> BytesObject"],
      "o": ["bn254_multi_pairing_check", "(vp1: VecObject, vp2: VecObject) -> Bool"],
      "p": ["poseidon_permutation", "(input: VecObject, field: Symbol, t: U32Val, d: U32Val, rounds_f: U32Val, rounds_p: U32Val, mds: VecObject, round_constants: VecObject) -> VecObject"],
      "q": ["poseidon2_permutation", "(input: VecObject, field: Symbol, t: U32Val, d: U32Val, rounds_f: U32Val, rounds_p: U32Val, mat_internal_diag_m_1: VecObject, round_constants: VecObject) -> VecObject"],
      "r": ["bn254_g1_msm", "(vp: VecObject, vs: VecObject) -> BytesObject"],
      "s": ["bn254_fr_add", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "t": ["bn254_fr_sub", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "u": ["bn254_fr_mul", "(lhs: U256Val, rhs: U256Val) -> U256Val"],
      "v": ["bn254_fr_pow", "(lhs: U256Val, rhs: U64Val) -> U256Val"],
      "w": ["bn254_fr_inv", "(lhs: U256Val) -> U256Val"],
      "x": ["bls12_381_g1_is_on_curve", "(point: BytesObject) -> Bool"],
      "y": ["bls12_381_g2_is_on_curve", "(point: BytesObject) -> Bool"],
      "z": ["bn254_g1_is_on_curve", "(point: BytesObject) -> Bool"],
    },
  },
  "a": {
    name: "address",
    functions: {
      "_": ["require_auth_for_args", "(address: AddressObject, args: VecObject) -> Void"],
      "0": ["require_auth", "(address: AddressObject) -> Void"],
      "1": ["strkey_to_address", "(strkey: Val) -> AddressObject"],
      "2": ["address_to_strkey", "(address: AddressObject) -> StringObject"],
      "3": ["authorize_as_curr_contract", "(auth_entires: VecObject) -> Void"],
      "4": ["get_address_from_muxed_address", "(muxed_address: MuxedAddressObject) -> AddressObject"],
      "5": ["get_id_from_muxed_address", "(muxed_address: MuxedAddressObject) -> U64Val"],
      "6": ["get_address_executable", "(address: AddressObject) -> Val"],
      "7": ["strkey_to_muxed_address", "(strkey: Val) -> Val"],
      "8": ["muxed_address_to_strkey", "(address: Val) -> StringObject"],
      "9": ["get_delegated_signers_for_current_auth_check", "() -> VecObject"],
      "a": ["delegate_account_auth", "(address: AddressObject) -> Void"],
    },
  },
  "t": {
    name: "test",
    functions: {
      "_": ["dummy0", "() -> Val"],
      "0": ["protocol_gated_dummy", "() -> Val"],
    },
  },
  "p": {
    name: "prng",
    functions: {
      "_": ["prng_reseed", "(seed: BytesObject) -> Void"],
      "0": ["prng_bytes_new", "(length: U32Val) -> BytesObject"],
      "1": ["prng_u64_in_inclusive_range", "(lo: u64, hi: u64) -> u64"],
      "2": ["prng_vec_shuffle", "(vec: VecObject) -> VecObject"],
    },
  },
};

/** Resolves a Soroban host import to its canonical name, or `null` if unknown. */
export function resolveHostFunction(module: string, field: string): { module: string; name: string; signature: string } | null {
  // Own-property checks keep crafted import names such as `__proto__` from
  // resolving to Object.prototype members.
  if (!Object.hasOwn(SOROBAN_HOST_MODULES, module)) return null;
  const hostModule = SOROBAN_HOST_MODULES[module];
  if (!Object.hasOwn(hostModule.functions, field)) return null;
  const [name, signature] = hostModule.functions[field];
  return { module: hostModule.name, name, signature };
}
