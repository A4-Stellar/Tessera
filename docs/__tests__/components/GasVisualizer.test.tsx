import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { axe } from "jest-axe";
import { xdr } from "@stellar/stellar-sdk";
import { GasVisualizer } from "@/components/GasVisualizer";
import type { ReleaseMeasurement, ResourceUsage } from "@/lib/soroban-resources";

const usage = (overrides: Partial<ResourceUsage>): ResourceUsage => ({
  instructions: 0,
  memBytes: 0,
  diskReadEntries: 0,
  diskReadBytes: 0,
  writeEntries: 0,
  writeBytes: 0,
  eventsBytes: 0,
  ...overrides,
});

const releases: ReleaseMeasurement[] = [
  {
    id: "aaaa111",
    label: "Baseline",
    date: "2026-01-01",
    methods: [{ contract: "vault", method: "rebalance", usage: usage({ instructions: 200_000_000, writeBytes: 60_000 }) }],
  },
  {
    id: "bbbb222",
    label: "Batched writes",
    date: "2026-02-01",
    methods: [{ contract: "vault", method: "rebalance", usage: usage({ instructions: 380_000_000, writeBytes: 66_048 }) }],
  },
];

describe("GasVisualizer", () => {
  beforeAll(() => {
    // jsdom has no ResizeObserver; recharts' ResponsiveContainer only needs it to exist.
    global.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  });

  afterEach(() => {
    jest.restoreAllMocks();
  });

  it("meters each resource against the limits and flags bottlenecks", async () => {
    const { container } = render(<GasVisualizer releases={releases} />);
    const card = screen.getByRole("listitem", { name: "vault.rebalance" });

    const cpu = within(card).getByRole("meter", { name: "CPU instructions used" });
    expect(cpu).toHaveAttribute("aria-valuetext", "95.0% of the per-transaction limit");
    expect(within(card).getByText("Critical")).toBeInTheDocument();
    expect(within(card).getByText("CPU instructions is close to the limit.")).toBeInTheDocument();
    expect(within(card).getByText("1× per transaction")).toBeInTheDocument();
    expect(await axe(container)).toHaveNoViolations();
  });

  it("switches release and lists every release in the comparison table", () => {
    render(<GasVisualizer releases={releases} />);

    fireEvent.change(screen.getByLabelText("Contract release"), { target: { value: "aaaa111" } });
    const card = screen.getByRole("listitem", { name: "vault.rebalance" });
    expect(within(card).getByRole("meter", { name: "CPU instructions used" })).toHaveAttribute(
      "aria-valuetext",
      "50.0% of the per-transaction limit"
    );
    expect(within(card).queryByText("Critical")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Resource"), { target: { value: "writeBytes" } });
    const row = screen.getByRole("rowheader", { name: "vault.rebalance" }).closest("tr") as HTMLElement;
    expect(within(row).getByText("60,000")).toBeInTheDocument();
    expect(within(row).getByText("66,048")).toBeInTheDocument();
  });

  it("reads live limits from Soroban RPC when an endpoint is given", async () => {
    const compute = xdr.LedgerEntryData.configSetting(
      xdr.ConfigSettingEntry.configSettingContractComputeV0(
        new xdr.ConfigSettingContractComputeV0({
          ledgerMaxInstructions: BigInt(800_000_000),
          txMaxInstructions: BigInt(760_000_000),
          feeRatePerInstructionsIncrement: BigInt(25),
          txMemoryLimit: 41_943_040,
        })
      )
    );
    const entries = [
      xdr.LedgerEntryData.configSetting(xdr.ConfigSettingEntry.configSettingContractMaxSizeBytes(131_072)),
      compute,
      xdr.LedgerEntryData.configSetting(
        xdr.ConfigSettingEntry.configSettingContractLedgerCostV0(
          new xdr.ConfigSettingContractLedgerCostV0({
            ledgerMaxDiskReadEntries: 1000,
            ledgerMaxDiskReadBytes: 400_000,
            ledgerMaxWriteLedgerEntries: 1000,
            ledgerMaxWriteBytes: 286_720,
            txMaxDiskReadEntries: 200,
            txMaxDiskReadBytes: 200_000,
            txMaxWriteLedgerEntries: 200,
            txMaxWriteBytes: 132_096,
            feeDiskReadLedgerEntry: BigInt(1563),
            feeWriteLedgerEntry: BigInt(2500),
            feeDiskRead1Kb: BigInt(447),
            sorobanStateTargetSizeBytes: BigInt(3_000_000_000),
            rentFee1KbSorobanStateSizeLow: BigInt(-17_000),
            rentFee1KbSorobanStateSizeHigh: BigInt(10_000),
            sorobanStateRentFeeGrowthFactor: 5000,
          })
        )
      ),
      xdr.LedgerEntryData.configSetting(
        xdr.ConfigSettingEntry.configSettingContractEventsV0(
          new xdr.ConfigSettingContractEventsV0({ txMaxContractEventsSizeBytes: 16_384, feeContractEvents1Kb: BigInt(5000) })
        )
      ),
    ].map((entry) => ({ xdr: Buffer.from(entry.toXdr()).toString("base64") }));

    global.fetch = jest.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
      const { method } = JSON.parse(String(init?.body));
      const result = method === "getLatestLedger" ? { protocolVersion: 30, sequence: 99 } : { entries, latestLedger: 99 };
      return { ok: true, json: async () => ({ result }) } as Response;
    }) as typeof fetch;

    render(<GasVisualizer releases={releases} rpcUrl="https://rpc.example.org" network="Testnet" />);

    expect(await screen.findByText(/read live from rpc\.example\.org at ledger 99/i)).toBeInTheDocument();
    const card = screen.getByRole("listitem", { name: "vault.rebalance" });
    await waitFor(() =>
      expect(within(card).getByRole("meter", { name: "CPU instructions used" })).toHaveAttribute(
        "aria-valuetext",
        "50.0% of the per-transaction limit"
      )
    );
  });
});
