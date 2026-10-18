//! `tessera-zk-aggregator` — off-chain KYC proof aggregation (issue #317).
//!
//! ```text
//! tessera-zk-aggregator setup     --out KEYS [--slots 32] [--depth 20]
//! tessera-zk-aggregator issue     --input creds.csv --queue QUEUE [--depth 20]
//! tessera-zk-aggregator aggregate --keys KEYS --queue QUEUE --root HEX --out bundle.json [--valid-until N]
//! tessera-zk-aggregator verify    --keys KEYS --bundle bundle.json
//! ```
//!
//! See `README.md` in this directory for the full workflow.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ark_bn254::{Bn254, Fr};
use ark_groth16::{Groth16, PreparedVerifyingKey, ProvingKey};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;
use rand_chacha::rand_core::{OsRng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use tessera_zk_aggregator::{
    commitment,
    encode::{
        fr_from_hex, fr_to_hex, subject_field, ProofBundle, SorobanProof, SorobanVerifyingKey,
    },
    poseidon_config, Accumulator, AggregationCircuit, Credential, CredentialTree, DEFAULT_DEPTH,
    DEFAULT_SLOTS,
};

#[derive(Serialize, Deserialize)]
struct CircuitShape {
    slots: usize,
    depth: usize,
}

type CliResult = Result<(), String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    let flags = match parse_flags(rest) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let result = match cmd.as_str() {
        "setup" => setup(&flags),
        "issue" => issue(&flags),
        "aggregate" => aggregate(&flags),
        "verify" => verify(&flags),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{}", usage())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage:
  tessera-zk-aggregator setup     --out KEYS [--slots 32] [--depth 20]
  tessera-zk-aggregator issue     --input creds.csv --queue QUEUE [--depth 20]
  tessera-zk-aggregator aggregate --keys KEYS --queue QUEUE --root HEX --out bundle.json [--valid-until N]
  tessera-zk-aggregator verify    --keys KEYS --bundle bundle.json"
}

fn parse_flags(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let name = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument `{flag}`"))?;
        let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
        out.insert(name.to_string(), value.clone());
    }
    Ok(out)
}

fn required<'a>(flags: &'a HashMap<String, String>, name: &str) -> Result<&'a str, String> {
    flags
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing --{name}"))
}

fn number<T: std::str::FromStr>(
    flags: &HashMap<String, String>,
    name: &str,
    default: T,
) -> Result<T, String> {
    flags.get(name).map_or(Ok(default), |v| {
        v.parse()
            .map_err(|_| format!("--{name}: invalid number `{v}`"))
    })
}

fn io<T>(r: std::io::Result<T>, what: &Path) -> Result<T, String> {
    r.map_err(|e| format!("{}: {e}", what.display()))
}

/// Generate the circuit's Groth16 keys. This is a single-party setup: whoever
/// runs it learns the toxic waste and could forge proofs. Production keys
/// must come from a multi-party ceremony (see README).
fn setup(flags: &HashMap<String, String>) -> CliResult {
    let out = PathBuf::from(required(flags, "out")?);
    let slots = number(flags, "slots", DEFAULT_SLOTS)?;
    let depth = number(flags, "depth", DEFAULT_DEPTH)?;
    if slots == 0 || slots > DEFAULT_SLOTS {
        return Err(format!("--slots must be 1..={DEFAULT_SLOTS}"));
    }
    io(fs::create_dir_all(&out), &out)?;

    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let mut rng = ChaCha20Rng::from_seed(seed);
    eprintln!("generating keys for {slots} slots, depth {depth} (single-party setup)…");
    let circuit = AggregationCircuit::blank(poseidon_config(), slots, depth);
    let (pk, vk) = Groth16::<Bn254>::circuit_specific_setup(circuit, &mut rng)
        .map_err(|e| format!("setup failed: {e}"))?;

    // Uncompressed, so `aggregate` can load it without decompressing and
    // subgroup-checking ~10^6 points (minutes at the default shape). The
    // file is produced locally and must be protected like the setup itself.
    let mut pk_bytes = Vec::new();
    pk.serialize_uncompressed(&mut pk_bytes)
        .map_err(|e| format!("serialize proving key: {e}"))?;
    let pk_path = out.join("proving_key.bin");
    io(fs::write(&pk_path, pk_bytes), &pk_path)?;
    write_json(
        &out.join("verifying_key.json"),
        &SorobanVerifyingKey::new(&vk, slots),
    )?;
    write_json(&out.join("circuit.json"), &CircuitShape { slots, depth })?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Build an issuer's credential tree from `subject,expiry_ledger` lines and
/// drop one credential file per subject into the queue.
fn issue(flags: &HashMap<String, String>) -> CliResult {
    let input = PathBuf::from(required(flags, "input")?);
    let queue = PathBuf::from(required(flags, "queue")?);
    let depth = number(flags, "depth", DEFAULT_DEPTH)?;

    let text = io(fs::read_to_string(&input), &input)?;
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (subject, expiry) = line
            .split_once(',')
            .ok_or_else(|| format!("line {}: expected `subject,expiry_ledger`", n + 1))?;
        let expiry: u32 = expiry
            .trim()
            .parse()
            .map_err(|_| format!("line {}: bad expiry", n + 1))?;
        entries.push((subject.trim().to_string(), expiry));
    }

    let tree =
        CredentialTree::build(&poseidon_config(), depth, entries).map_err(|e| e.to_string())?;
    io(fs::create_dir_all(&queue), &queue)?;
    let creds = tree.credentials();
    for cred in &creds {
        write_json(
            &queue.join(format!("{:08}-{}.json", cred.index, cred.subject)),
            cred,
        )?;
    }
    println!("{}", fr_to_hex(&tree.root()));
    eprintln!(
        "issued {} credentials into {}; publish the root above with set_zk_issuer_root",
        creds.len(),
        queue.display()
    );
    Ok(())
}

fn load_keys(keys: &Path) -> Result<(CircuitShape, ProvingKey<Bn254>), String> {
    let shape: CircuitShape = read_json(&keys.join("circuit.json"))?;
    let pk_path = keys.join("proving_key.bin");
    let bytes = io(fs::read(&pk_path), &pk_path)?;
    // Trusted local artifact written by `setup` (see there): skip validation.
    let pk = ProvingKey::<Bn254>::deserialize_uncompressed_unchecked(&bytes[..])
        .map_err(|e| format!("{}: {e}", pk_path.display()))?;
    Ok((shape, pk))
}

/// Drain up to `slots` credentials from the queue, prove them as one batch,
/// and move them to `QUEUE/done`. Credentials that fail their individual
/// check are moved to `QUEUE/rejected` and left out of the batch.
fn aggregate(flags: &HashMap<String, String>) -> CliResult {
    let keys = PathBuf::from(required(flags, "keys")?);
    let queue = PathBuf::from(required(flags, "queue")?);
    let out = PathBuf::from(required(flags, "out")?);
    let root = fr_from_hex(required(flags, "root")?).ok_or("--root: not a 32-byte scalar")?;
    let valid_until: Option<u32> = flags
        .get("valid-until")
        .map(|v| v.parse().map_err(|_| "--valid-until: invalid number"))
        .transpose()?;

    let (shape, pk) = load_keys(&keys)?;
    let config = poseidon_config();

    let mut files: Vec<PathBuf> = io(fs::read_dir(&queue), &queue)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();

    let mut batch: Vec<(PathBuf, Credential)> = Vec::new();
    for path in files {
        if batch.len() == shape.slots {
            break;
        }
        let cred: Credential = read_json(&path)?;
        // Individual check against the root and a self-only batch.
        let single = Accumulator::build(&config, root, shape.depth, 1, vec![cred.clone()], None);
        let duplicate = batch.iter().any(|(_, c)| c.subject == cred.subject);
        match single {
            Ok(_) if !duplicate => batch.push((path, cred)),
            Ok(_) => move_to(&queue, "rejected", &path, "duplicate subject")?,
            Err(e) => move_to(&queue, "rejected", &path, &e.to_string())?,
        }
    }

    let creds: Vec<Credential> = batch.iter().map(|(_, c)| c.clone()).collect();
    let acc = Accumulator::build(&config, root, shape.depth, shape.slots, creds, valid_until)
        .map_err(|e| e.to_string())?;
    eprintln!(
        "proving {} credentials (valid until ledger {})…",
        acc.credentials.len(),
        acc.valid_until
    );
    let circuit = AggregationCircuit::from_accumulator(config, shape.depth, &acc);
    let mut rng = ChaCha20Rng::from_seed({
        let mut s = [0u8; 32];
        OsRng.fill_bytes(&mut s);
        s
    });
    let proof = Groth16::<Bn254>::prove(&pk, circuit, &mut rng)
        .map_err(|e| format!("proving failed: {e}"))?;

    let pvk: PreparedVerifyingKey<Bn254> = pk.vk.clone().into();
    let ok = Groth16::<Bn254>::verify_with_processed_vk(&pvk, &acc.public_inputs(), &proof)
        .map_err(|e| format!("self-check failed: {e}"))?;
    if !ok {
        return Err("self-check failed: proof does not verify".into());
    }

    let bundle = ProofBundle {
        issuer_root: fr_to_hex(&acc.root),
        valid_until: acc.valid_until,
        subjects: acc.subjects.clone(),
        proof: SorobanProof::from(&proof),
    };
    write_json(&out, &bundle)?;
    for (path, _) in &batch {
        move_to(&queue, "done", path, "aggregated")?;
    }
    println!("wrote {}", out.display());
    Ok(())
}

/// Natively re-check a bundle, recomputing the public inputs from its
/// subjects exactly as the contract does.
fn verify(flags: &HashMap<String, String>) -> CliResult {
    let keys = PathBuf::from(required(flags, "keys")?);
    let bundle: ProofBundle = read_json(Path::new(required(flags, "bundle")?))?;
    let shape: CircuitShape = read_json(&keys.join("circuit.json"))?;
    let vk = read_json::<SorobanVerifyingKey>(&keys.join("verifying_key.json"))?
        .to_arkworks()
        .ok_or("verifying_key.json: invalid points")?;

    let root = fr_from_hex(&bundle.issuer_root).ok_or("bundle: bad issuer_root")?;
    if bundle.subjects.len() > shape.slots {
        return Err("bundle has more subjects than the circuit has slots".into());
    }
    let subjects = bundle
        .subjects
        .iter()
        .map(|s| subject_field(s).ok_or_else(|| format!("bundle: bad subject {s}")))
        .collect::<Result<Vec<Fr>, _>>()?;
    let inputs = vec![commitment(
        &root,
        bundle.valid_until,
        subjects.len() as u32,
        &subjects,
        shape.slots,
    )];

    let proof = bundle
        .proof
        .to_arkworks()
        .ok_or("bundle: invalid proof points")?;
    let ok = Groth16::<Bn254>::verify(&vk, &inputs, &proof).map_err(|e| e.to_string())?;
    if ok {
        println!(
            "valid: {} subjects until ledger {}",
            bundle.subjects.len(),
            bundle.valid_until
        );
        Ok(())
    } else {
        Err("proof does not verify".into())
    }
}

fn move_to(queue: &Path, sub: &str, path: &Path, why: &str) -> CliResult {
    let dir = queue.join(sub);
    io(fs::create_dir_all(&dir), &dir)?;
    let target = dir.join(path.file_name().unwrap_or_default());
    io(fs::rename(path, &target), path)?;
    if sub == "rejected" {
        eprintln!("rejected {}: {why}", path.display());
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> CliResult {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    io(fs::write(path, text + "\n"), path)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = io(fs::read_to_string(path), path)?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}
