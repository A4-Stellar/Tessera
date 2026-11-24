import { readFileSync } from "fs";
import { join } from "path";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { axe } from "jest-axe";
import { CapTableCompare } from "../../components/CapTableCompare";

const FORM4_CSV = readFileSync(join(__dirname, "..", "fixtures", "edgar-form4-holders.csv"), "utf8");
const DOE = "GAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAI";
const ROE = "GAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQ";
const NEWCOMER = "GAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBA";

function ok(body: unknown) {
  return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(body) });
}

function csvFile(text: string, name = "form4.csv") {
  const file = new File([text], name, { type: "text/csv" });
  // jsdom's File lacks text() in some versions.
  Object.defineProperty(file, "text", { value: () => Promise.resolve(text) });
  return file;
}

async function upload(text: string) {
  const input = screen.getByLabelText(/form 4 holdings csv/i);
  await act(async () => {
    fireEvent.change(input, { target: { files: [csvFile(text)] } });
  });
}

describe("CapTableCompare mode=edgar", () => {
  const fetchImpl = jest.fn((url: string) => {
    if (url.endsWith("/v1/assets/1")) return ok({ total_supply: "1000000", decimals: 0 });
    if (url.includes("/v1/assets/1/holders")) {
      return ok([
        { address: DOE, balance: "115000", share_percent: 11.5 },
        { address: ROE, balance: "42500", share_percent: 4.25 },
        { address: NEWCOMER, balance: "12000", share_percent: 1.2 },
      ]);
    }
    return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({}) });
  });

  beforeEach(() => fetchImpl.mockClear());

  it("loads on-chain holders and compares them with the uploaded filing", async () => {
    render(<CapTableCompare mode="edgar" assetId={1} apiBaseUrl="https://api.test" fetchImpl={fetchImpl} />);
    await waitFor(() => expect(screen.getByText(/upload a csv/i)).toBeInTheDocument());
    await upload(FORM4_CSV);

    expect(await screen.findByText(/1 new, 1 departed, 1 changed, 1 unchanged/i)).toBeInTheDocument();
    expect(screen.getByText(/2 discrepancies over 1 % of total supply/i)).toBeInTheDocument();

    const rows = screen.getAllByRole("row").slice(1);
    const byKind = (kind: string) => rows.find((r) => r.getAttribute("data-kind") === kind)!;
    // ROE: 42 500 vs 40 000 = +6.25 %: yellow, labelled in text, not only colour.
    expect(byKind("changed")).toHaveAttribute("data-highlight", "yellow");
    expect(within(byKind("changed")).getByText("Over 5 % change")).toBeInTheDocument();
    expect(byKind("new")).toHaveAttribute("data-highlight", "red");
    expect(within(byKind("departed")).getByText("Over 1 % of supply")).toBeInTheDocument();
    expect(byKind("unchanged")).toHaveAttribute("data-highlight", "none");
  });

  it("explains a malformed CSV", async () => {
    render(<CapTableCompare mode="edgar" assetId={1} holders={[]} totalSupply="1" decimals={0} />);
    await upload("name,amount\nx,1");
    expect(await screen.findByRole("alert")).toHaveTextContent(/address.*shares/i);
  });

  it("signs the report through the API and exports a PDF", async () => {
    const created: Blob[] = [];
    const urlApi = URL as unknown as { createObjectURL: (b: Blob) => string; revokeObjectURL: (u: string) => void };
    urlApi.createObjectURL = jest.fn((b: Blob) => {
      created.push(b);
      return "blob:report";
    });
    urlApi.revokeObjectURL = jest.fn();
    const click = jest.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    const sign = jest.fn((_url: string, init?: RequestInit) => {
      expect((init?.headers as Record<string, string>).authorization).toBe("Bearer secret-token");
      expect(JSON.parse(String(init?.body)).report.rows).toHaveLength(4);
      return ok({ algorithm: "ed25519-sha256", digest: "ab".repeat(32), signature: "cd".repeat(64), public_key: "ef".repeat(32), signed_at: "2026-11-24T00:00:00Z" });
    });
    const impl = jest.fn((url: string, init?: RequestInit) => (url.endsWith("/reports/sign") ? sign(url, init) : fetchImpl(url)));

    render(<CapTableCompare mode="edgar" assetId={1} apiBaseUrl="https://api.test" fetchImpl={impl} />);
    await waitFor(() => expect(screen.getByText(/upload a csv/i)).toBeInTheDocument());
    await upload(FORM4_CSV);
    const button = await screen.findByRole("button", { name: /export signed pdf/i });
    expect(button).toBeDisabled();
    fireEvent.change(screen.getByLabelText(/admin token/i), { target: { value: "secret-token" } });
    await act(async () => {
      fireEvent.click(button);
    });

    expect(await screen.findByRole("status")).toHaveTextContent(/signed \(sha-256 abababab/i);
    expect(sign).toHaveBeenCalledTimes(1);
    expect(click).toHaveBeenCalled();
    expect(created[0].type).toBe("application/pdf");
    click.mockRestore();
  });

  it("has no accessibility violations", async () => {
    const { container } = render(
      <CapTableCompare mode="edgar" assetId={1} holders={[{ address: DOE, balance: "1" }]} totalSupply="10" decimals={0} />
    );
    await upload(FORM4_CSV);
    await screen.findByRole("table");
    expect(await axe(container)).toHaveNoViolations();
  });
});
