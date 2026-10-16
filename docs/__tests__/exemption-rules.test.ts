import { EMPTY_ANSWERS, evaluateExemptions, type ExemptionAnswers } from "@/lib/exemption-rules";

const answers = (overrides: Partial<ExemptionAnswers>): ExemptionAnswers => ({ ...EMPTY_ANSWERS, ...overrides });
const byId = (overrides: Partial<ExemptionAnswers>) =>
  Object.fromEntries(evaluateExemptions(answers(overrides)).results.map((result) => [result.id, result]));

const usIssuer: Partial<ExemptionAnswers> = {
  issuerLocation: "us",
  instrument: "equity",
  markets: ["us"],
  reportingIssuer: false,
  regAExcludedIssuer: false,
};

describe("evaluateExemptions", () => {
  it("recommends Rule 506(c) for a publicly marketed raise from accredited investors", () => {
    const results = byId({ ...usIssuer, usInvestors: "accredited-only", generalSolicitation: true, usRaiseUsd: 10_000_000 });

    expect(results["reg-d-506c"]).toMatchObject({ status: "eligible", recommended: true });
    expect(results["reg-d-506b"]).toMatchObject({ status: "ineligible", recommended: false });
    expect(results["reg-a-tier-1"].status).toBe("eligible");
    expect(results["reg-d-506c"].setup[1].text).toContain("6,324,480 ledgers");
  });

  it("recommends Regulation A Tier 2 for a publicly marketed raise that includes non-accredited investors", () => {
    const results = byId({ ...usIssuer, usInvestors: "includes-non-accredited", generalSolicitation: true, usRaiseUsd: 50_000_000 });

    expect(results["reg-a-tier-2"]).toMatchObject({ status: "eligible", recommended: true });
    expect(results["reg-a-tier-1"].reasons[0]).toContain("exceeds the $20,000,000 limit");
    expect(results["reg-d-506c"].status).toBe("ineligible");
    expect(results["reg-d-506b"].status).toBe("ineligible");
  });

  it("uses Rule 506(b) for a private raise and shortens the holding period for reporting issuers", () => {
    const results = byId({
      ...usIssuer,
      reportingIssuer: true,
      usInvestors: "includes-non-accredited",
      generalSolicitation: false,
      usRaiseUsd: 120_000_000,
    });

    expect(results["reg-d-506b"]).toMatchObject({ status: "eligible", recommended: true });
    expect(results["reg-d-506b"].reasons).toContain(
      "Non-accredited purchasers are allowed: no more than 35 in any 90-calendar-day period, and each must be financially sophisticated."
    );
    expect(results["reg-d-506b"].setup[1].text).toContain("six-month");
    expect(results["reg-a-tier-2"].status).toBe("ineligible");
  });

  it("adds Regulation S for offshore investors and rules out Regulation A for non-U.S./Canadian issuers", () => {
    const results = byId({
      issuerLocation: "eu",
      instrument: "equity",
      markets: ["us", "other"],
      usInvestors: "accredited-only",
      generalSolicitation: false,
      usRaiseUsd: 5_000_000,
      reportingIssuer: false,
    });

    expect(results["reg-s"]).toMatchObject({ status: "eligible", recommended: true });
    expect(results["reg-s"].reasons[1]).toContain("Category 1");
    expect(results["reg-a-tier-2"].reasons[0]).toContain("United States or Canada");
  });

  it("applies a one-year Category 3 compliance period to offshore equity of a U.S. issuer", () => {
    const results = byId({ issuerLocation: "us", instrument: "equity", markets: ["other"], reportingIssuer: false });

    expect(Object.keys(results)).toEqual(["reg-s"]);
    expect(results["reg-s"].reasons[1]).toContain("Category 3 with a one-year");
    expect(results["reg-s"].setup[0].text).toContain('block_jurisdiction(admin, "US")');
  });

  it("applies the Prospectus Regulation, not MiCA, to tokenized securities offered in the EU", () => {
    const small = byId({ issuerLocation: "eu", instrument: "debt", markets: ["eu"], euInvestors: "retail", euRaiseEur: 8_000_000, euFewerThan150PerState: false, euMinimumTicket100k: false });
    const large = byId({ issuerLocation: "eu", instrument: "debt", markets: ["eu"], euInvestors: "retail", euRaiseEur: 20_000_000, euFewerThan150PerState: false, euMinimumTicket100k: false });

    expect(small["eu-prospectus"]).toMatchObject({ status: "eligible", name: "EU Prospectus Regulation — exempt offer" });
    expect(small["eu-prospectus"].reasons[1]).toContain("Article 3(2)");
    expect(small["eu-prospectus"].reasons[1]).toContain("not passported");
    expect(small["eu-prospectus"].reasons[1]).toContain("unless the Member State applies the €5,000,000 threshold instead");
    expect(large["eu-prospectus"]).toMatchObject({ status: "required", name: "EU Prospectus Regulation — approved prospectus" });
  });

  it("treats EU offers below €1,000,000 as outside the Prospectus Regulation", () => {
    const results = byId({ issuerLocation: "eu", instrument: "equity", markets: ["eu"], euInvestors: "retail", euRaiseEur: 800_000, euFewerThan150PerState: false, euMinimumTicket100k: false });

    expect(results["eu-prospectus"].reasons[1]).toContain("does not apply to it (Article 1(3))");
    expect(results["eu-prospectus"].requirements[0]).toContain("national disclosure requirements");
  });

  it("applies the Article 16(2) authorisation exemptions to asset-referenced tokens", () => {
    const base: Partial<ExemptionAnswers> = { issuerLocation: "eu", instrument: "crypto-asset", markets: ["eu"], stableValue: "other-assets" };
    const small = byId({ ...base, euInvestors: "retail", artOutstandingUnder5m: true });
    const large = byId({ ...base, euInvestors: "retail", artOutstandingUnder5m: false });
    const qualified = byId({ ...base, euInvestors: "qualified-only" });

    expect(small["mica-art"]).toMatchObject({ status: "eligible" });
    expect(small["mica-art"].requirements[0]).toContain("white paper under Article 19");
    expect(qualified["mica-art"].reasons[1]).toContain("Article 16(2)(b)");
    expect(large["mica-art"]).toMatchObject({ status: "required" });
    expect(large["mica-art"].requirements[0]).toContain("authorised under Article 21");
  });

  it("drops MiCA white paper exemptions when admission to trading is sought", () => {
    const results = byId({
      issuerLocation: "eu",
      instrument: "crypto-asset",
      markets: ["eu"],
      stableValue: "none",
      euInvestors: "retail",
      euRaiseEur: 900_000,
      euFewerThan150PerState: false,
      seeksAdmissionToTrading: true,
    });

    expect(results["mica-title-ii"].status).toBe("required");
    expect(results["mica-title-ii"].reasons).toContain(
      "The Article 4(2) exemptions are lost because the offeror intends to seek admission to trading (Article 4(4))."
    );
  });

  it("requires authorisation for e-money tokens and skips U.S. exemptions for non-securities", () => {
    const assessment = evaluateExemptions(answers({ issuerLocation: "eu", instrument: "crypto-asset", markets: ["eu", "us"], stableValue: "single-currency" }));

    expect(assessment.results.map((result) => result.id)).toEqual(["mica-emt"]);
    expect(assessment.results[0].source.label).toBe("MiCA Article 48");
    expect(assessment.results[0].requirements[1]).toContain("Article 9(1) of Directive 2009/110/EC");
    expect(assessment.notes[0]).toContain("Howey");
  });
});
