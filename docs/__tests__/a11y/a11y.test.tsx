import { render } from "@testing-library/react";
import { axe } from "jest-axe";
import { DocHeader } from "../../components/DocHeader";
import { Sidebar } from "../../components/Sidebar";
import { Search } from "../../components/Search";
import { CodeBlock } from "../../components/CodeBlock";
import { MultiCodeBlock } from "../../components/MultiCodeBlock";
import { EventExplorer } from "../../components/EventExplorer";
import { CalloutBox } from "../../components/CalloutBox";
import { ErrorCodeTable } from "../../components/ErrorCodeTable";
import { VersionBanner } from "../../components/VersionBanner";
import { PrevNext } from "../../components/PrevNext";
import { ApiEndpoint } from "../../components/ApiEndpoint";
import { ApiPlayground } from "../../components/ApiPlayground";

// Mock next/navigation
jest.mock("next/navigation", () => ({
  useRouter: () => ({ push: jest.fn() }),
  usePathname: () => "/docs/getting-started",
}));

describe("Automated Accessibility (a11y) WCAG 2.1 AA Audits", () => {
  it("DocHeader passes accessibility audit", async () => {
    const { container } = render(<DocHeader />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("Sidebar passes accessibility audit", async () => {
    const { container } = render(<Sidebar />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("Search passes accessibility audit", async () => {
    const { container } = render(<Search />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("CodeBlock passes accessibility audit", async () => {
    const { container } = render(
      <CodeBlock title="example.ts" code="const x = 1;">
        const x = 1;
      </CodeBlock>
    );
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("MultiCodeBlock passes accessibility audit", async () => {
    const { container } = render(
      <MultiCodeBlock
        title="Example Snippets"
        snippets={[
          { language: "curl", label: "cURL", code: "curl https://api.tessera.xyz" },
          { language: "typescript", label: "TypeScript", code: "const api = true;" },
          { language: "rust", label: "Rust", code: "let api = true;" },
        ]}
      />
    );
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("EventExplorer passes accessibility audit", async () => {
    const { container } = render(<EventExplorer />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("CalloutBox passes accessibility audit", async () => {
    const { container } = render(
      <CalloutBox variant="warning" title="Important Notice">
        Please ensure all Soroban contract parameters are verified.
      </CalloutBox>
    );
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("ErrorCodeTable passes accessibility audit", async () => {
    const { container } = render(
      <ErrorCodeTable
        contract="Registry"
        codes={[
          { code: 1, name: "AlreadyInitialized", description: "Contract has already been initialized." },
          { code: 2, name: "AssetNotFound", description: "Requested asset does not exist." },
        ]}
      />
    );
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("VersionBanner passes accessibility audit", async () => {
    const { container } = render(
      <VersionBanner docsVersion="v1.0.0" contractVersion="v1.2.0" />
    );
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("PrevNext navigation passes accessibility audit", async () => {
    const { container } = render(<PrevNext />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("ApiEndpoint passes accessibility audit", async () => {
    const { container } = render(
      <ApiEndpoint method="GET" path="/assets/:id" description="Fetch asset metadata." />
    );
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });

  it("ApiPlayground passes accessibility audit", async () => {
    const { container } = render(<ApiPlayground defaultPath="/stats" />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });
});
