/**
 * Decision rules for the regulatory exemption checker. Thresholds reflect the
 * rules in force on {@link RULES_AS_OF}: SEC Regulation D (Rule 506(b)/(c)),
 * Regulation A (as amended effective March 2021), Regulation S (Rule 903),
 * Rule 144(d) holding periods, the EU Prospectus Regulation (EU) 2017/1129 as
 * amended by the Listing Act (Regulation (EU) 2024/2809, small-offer threshold
 * applicable from 5 June 2026) and MiCA (Regulation (EU) 2023/1114).
 *
 * The output is an educational starting point, not legal advice.
 */

export const RULES_AS_OF = "2026-10-16";

export type IssuerLocation = "us" | "canada" | "eu" | "other";
export type Market = "us" | "eu" | "other";
export type Instrument = "equity" | "debt" | "crypto-asset";
export type StableValue = "none" | "single-currency" | "other-assets";

export interface ExemptionAnswers {
  issuerLocation: IssuerLocation | null;
  markets: Market[];
  instrument: Instrument | null;
  /** EU crypto-assets only: whether the token references a currency or other assets to keep a stable value. */
  stableValue: StableValue | null;
  usInvestors: "accredited-only" | "includes-non-accredited" | null;
  /** US offerings: whether the offering will be generally solicited (public advertising, open websites, social media). */
  generalSolicitation: boolean | null;
  usRaiseUsd: number | null;
  /** US offerings: Exchange Act reporting company status (drives Rule 144 and Regulation S periods). */
  reportingIssuer: boolean | null;
  /** US offerings by US/Canadian issuers: investment company/BDC, blank-check/SPAC or fractional oil, gas or mineral rights issuer. */
  regAExcludedIssuer: boolean | null;
  euInvestors: "qualified-only" | "retail" | null;
  euRaiseEur: number | null;
  /** EU: offer reaches fewer than 150 persons (other than qualified investors) in every Member State. */
  euFewerThan150PerState: boolean | null;
  /** EU securities: every investor commits at least EUR 100,000 (or units are denominated at EUR 100,000+). */
  euMinimumTicket100k: boolean | null;
  /** EU crypto-assets: the offeror intends to seek admission to trading on a crypto-asset trading platform. */
  seeksAdmissionToTrading: boolean | null;
  /** EU asset-referenced tokens: the average outstanding value stays at or below EUR 5,000,000 over every 12 months. */
  artOutstandingUnder5m: boolean | null;
}

export const EMPTY_ANSWERS: ExemptionAnswers = {
  issuerLocation: null,
  markets: [],
  instrument: null,
  stableValue: null,
  usInvestors: null,
  generalSolicitation: null,
  usRaiseUsd: null,
  reportingIssuer: null,
  regAExcludedIssuer: null,
  euInvestors: null,
  euRaiseEur: null,
  euFewerThan150PerState: null,
  euMinimumTicket100k: null,
  seeksAdmissionToTrading: null,
  artOutstandingUnder5m: null,
};

export interface SetupStep {
  text: string;
  guide: GuideLink;
}

export type FrameworkStatus = "eligible" | "required" | "ineligible";

export interface GuideLink {
  label: string;
  href: string;
}

export interface FrameworkResult {
  id: string;
  name: string;
  jurisdiction: "US" | "EU";
  status: FrameworkStatus;
  /** The best-fitting path for its jurisdiction. */
  recommended: boolean;
  reasons: string[];
  requirements: string[];
  /** How to configure the Tessera contracts for this framework. */
  setup: SetupStep[];
  source: GuideLink;
}

export interface Assessment {
  results: FrameworkResult[];
  notes: string[];
}

export const LIMITS = {
  rule506bNonAccreditedPurchasers: 35,
  regATier1Usd: 20_000_000,
  regATier1SellingHoldersUsd: 6_000_000,
  regATier2Usd: 75_000_000,
  regATier2SellingHoldersUsd: 22_500_000,
  prospectusOutOfScopeEur: 1_000_000,
  prospectusSmallOfferEur: 12_000_000,
  prospectusSmallOfferNationalEur: 5_000_000,
  prospectusMinimumTicketEur: 100_000,
  micaSmallOfferEur: 1_000_000,
  micaArtOutstandingEur: 5_000_000,
  euPersonsPerMemberState: 150,
} as const;

/** Stellar's 5-second target ledger close time gives 17,280 ledgers per day. */
const LEDGERS_PER_DAY = 17_280;

const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });
const eur = new Intl.NumberFormat("en-US", { style: "currency", currency: "EUR", maximumFractionDigits: 0 });
const count = new Intl.NumberFormat("en-US");

function lockupStep(period: string, days: number): string {
  return `Set each purchaser's holding period with asset-token \`set_lockup(admin, holder, unlock_ledger)\`: ${period} ≈ ${count.format(
    days * LEDGERS_PER_DAY
  )} ledgers after purchase at the 5-second target close time (verify against ledger close times before relying on it).`;
}

const GUIDES = {
  compliance: { label: "Compliance guide", href: "/docs/compliance-guide" },
  complianceContract: { label: "Compliance contract reference", href: "/docs/contracts/compliance" },
  assetToken: { label: "Asset token contract reference", href: "/docs/contracts/asset-token" },
  ledgers: { label: "Time & ledgers", href: "/docs/time-and-ledgers" },
} satisfies Record<string, GuideLink>;

const SOURCES = {
  rule506b: { label: "SEC — Rule 506(b)", href: "https://www.sec.gov/resources-small-businesses/exempt-offerings/private-placements-rule-506b" },
  rule506c: { label: "SEC — Rule 506(c)", href: "https://www.sec.gov/resources-small-businesses/exempt-offerings/general-solicitation-rule-506c" },
  regA: { label: "SEC — Regulation A guide", href: "https://www.sec.gov/resources-small-businesses/small-business-compliance-guides/regulation-guidance-issuers" },
  regS: { label: "17 CFR 230.903", href: "https://www.law.cornell.edu/cfr/text/17/230.903" },
  prospectus: { label: "Regulation (EU) 2017/1129", href: "https://eur-lex.europa.eu/eli/reg/2017/1129/oj/eng" },
  micaScope: { label: "MiCA Article 2", href: "https://www.esma.europa.eu/publications-and-data/interactive-single-rulebook/mica/article-2-scope" },
  micaArt4: { label: "MiCA Article 4", href: "https://www.esma.europa.eu/publications-and-data/interactive-single-rulebook/mica/article-4-offers-public-crypto-assets-other" },
  micaArt16: { label: "MiCA Article 16", href: "https://www.esma.europa.eu/publications-and-data/interactive-single-rulebook/mica/article-16-authorisation" },
  micaArt48: { label: "MiCA Article 48", href: "https://www.esma.europa.eu/publications-and-data/interactive-single-rulebook/mica/article-48-requirements-offer-public-or" },
} satisfies Record<string, GuideLink>;

function holdingPeriodStep(answers: ExemptionAnswers): string {
  return answers.reportingIssuer
    ? lockupStep("Rule 144's six-month holding period for reporting issuers", 184)
    : lockupStep("Rule 144's one-year holding period for non-reporting issuers", 366);
}

function evaluateRegD(answers: ExemptionAnswers): FrameworkResult[] {
  const nonAccredited = answers.usInvestors === "includes-non-accredited";
  const solicited = answers.generalSolicitation === true;
  const allowlist = "Allowlist each purchaser with compliance `add_to_allowlist` only after KYC and accreditation checks, with `expires_at` matching your re-verification cycle.";

  const rule506b: FrameworkResult = {
    id: "reg-d-506b",
    name: "Regulation D — Rule 506(b)",
    jurisdiction: "US",
    status: solicited ? "ineligible" : "eligible",
    recommended: false,
    reasons: solicited
      ? ["Rule 506(b) prohibits general solicitation and advertising."]
      : [
          "No general solicitation is planned, and there is no cap on the amount raised.",
          ...(nonAccredited
            ? [`Non-accredited purchasers are allowed: no more than ${LIMITS.rule506bNonAccreditedPurchasers} in any 90-calendar-day period, and each must be financially sophisticated.`]
            : []),
        ],
    requirements: [
      "File Form D within 15 days after the first sale; states may require notice filings and fees.",
      ...(nonAccredited
        ? ["Give non-accredited purchasers Regulation A-level disclosure, including the financial statement information Rule 506 specifies."]
        : []),
      "Securities sold are restricted securities, and the bad-actor disqualification provisions apply.",
    ],
    setup: [
      { text: allowlist, guide: GUIDES.compliance },
      { text: holdingPeriodStep(answers), guide: GUIDES.assetToken },
    ],
    source: SOURCES.rule506b,
  };

  const rule506c: FrameworkResult = {
    id: "reg-d-506c",
    name: "Regulation D — Rule 506(c)",
    jurisdiction: "US",
    status: nonAccredited ? "ineligible" : "eligible",
    recommended: false,
    reasons: nonAccredited
      ? ["Every purchaser in a Rule 506(c) offering must be an accredited investor."]
      : ["General solicitation is permitted when every purchaser is accredited and verified; there is no cap on the amount raised."],
    requirements: [
      "Take reasonable steps to verify each purchaser's accredited status. Under the SEC staff's March 12, 2025 no-action letter, minimum investments of $200,000 (natural persons) or $1,000,000 (entities), plus written representations of accredited status and of no third-party financing, can satisfy this.",
      "File Form D within 15 days after the first sale.",
      "Securities sold are restricted securities, and the bad-actor disqualification provisions apply.",
    ],
    setup: [
      { text: allowlist, guide: GUIDES.compliance },
      { text: holdingPeriodStep(answers), guide: GUIDES.assetToken },
    ],
    source: SOURCES.rule506c,
  };

  return [rule506b, rule506c];
}

function evaluateRegA(answers: ExemptionAnswers): FrameworkResult[] {
  const raise = answers.usRaiseUsd ?? 0;
  const issuerBlocked =
    answers.issuerLocation !== "us" && answers.issuerLocation !== "canada"
      ? "Regulation A is only available to issuers organized in, and with their principal place of business in, the United States or Canada."
      : answers.regAExcludedIssuer
        ? "Investment companies, BDCs, blank-check companies/SPACs and issuers of fractional oil, gas or mineral rights cannot use Regulation A."
        : null;
  const setup: SetupStep[] = [
    {
      text: "Allowlist purchasers with compliance `add_to_allowlist` after KYC; Regulation A securities are not restricted, so no Rule 144 lockup is required.",
      guide: GUIDES.compliance,
    },
  ];

  const tier = (id: string, name: string, cap: number, sellingCap: number, extra: string[]): FrameworkResult => {
    const overCap = raise > cap;
    return {
      id,
      name,
      jurisdiction: "US",
      status: issuerBlocked || overCap ? "ineligible" : "eligible",
      recommended: false,
      reasons: issuerBlocked
        ? [issuerBlocked]
        : overCap
          ? [`The planned ${usd.format(raise)} raise exceeds the ${usd.format(cap)} limit for any 12-month period.`]
          : [`Raises up to ${usd.format(cap)} in 12 months (including up to ${usd.format(sellingCap)} by selling securityholders) and allows non-accredited investors and general solicitation.`],
      requirements: [
        "File an offering statement on Form 1-A and have it qualified by the SEC before selling.",
        "The issuer must also be current in its Regulation A and Exchange Act reports for the past two years, not subject to a Section 12(j) order entered in the past five years, and not disqualified as a bad actor under Rule 262.",
        ...extra,
      ],
      setup,
      source: SOURCES.regA,
    };
  };

  return [
    tier("reg-a-tier-2", "Regulation A — Tier 2", LIMITS.regATier2Usd, LIMITS.regATier2SellingHoldersUsd, [
      "Audited financial statements; ongoing Form 1-K, 1-SA and 1-U reports.",
      "Non-accredited natural persons may invest no more than 10% of the greater of annual income or net worth (entities: revenue or net assets), unless the securities are listed on a national securities exchange.",
      "State registration is preempted (state notice filings and fees may still apply).",
    ]),
    tier("reg-a-tier-1", "Regulation A — Tier 1", LIMITS.regATier1Usd, LIMITS.regATier1SellingHoldersUsd, [
      "Register or qualify the offering in every state where it is offered.",
    ]),
  ];
}

function evaluateRegS(answers: ExemptionAnswers): FrameworkResult {
  const usIssuer = answers.issuerLocation === "us";
  const debt = answers.instrument === "debt";
  const reasons = ["Offers and sales must be made in offshore transactions with no directed selling efforts in the United States."];
  const requirements: string[] = [];
  let period: { label: string; days: number } | null = null;

  if (!usIssuer) {
    reasons.push(
      "As a foreign issuer, Category 1 applies if there is no substantial U.S. market interest in the class of securities; otherwise Category 2 (40-day distribution compliance period) or Category 3 applies."
    );
  } else if (debt) {
    reasons.push("Debt securities of a U.S. issuer fall in Category 2 (reporting issuer) or Category 3 (non-reporting issuer), both with a 40-day distribution compliance period.");
    period = { label: "the 40-day distribution compliance period", days: 40 };
  } else {
    reasons.push(
      `Equity securities of a U.S. issuer fall in Category 3 with a ${answers.reportingIssuer ? "six-month (reporting issuer)" : "one-year"} distribution compliance period.`
    );
    requirements.push("Each purchaser must certify it is not a U.S. person (and is not buying for one) and agree to resell only under Regulation S, registration or an exemption.");
    period = answers.reportingIssuer
      ? { label: "the six-month distribution compliance period", days: 184 }
      : { label: "the one-year distribution compliance period", days: 366 };
  }
  requirements.push("Implement Regulation S offering restrictions for Category 2 and 3 offerings; Regulation S can run alongside a concurrent Regulation D offering to U.S. investors.");

  return {
    id: "reg-s",
    name: "Regulation S — offshore offering",
    jurisdiction: "US",
    status: "eligible",
    recommended: false,
    reasons,
    requirements,
    setup: [
      {
        text: answers.markets.includes("us")
          ? "Record each holder's jurisdiction on `add_to_allowlist` and keep U.S. buyers in the Regulation D tranche; do not resell offshore tokens to U.S. persons during the compliance period."
          : "Use compliance `block_jurisdiction(admin, \"US\")` so no U.S.-jurisdiction holder can receive the tokens during the compliance period.",
        guide: GUIDES.complianceContract,
      },
      ...(period ? [{ text: lockupStep(period.label, period.days), guide: GUIDES.ledgers }] : []),
    ],
    source: SOURCES.regS,
  };
}

function evaluateEuSecurities(answers: ExemptionAnswers): FrameworkResult {
  const raise = answers.euRaiseEur ?? 0;
  const exemptions: string[] = [];
  if (answers.euInvestors === "qualified-only") exemptions.push("The offer is addressed solely to qualified investors (Article 1(4)(a)).");
  if (answers.euFewerThan150PerState) {
    exemptions.push(`The offer reaches fewer than ${LIMITS.euPersonsPerMemberState} natural or legal persons per Member State, other than qualified investors (Article 1(4)(b)).`);
  }
  if (answers.euMinimumTicket100k) {
    exemptions.push(`Every investor commits at least ${eur.format(LIMITS.prospectusMinimumTicketEur)} (Article 1(4)(c)–(d)).`);
  }
  const notPassported = "provided the offer is not passported to other Member States (no Article 25 notification)";
  if (raise < LIMITS.prospectusOutOfScopeEur) {
    exemptions.push(`The ${eur.format(raise)} offer is below ${eur.format(LIMITS.prospectusOutOfScopeEur)} over 12 months, so the Prospectus Regulation does not apply to it (Article 1(3)).`);
  } else if (raise < LIMITS.prospectusSmallOfferNationalEur) {
    exemptions.push(
      `The ${eur.format(raise)} offer is below ${eur.format(LIMITS.prospectusSmallOfferNationalEur)} over 12 months, within every Member State's small-offer exemption (Article 3(2)), ${notPassported}.`
    );
  } else if (raise < LIMITS.prospectusSmallOfferEur) {
    exemptions.push(
      `The ${eur.format(raise)} offer is below the ${eur.format(LIMITS.prospectusSmallOfferEur)} 12-month small-offer exemption (Article 3(2)), ${notPassported}, unless the Member State applies the ${eur.format(LIMITS.prospectusSmallOfferNationalEur)} threshold instead.`
    );
  }
  const exempt = exemptions.length > 0;

  return {
    id: "eu-prospectus",
    name: exempt ? "EU Prospectus Regulation — exempt offer" : "EU Prospectus Regulation — approved prospectus",
    jurisdiction: "EU",
    status: exempt ? "eligible" : "required",
    recommended: true,
    reasons: [
      "A token that is a transferable security is a MiFID II financial instrument, which MiCA excludes (Article 2(4)(a)); the Prospectus Regulation governs its public offer.",
      ...(exempt ? exemptions : ["No exemption applies, so a prospectus approved by the home Member State's competent authority is required before the offer."]),
    ],
    requirements: [
      ...(!exempt
        ? ["An approved prospectus can be passported to other Member States."]
        : raise < LIMITS.prospectusOutOfScopeEur
          ? ["Member States may impose other national disclosure requirements on offers below €1,000,000 (Article 1(3))."]
          : []),
      "Placement and resale through intermediaries must comply with MiFID II.",
    ],
    setup: [
      {
        text: "Record each investor's Member State as the jurisdiction on `add_to_allowlist`, and block Member States the offer does not cover with `block_jurisdiction`.",
        guide: GUIDES.complianceContract,
      },
    ],
    source: SOURCES.prospectus,
  };
}

function evaluateMica(answers: ExemptionAnswers): FrameworkResult {
  if (answers.stableValue === "single-currency") {
    return {
      id: "mica-emt",
      name: "MiCA Title IV — e-money token",
      jurisdiction: "EU",
      status: "required",
      recommended: true,
      reasons: ["A token that keeps a stable value by referencing one official currency is an e-money token."],
      requirements: [
        "Only a credit institution or an electronic money institution may offer it, after notifying and publishing a crypto-asset white paper (Article 48(1)).",
        "Narrow exemptions exist for issuers exempted under Article 9(1) of Directive 2009/110/EC and for tokens excluded by its Article 1(4)–(5); those issuers must still draw up and notify a white paper (Article 48(4), (5) and (7)).",
      ],
      setup: [{ text: "Gate holders through the compliance allowlist once the issuer is authorised.", guide: GUIDES.compliance }],
      source: SOURCES.micaArt48,
    };
  }

  if (answers.stableValue === "other-assets") {
    const exemptions: string[] = [];
    if (answers.artOutstandingUnder5m) {
      exemptions.push(
        `The token's average outstanding value never exceeds ${eur.format(LIMITS.micaArtOutstandingEur)} over 12 months (Article 16(2)(a)); this exemption is unavailable if the issuer is part of a network of other exempt issuers.`
      );
    }
    if (answers.euInvestors === "qualified-only") {
      exemptions.push("The offer is addressed solely to qualified investors and the token can only be held by them (Article 16(2)(b)).");
    }
    const exempt = exemptions.length > 0;
    return {
      id: "mica-art",
      name: exempt ? "MiCA Title III — asset-referenced token (authorisation exemption)" : "MiCA Title III — asset-referenced token",
      jurisdiction: "EU",
      status: exempt ? "eligible" : "required",
      recommended: true,
      reasons: [
        "A token that keeps a stable value by referencing other assets, rights or a basket is an asset-referenced token.",
        ...(exempt ? exemptions : ["No Article 16(2) exemption applies, so the issuer needs authorisation before the offer."]),
      ],
      requirements: exempt
        ? ["Draw up a crypto-asset white paper under Article 19 and notify it, and on request any marketing communications, to the home Member State's competent authority (Article 16(2))."]
        : ["The issuer must be established in the EU and authorised under Article 21, or be a credit institution complying with Article 17 (Article 16(1))."],
      setup: [{ text: "Gate holders through the compliance allowlist; for the qualified-investor exemption, allowlist only verified qualified investors.", guide: GUIDES.compliance }],
      source: SOURCES.micaArt16,
    };
  }

  const raise = answers.euRaiseEur ?? 0;
  const exemptions: string[] = [];
  if (answers.euFewerThan150PerState) {
    exemptions.push(`The offer reaches fewer than ${LIMITS.euPersonsPerMemberState} natural or legal persons per Member State acting on their own account (Article 4(2)(a)).`);
  }
  if (raise <= LIMITS.micaSmallOfferEur) {
    exemptions.push(`Total consideration of ${eur.format(raise)} does not exceed ${eur.format(LIMITS.micaSmallOfferEur)} over 12 months (Article 4(2)(b)).`);
  }
  if (answers.euInvestors === "qualified-only") {
    exemptions.push("The offer is addressed solely to qualified investors and the token can only be held by them (Article 4(2)(c)).");
  }
  const blockedByTrading = answers.seeksAdmissionToTrading === true;
  const exempt = exemptions.length > 0 && !blockedByTrading;

  return {
    id: "mica-title-ii",
    name: exempt ? "MiCA Title II — white paper exemption" : "MiCA Title II — crypto-asset white paper",
    jurisdiction: "EU",
    status: exempt ? "eligible" : "required",
    recommended: true,
    reasons: [
      ...(exempt ? exemptions : []),
      ...(blockedByTrading && exemptions.length > 0
        ? ["The Article 4(2) exemptions are lost because the offeror intends to seek admission to trading (Article 4(4))."]
        : []),
      ...(!exempt ? ["The offeror must draw up, notify and publish a crypto-asset white paper before the offer."] : []),
    ],
    requirements: [
      "The offeror must be a legal person and comply with the Article 14 offeror obligations.",
      ...(exempt ? [] : ["Marketing communications must follow Article 7 and be published under Article 9."]),
    ],
    setup: [
      {
        text: answers.euInvestors === "qualified-only"
          ? "Allowlist only verified qualified investors so the token cannot be held by anyone else."
          : "Allowlist holders by Member State with `add_to_allowlist` so the per-state investor count can be audited.",
        guide: GUIDES.compliance,
      },
    ],
    source: SOURCES.micaArt4,
  };
}

/** Marks the first eligible framework in `preference` as recommended. */
function recommendFirst(results: FrameworkResult[], preference: string[]): void {
  const pick = preference.map((id) => results.find((result) => result.id === id)).find((result) => result?.status === "eligible");
  if (pick) pick.recommended = true;
}

/**
 * Evaluates the frameworks relevant to the answers. Each framework is a
 * constant-time rule, so evaluation is O(1) in the number of answers.
 */
export function evaluateExemptions(answers: ExemptionAnswers): Assessment {
  const results: FrameworkResult[] = [];
  const notes: string[] = [];
  const usMarket = answers.markets.includes("us");
  const offshore = answers.markets.some((market) => market !== "us");

  if (answers.instrument === "crypto-asset") {
    notes.push(
      "U.S. exemptions are not evaluated for tokens you classify as non-securities. Whether a token is an investment contract is a facts-and-circumstances Howey analysis; get counsel before relying on that classification."
    );
  } else if (usMarket || offshore) {
    if (usMarket) results.push(...evaluateRegD(answers), ...evaluateRegA(answers));
    if (offshore && (usMarket || answers.issuerLocation === "us")) results.push(evaluateRegS(answers));
    if (usMarket) {
      // Preference: fewest ongoing obligations first, then the broadest investor base.
      recommendFirst(
        results,
        answers.usInvestors === "accredited-only"
          ? answers.generalSolicitation
            ? ["reg-d-506c", "reg-a-tier-2", "reg-a-tier-1"]
            : ["reg-d-506b", "reg-d-506c"]
          : answers.generalSolicitation
            ? ["reg-a-tier-2", "reg-a-tier-1"]
            : ["reg-d-506b", "reg-a-tier-2", "reg-a-tier-1"]
      );
    }
    const regS = results.find((result) => result.id === "reg-s");
    if (regS) regS.recommended = true;
  }

  if (answers.markets.includes("eu")) {
    results.push(answers.instrument === "crypto-asset" ? evaluateMica(answers) : evaluateEuSecurities(answers));
  }
  if (answers.markets.includes("other")) {
    notes.push("Investors outside the U.S. and EU are subject to their own local securities laws, which this checker does not evaluate.");
  }
  return { results, notes };
}
