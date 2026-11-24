import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { IssuerWizard } from "../../components/IssuerWizard";

describe("IssuerWizard", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("validates a public key before continuing to the next step", async () => {
    render(<IssuerWizard />);

    fireEvent.change(screen.getByLabelText(/asset name/i), { target: { value: "Atlas Residences" } });
    fireEvent.change(screen.getByLabelText(/issuer address/i), { target: { value: "bad-key" } });
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));

    await waitFor(() => {
      expect(screen.getByText(/issuer address must be a valid stellar public key/i)).toBeInTheDocument();
    });
  });

  it("persists form progress in localStorage across refreshes", async () => {
    render(<IssuerWizard />);

    fireEvent.change(screen.getByLabelText(/project name/i), { target: { value: "Atlas Fund" } });
    fireEvent.change(screen.getByLabelText(/asset name/i), { target: { value: "Atlas Residences" } });
    fireEvent.change(screen.getByLabelText(/issuer address/i), { target: { value: "GCVJ2NK4ZWIMQ5M3W7ZVQ7R7Q7YQ2J2V6A4YQY3O2ZY7J3QY2QJX7A2" } });

    await waitFor(() => {
      expect(localStorage.getItem("tessera-issuer-wizard")).toContain("Atlas Fund");
    });
  });
});

describe("IssuerWizard board approvals (issue #478)", () => {
  const key = (c: string) => `G${c.repeat(55)}`;
  const board = ["A", "B", "C", "D", "E"].map(key);

  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem(
      "tessera-issuer-wizard",
      JSON.stringify({
        projectName: "Atlas Fund",
        assetName: "Atlas Residences",
        issuerAddress: key("Q"),
        allowlist: key("Q"),
      })
    );
  });

  async function goToBoardStep() {
    render(<IssuerWizard />);
    await waitFor(() => expect(screen.getByLabelText(/project name/i)).toHaveValue("Atlas Fund"));
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));
    await screen.findByText(/board approval for large transfers/i);
  }

  it("is optional and can be skipped", async () => {
    await goToBoardStep();
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));
    expect(await screen.findByText(/compliance gate summary/i)).toBeInTheDocument();
  });

  it("validates an m-of-n policy", async () => {
    await goToBoardStep();
    fireEvent.click(screen.getByLabelText(/require board approval/i));
    fireEvent.change(screen.getByLabelText(/board signers/i), { target: { value: board.slice(0, 2).join("\n") } });
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));
    expect(await screen.findByText(/between 1 and 2/i)).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText(/board signers/i), { target: { value: `${board[0]}\n${board[0]}\n${board[1]}` } });
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));
    expect(await screen.findByText(/only be listed once/i)).toBeInTheDocument();
  });

  it("adds set_multisig_policy to the deployment script", async () => {
    await goToBoardStep();
    fireEvent.click(screen.getByLabelText(/require board approval/i));
    fireEvent.change(screen.getByLabelText(/board signers/i), { target: { value: board.join("\n") } });
    expect(screen.getByText("3 of 5 board signatures required")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));
    await screen.findByText(/compliance gate summary/i);
    fireEvent.click(screen.getByRole("button", { name: /continue/i }));

    const script = await screen.findByText(/set_multisig_policy/);
    expect(script.textContent).toContain("--required_signatures 3");
    expect(script.textContent).toContain(`--board '${JSON.stringify(board)}'`);
    expect(script.textContent).toContain("--threshold_amount 100000");
  });
});
