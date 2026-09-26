use std::io::{self, BufRead, Write};

use crate::error::CliError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectChoices {
    pub asset_type: AssetType,
    pub compliance_model: ComplianceModel,
    pub dividend_strategy: DividendStrategy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetType {
    RealEstate,
    Invoice,
    Commodity,
    Debt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComplianceModel {
    Kyc,
    Allowlist,
    Permissionless,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DividendStrategy {
    Proportional,
    Fixed,
    Tiered,
    None,
}

impl AssetType {
    pub const ALL: [Self; 4] = [Self::RealEstate, Self::Invoice, Self::Commodity, Self::Debt];

    pub fn parse_value(value: &str) -> Result<Self, String> {
        match normalize(value).as_str() {
            "1" | "realestate" | "property" | "re" => Ok(Self::RealEstate),
            "2" | "invoice" | "invoices" => Ok(Self::Invoice),
            "3" | "commodity" | "commodities" => Ok(Self::Commodity),
            "4" | "debt" => Ok(Self::Debt),
            _ => Err("expected Real Estate, Invoice, Commodity, or Debt".to_owned()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::RealEstate => "Real Estate",
            Self::Invoice => "Invoice",
            Self::Commodity => "Commodity",
            Self::Debt => "Debt",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::RealEstate => "real-estate",
            Self::Invoice => "invoice",
            Self::Commodity => "commodity",
            Self::Debt => "debt",
        }
    }

    pub fn choice_help() -> &'static str {
        "Real Estate, Invoice, Commodity, or Debt"
    }
}

impl ComplianceModel {
    pub const ALL: [Self; 4] = [
        Self::Kyc,
        Self::Allowlist,
        Self::Permissionless,
        Self::Hybrid,
    ];

    pub fn parse_value(value: &str) -> Result<Self, String> {
        match normalize(value).as_str() {
            "1" | "kyc" | "knowyourcustomer" => Ok(Self::Kyc),
            "2" | "allowlist" | "allow" | "whitelist" => Ok(Self::Allowlist),
            "3" | "permissionless" | "none" | "open" => Ok(Self::Permissionless),
            "4" | "hybrid" | "hybridkyc" | "hybridkycallowlist" | "jurisdiction"
            | "jurisdictionaware" => Ok(Self::Hybrid),
            _ => Err("expected KYC, Allowlist, Permissionless, or Hybrid".to_owned()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Kyc => "KYC",
            Self::Allowlist => "Allowlist",
            Self::Permissionless => "Permissionless",
            Self::Hybrid => "Hybrid KYC + allowlist",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Kyc => "kyc",
            Self::Allowlist => "allowlist",
            Self::Permissionless => "permissionless",
            Self::Hybrid => "hybrid",
        }
    }

    pub fn choice_help() -> &'static str {
        "KYC, Allowlist, Permissionless, or Hybrid"
    }
}

impl DividendStrategy {
    pub const ALL: [Self; 4] = [Self::Proportional, Self::Fixed, Self::Tiered, Self::None];

    pub fn parse_value(value: &str) -> Result<Self, String> {
        match normalize(value).as_str() {
            "1" | "proportional" | "prorata" => Ok(Self::Proportional),
            "2" | "fixed" | "fixedperholder" | "flat" | "equal" => Ok(Self::Fixed),
            "3" | "tiered" | "tier" => Ok(Self::Tiered),
            "0" | "none" | "disabled" | "off" => Ok(Self::None),
            _ => Err("expected Proportional, Fixed, Tiered, or None".to_owned()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Proportional => "Proportional",
            Self::Fixed => "Fixed per holder",
            Self::Tiered => "Tiered",
            Self::None => "None",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Proportional => "proportional",
            Self::Fixed => "fixed",
            Self::Tiered => "tiered",
            Self::None => "none",
        }
    }

    pub fn choice_help() -> &'static str {
        "Proportional, Fixed per holder, Tiered, or None"
    }

    pub fn preview_for(self, amount: i128, balance: i128, total_supply: i128) -> i128 {
        if amount <= 0 || balance <= 0 || total_supply <= 0 {
            return 0;
        }
        match self {
            Self::Proportional => amount.saturating_mul(balance) / total_supply,
            Self::Fixed => amount,
            Self::Tiered => {
                let tier_cap = total_supply / 10;
                let tier_balance = if tier_cap > 0 && balance > tier_cap {
                    tier_cap
                } else {
                    balance
                };
                amount.saturating_mul(tier_balance) / total_supply
            }
            Self::None => 0,
        }
    }
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' ', '-', '+'], "")
}

#[derive(Debug, Clone, clap::Args)]
pub struct InitArgs {
    #[arg(value_name = "NAME", help = "Project directory and Cargo package name")]
    pub name: String,

    #[arg(
        long = "asset-type",
        visible_alias = "type",
        short = 'a',
        value_name = "TYPE",
        help = "Asset type for noninteractive generation"
    )]
    pub asset_type: Option<String>,

    #[arg(
        long = "compliance-model",
        visible_alias = "compliance",
        short = 'c',
        value_name = "MODEL",
        help = "Compliance model for noninteractive generation"
    )]
    pub compliance_model: Option<String>,

    #[arg(
        long = "dividend-strategy",
        visible_aliases = ["dividend", "dividends"],
        short = 'd',
        value_name = "STRATEGY",
        help = "Dividend strategy for noninteractive generation"
    )]
    pub dividend_strategy: Option<String>,

    #[arg(
        long = "non-interactive",
        visible_alias = "no-interactive",
        help = "Require all selection flags and never read stdin"
    )]
    pub non_interactive: bool,

    #[arg(
        long = "defaults",
        visible_alias = "yes",
        help = "Use standard selections without prompting"
    )]
    pub defaults: bool,

    #[arg(
        long = "output-dir",
        visible_alias = "output",
        short = 'o',
        value_name = "DIRECTORY",
        help = "Existing directory that will contain the new project"
    )]
    pub output_dir: Option<std::path::PathBuf>,
}

const ASSET_OPTIONS: [(&str, &str); 4] = [
    ("1", "Real Estate"),
    ("2", "Invoice"),
    ("3", "Commodity"),
    ("4", "Debt"),
];

const COMPLIANCE_OPTIONS: [(&str, &str); 4] = [
    ("1", "KYC"),
    ("2", "Allowlist"),
    ("3", "Permissionless"),
    ("4", "Hybrid KYC + allowlist"),
];

const DIVIDEND_OPTIONS: [(&str, &str); 4] = [
    ("1", "Proportional"),
    ("2", "Fixed per holder"),
    ("3", "Tiered"),
    ("4", "None"),
];

struct ChoiceSpec<T: Copy> {
    flag: &'static str,
    title: &'static str,
    options: &'static [(&'static str, &'static str)],
    parser: fn(&str) -> Result<T, String>,
    help: fn() -> &'static str,
    default: T,
}

pub fn resolve_choices(
    args: &InitArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<ProjectChoices, CliError> {
    let asset_type = resolve_one(
        ChoiceSpec {
            flag: "asset-type",
            title: "Asset type",
            options: &ASSET_OPTIONS,
            parser: AssetType::parse_value,
            help: AssetType::choice_help,
            default: AssetType::RealEstate,
        },
        args.asset_type.as_deref(),
        args.non_interactive,
        args.defaults,
        input,
        output,
    )?;
    let compliance_model = resolve_one(
        ChoiceSpec {
            flag: "compliance-model",
            title: "Compliance model",
            options: &COMPLIANCE_OPTIONS,
            parser: ComplianceModel::parse_value,
            help: ComplianceModel::choice_help,
            default: ComplianceModel::Kyc,
        },
        args.compliance_model.as_deref(),
        args.non_interactive,
        args.defaults,
        input,
        output,
    )?;
    let dividend_strategy = resolve_one(
        ChoiceSpec {
            flag: "dividend-strategy",
            title: "Dividend strategy",
            options: &DIVIDEND_OPTIONS,
            parser: DividendStrategy::parse_value,
            help: DividendStrategy::choice_help,
            default: DividendStrategy::Proportional,
        },
        args.dividend_strategy.as_deref(),
        args.non_interactive,
        args.defaults,
        input,
        output,
    )?;

    Ok(ProjectChoices {
        asset_type,
        compliance_model,
        dividend_strategy,
    })
}

fn resolve_one<T: Copy>(
    spec: ChoiceSpec<T>,
    supplied: Option<&str>,
    non_interactive: bool,
    use_defaults: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<T, CliError> {
    let ChoiceSpec {
        flag,
        title,
        options,
        parser,
        help,
        default,
    } = spec;
    if let Some(value) = supplied {
        return parser(value).map_err(|_| CliError::InvalidChoice {
            field: flag,
            value: value.to_owned(),
            choices: help(),
        });
    }
    if use_defaults {
        return Ok(default);
    }
    if non_interactive {
        return Err(CliError::MissingNonInteractive { field: flag });
    }
    prompt_choice(title, options, input, output, parser, help)
}

fn prompt_choice<T>(
    title: &str,
    options: &[(&str, &str)],
    input: &mut impl BufRead,
    output: &mut impl Write,
    parser: fn(&str) -> Result<T, String>,
    help: fn() -> &'static str,
) -> Result<T, CliError> {
    writeln!(output, "{title}:").map_err(prompt_io)?;
    for (number, label) in options {
        writeln!(output, "  {number}) {label}").map_err(prompt_io)?;
    }
    write!(output, "Select {title} [1-{}]: ", options.len()).map_err(prompt_io)?;
    output.flush().map_err(prompt_io)?;

    for _ in 0..32 {
        let mut line = String::new();
        let read = input.read_line(&mut line).map_err(prompt_io)?;
        if read == 0 {
            return Err(CliError::Prompt(format!(
                "input ended while selecting {title}"
            )));
        }
        let value = line.trim();
        let value = if value.is_empty() {
            options[0].0
        } else {
            value
        };
        match parser(value) {
            Ok(choice) => return Ok(choice),
            Err(_) => {
                writeln!(output, "Invalid selection. Choose one of: {}", help())
                    .map_err(prompt_io)?;
                write!(output, "Select {title} [1-{}]: ", options.len()).map_err(prompt_io)?;
                output.flush().map_err(prompt_io)?;
            }
        }
    }
    Err(CliError::Prompt(format!(
        "too many invalid selections for {title}"
    )))
}

fn prompt_io(error: io::Error) -> CliError {
    CliError::PromptIo { source: error }
}
