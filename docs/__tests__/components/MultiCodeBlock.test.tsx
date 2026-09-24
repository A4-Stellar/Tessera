import { render, screen, fireEvent, act } from "@testing-library/react";
import { MultiCodeBlock, type CodeSnippet } from "../../components/MultiCodeBlock";

const mockSnippets: CodeSnippet[] = [
  { language: "curl", label: "cURL", code: "curl https://api.tessera.xyz/assets/1" },
  { language: "typescript", label: "TypeScript", code: "const asset = await client.getAsset(1);" },
  { language: "rust", label: "Rust SDK", code: "let asset = client.get_asset(1).await?;" },
  { language: "python", label: "Python", code: "asset = requests.get('https://api.tessera.xyz/assets/1').json()" },
];

describe("MultiCodeBlock Component", () => {
  beforeEach(() => {
    localStorage.clear();
    jest.clearAllMocks();
    Object.assign(navigator, {
      clipboard: {
        writeText: jest.fn().mockImplementation(() => Promise.resolve()),
      },
    });
  });

  it("renders all language tabs with labels", () => {
    render(<MultiCodeBlock snippets={mockSnippets} title="Get Asset" />);

    expect(screen.getByText("Get Asset")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /cURL/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /TypeScript/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Rust SDK/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Python/i })).toBeInTheDocument();
  });

  it("displays the first tab by default when no saved preference exists", () => {
    render(<MultiCodeBlock snippets={mockSnippets} />);

    const firstTab = screen.getByRole("tab", { name: /cURL/i });
    expect(firstTab).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("curl https://api.tessera.xyz/assets/1")).toBeInTheDocument();
  });

  it("switches code snippet when clicking a different tab", () => {
    render(<MultiCodeBlock snippets={mockSnippets} />);

    const rustTab = screen.getByRole("tab", { name: /Rust SDK/i });
    fireEvent.click(rustTab);

    expect(rustTab).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("let asset = client.get_asset(1).await?;")).toBeInTheDocument();
    expect(localStorage.getItem("tessera_preferred_code_lang")).toBe("rust");
  });

  it("supports keyboard navigation across tabs (ArrowRight, ArrowLeft, Home, End)", () => {
    render(<MultiCodeBlock snippets={mockSnippets} />);

    const curlTab = screen.getByRole("tab", { name: /cURL/i });
    curlTab.focus();

    // ArrowRight -> TypeScript
    fireEvent.keyDown(curlTab, { key: "ArrowRight" });
    const tsTab = screen.getByRole("tab", { name: /TypeScript/i });
    expect(tsTab).toHaveAttribute("aria-selected", "true");

    // ArrowRight -> Rust
    fireEvent.keyDown(tsTab, { key: "ArrowRight" });
    const rustTab = screen.getByRole("tab", { name: /Rust SDK/i });
    expect(rustTab).toHaveAttribute("aria-selected", "true");

    // End -> Python
    fireEvent.keyDown(rustTab, { key: "End" });
    const pyTab = screen.getByRole("tab", { name: /Python/i });
    expect(pyTab).toHaveAttribute("aria-selected", "true");

    // Home -> cURL
    fireEvent.keyDown(pyTab, { key: "Home" });
    expect(screen.getByRole("tab", { name: /cURL/i })).toHaveAttribute("aria-selected", "true");
  });

  it("synchronizes language selection across multiple code blocks via CustomEvents", () => {
    const { container } = render(
      <div>
        <MultiCodeBlock title="Block A" snippets={mockSnippets} />
        <MultiCodeBlock title="Block B" snippets={mockSnippets} />
      </div>
    );

    const blockATabs = screen.getAllByRole("tab", { name: /Rust SDK/i });
    // Click Rust on the first block
    fireEvent.click(blockATabs[0]);

    // Both blocks should now show Rust as selected
    const allRustTabs = screen.getAllByRole("tab", { name: /Rust SDK/i });
    allRustTabs.forEach((tab) => {
      expect(tab).toHaveAttribute("aria-selected", "true");
    });
  });

  it("copies active snippet code to clipboard and shows feedback tooltip", async () => {
    render(<MultiCodeBlock snippets={mockSnippets} />);

    const copyBtn = screen.getByRole("button", { name: /Copy code snippet to clipboard/i });
    await act(async () => {
      fireEvent.click(copyBtn);
    });

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(
      "curl https://api.tessera.xyz/assets/1"
    );
    expect(screen.getByText("Copied!")).toBeInTheDocument();
  });

  it("respects defaultLanguage prop if specified", () => {
    render(<MultiCodeBlock snippets={mockSnippets} defaultLanguage="python" />);

    const pyTab = screen.getByRole("tab", { name: /Python/i });
    expect(pyTab).toHaveAttribute("aria-selected", "true");
  });
});
