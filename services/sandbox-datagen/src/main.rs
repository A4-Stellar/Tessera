//! `tessera-sandbox-datagen` — deterministic synthetic data for the regulatory
//! sandbox.
//!
//! Generates (by default) 10,000 assets and 100,000 holders with realistic
//! distributions:
//!
//! * holder balances — Pareto type I (heavy tail: a few whales, many dust holders)
//! * asset prices   — log-normal (strictly positive, right-skewed)
//! * dividend gaps  — Poisson (independent arrivals measured in days)
//!
//! The output is plain CSV plus a `seed.sql` that loads it with `psql`, so the
//! seeder service needs no Rust and the files can be inspected by hand.

mod distributions;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use distributions::Rng;

const DEFAULT_ASSETS: usize = 10_000;
const DEFAULT_HOLDERS: usize = 100_000;
const B58: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

#[derive(Debug, Clone)]
struct Config {
    assets: usize,
    holders: usize,
    tenant: String,
    out: PathBuf,
    seed: u64,
    alpha: f64,
    price_mu: f64,
    price_sigma: f64,
    dividend_lambda: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            assets: DEFAULT_ASSETS,
            holders: DEFAULT_HOLDERS,
            tenant: "sandbox-default".to_owned(),
            out: PathBuf::from("./sandbox-data"),
            seed: 0x5445_5353_4552_41,
            alpha: 1.35,
            price_mu: 3.4, // exp(3.4) ~= $30 median asset price
            price_sigma: 0.65,
            dividend_lambda: 90.0,
        }
    }
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let (key, inline) = arg.split_once('=').unwrap_or((&arg, ""));
        let value = if inline.is_empty() {
            args.next()
                .ok_or_else(|| format!("missing value for {key}"))?
        } else {
            inline.to_owned()
        };
        match key {
            "--assets" => config.assets = value.parse().map_err(|_| "invalid --assets")?,
            "--holders" => config.holders = value.parse().map_err(|_| "invalid --holders")?,
            "--tenant" => config.tenant = value,
            "--out" => config.out = PathBuf::from(value),
            "--seed" => config.seed = value.parse().map_err(|_| "invalid --seed")?,
            "--alpha" => config.alpha = value.parse().map_err(|_| "invalid --alpha")?,
            "--price-mu" => config.price_mu = value.parse().map_err(|_| "invalid --price-mu")?,
            "--price-sigma" => {
                config.price_sigma = value.parse().map_err(|_| "invalid --price-sigma")?
            }
            "--dividend-lambda" => {
                config.dividend_lambda = value.parse().map_err(|_| "invalid --dividend-lambda")?
            }
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown option {other}\n\n{}", usage())),
        }
    }
    if config.assets == 0 || config.holders == 0 {
        return Err("--assets and --holders must be positive".to_owned());
    }
    if !config
        .tenant
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("--tenant must be 1-128 ASCII letters, digits, '-' or '_'".to_owned());
    }
    Ok(config)
}

fn usage() -> String {
    format!(
        "tessera-sandbox-datagen [options]\n\
         \x20 --assets N            number of synthetic assets (default {DEFAULT_ASSETS})\n\
         \x20 --holders N           number of synthetic holders (default {DEFAULT_HOLDERS})\n\
         \x20 --tenant ID           sandbox tenant id (default sandbox-default)\n\
         \x20 --out DIR             output directory (default ./sandbox-data)\n\
         \x20 --seed N              PRNG seed (default 0x54455353455241)\n\
         \x20 --alpha F             Pareto tail index for balances (default 1.35)\n\
         \x20 --price-mu F          log-normal mu for prices (default 3.4)\n\
         \x20 --price-sigma F       log-normal sigma for prices (default 0.65)\n\
         \x20 --dividend-lambda F   Poisson mean gap in days (default 90)"
    )
}

/// A Stellar-looking G-address (56 base32 chars). It is deliberately not a
/// valid keypair: sandbox data must never be usable on a real network.
fn fake_address(rng: &mut Rng) -> String {
    let mut out = String::with_capacity(56);
    out.push('G');
    for _ in 1..56 {
        out.push(B58[rng.next_u64() as usize % B58.len()] as char);
    }
    out
}

fn write_csv(path: &Path, header: &str, rows: impl Iterator<Item = String>) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(file, "{header}").map_err(|e| e.to_string())?;
    // Buffered writes matter at 100k rows: without them every row is a syscall.
    let mut writer = std::io::BufWriter::new(file);
    for row in rows {
        writeln!(writer, "{row}").map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}

const JURISDICTIONS: &[&str] = &["US", "GB", "DE", "FR", "SG", "AE", "CH", "JP"];
const ASSET_TYPES: &[&str] = &["real-estate", "invoice", "commodity", "debt", "equity"];

fn run(config: &Config) -> Result<(), String> {
    fs::create_dir_all(&config.out).map_err(|e| format!("{}: {e}", config.out.display()))?;
    let mut rng = Rng::new(config.seed);

    // Assets: log-normal prices. Prices are stored in cents as integers so the
    // CSV round-trips through Postgres NUMERIC/BIGINT without float drift.
    let assets_path = config.out.join("assets.csv");
    write_csv(
        &assets_path,
        "asset_id,name,issuer,asset_type,jurisdiction,price_cents",
        (1..=config.assets).map(|id| {
            let price = rng.log_normal(config.price_mu, config.price_sigma);
            let cents = (price * 100.0).round().clamp(1.0, 1e12) as u64;
            let kind = rng.choose(ASSET_TYPES);
            let jurisdiction = rng.choose(JURISDICTIONS);
            format!(
                "{id},\"Synthetic Asset {id}\",\"Synthetic Issuer {}\",{kind},{jurisdiction},{cents}",
                id % 1_000
            )
        }),
    )?;

    // Holders: Pareto balances. Balances are integer "units" (stored as i128
    // on-chain); x_min=100 keeps dust holders meaningful while alpha=1.35
    // produces a realistic concentration curve (top 1% holds ~30-40%).
    let holders_path = config.out.join("holders.csv");
    write_csv(
        &holders_path,
        "holder_id,asset_id,address,balance,jurisdiction,joined_day",
        (1..=config.holders).map(|id| {
            let asset_id = 1 + rng.next_u64() as usize % config.assets;
            let balance = rng.pareto(100.0, config.alpha).round().clamp(1.0, 1e15) as u64;
            let address = fake_address(&mut rng);
            let jurisdiction = rng.choose(JURISDICTIONS);
            let joined_day = rng.poisson(config.dividend_lambda);
            format!("{id},{asset_id},{address},{balance},{jurisdiction},{joined_day}")
        }),
    )?;

    // Dividend schedule: the gap between paydays is Poisson-distributed.
    let dividends_path = config.out.join("dividends.csv");
    write_csv(
        &dividends_path,
        "dividend_id,asset_id,amount_cents,interval_days",
        (1..=config.assets).map(|id| {
            let amount = (rng.log_normal(8.0, 1.0) * 100.0).round().clamp(1.0, 1e12) as u64;
            let interval = rng.poisson(config.dividend_lambda).clamp(1, 3650);
            format!("{id},{id},{amount},{interval}")
        }),
    )?;

    // psql meta-commands so the seeder can load with a single `psql -f`.
    // Data lands in the unscoped template tables; each tenant gets its own copy
    // on first use / reset via INSERT ... SELECT. The tenant_id column is left
    // out of the CSVs for the template load but kept in the files so operators
    // can audit which tenant requested the generation.
    let seed_path = config.out.join("seed.sql");
    let load = format!(
        "-- Generated by tessera-sandbox-datagen. Requested by tenant: {}\n\
         \\copy sandbox_template_assets (asset_id, name, issuer, asset_type, jurisdiction, price_cents) FROM '{}' WITH (FORMAT csv, HEADER true)\n\
         \\copy sandbox_template_holders (holder_id, asset_id, address, balance, jurisdiction, joined_day) FROM '{}' WITH (FORMAT csv, HEADER true)\n\
         \\copy sandbox_template_dividends (dividend_id, asset_id, amount_cents, interval_days) FROM '{}' WITH (FORMAT csv, HEADER true)\n\
         ANALYZE sandbox_template_assets;\nANALYZE sandbox_template_holders;\nANALYZE sandbox_template_dividends;\n",
        config.tenant,
        assets_path.display(),
        holders_path.display(),
        dividends_path.display(),
    );
    fs::write(&seed_path, load).map_err(|e| format!("{}: {e}", seed_path.display()))?;

    println!(
        "generated {} assets, {} holders, {} dividend schedules for tenant '{}' in {}",
        config.assets,
        config.holders,
        config.assets,
        config.tenant,
        config.out.display()
    );
    Ok(())
}

fn main() {
    let config = match parse_args() {
        Ok(config) => config,
        Err(message) => {
            if message.starts_with("tessera-sandbox-datagen") {
                println!("{message}");
            } else {
                eprintln!("error: {message}");
            }
            std::process::exit(if message.starts_with("tessera-sandbox-datagen") {
                0
            } else {
                2
            });
        }
    };
    if let Err(error) = run(&config) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_requested_row_counts() {
        let dir = std::env::temp_dir().join(format!("tessera-datagen-test-{}", std::process::id()));
        let config = Config {
            assets: 17,
            holders: 53,
            tenant: "test-tenant".to_owned(),
            out: dir.clone(),
            ..Config::default()
        };
        run(&config).expect("generation must succeed");
        assert_eq!(
            fs::read_to_string(dir.join("assets.csv"))
                .unwrap()
                .lines()
                .count(),
            18
        );
        assert_eq!(
            fs::read_to_string(dir.join("holders.csv"))
                .unwrap()
                .lines()
                .count(),
            54
        );
        assert!(fs::read_to_string(dir.join("seed.sql"))
            .unwrap()
            .contains("\\copy"));
        let _ = fs::remove_dir_all(dir);
    }
}
