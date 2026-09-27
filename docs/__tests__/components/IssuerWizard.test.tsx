import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Keypair } from "@stellar/stellar-sdk";
import { IssuerWizard } from "../../components/IssuerWizard";

describe("IssuerWizard", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("renders the wizard and validates Stellar public keys", async () => {
    render(<IssuerWizard />);

    fireEvent.click(screen.getByRole("button", { name: /start onboarding/i }));

    fireEvent.click(screen.getByRole("button", { name: /next/i }));
    await waitFor(() => {
      expect(screen.getByText(/asset name is required/i)).toBeInTheDocument();
    });

    const validKey = Keypair.random().publicKey();

    fireEvent.change(screen.getByLabelText(/asset name/i), {
      target: { value: "Bluebird Capital" },
    });
    fireEvent.change(screen.getByLabelText(/ticker symbol/i), {
      target: { value: "BRC" },
    });
    fireEvent.change(screen.getByLabelText(/issuer public key/i), {
      target: { value: validKey },
    });
    fireEvent.change(screen.getByLabelText(/total supply/i), {
      target: { value: "1000000" },
    });

    fireEvent.click(screen.getByRole("button", { name: /next/i }));

    await waitFor(() => {
      expect(screen.getByText(/compliance configuration/i)).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: /next/i }));
    await waitFor(() => {
      expect(screen.getByText(/compliance officer key is required/i)).toBeInTheDocument();
    });
  });

  it("persists form state in localStorage and restores it after rerender", () => {
    const { unmount } = render(<IssuerWizard />);

    fireEvent.click(screen.getByRole("button", { name: /start onboarding/i }));

    fireEvent.change(screen.getByLabelText(/asset name/i), {
      target: { value: "Harbor Logistics" },
    });
    fireEvent.change(screen.getByLabelText(/ticker symbol/i), {
      target: { value: "HLG" },
    });
    fireEvent.change(screen.getByLabelText(/issuer public key/i), {
      target: { value: Keypair.random().publicKey() },
    });
    fireEvent.change(screen.getByLabelText(/total supply/i), {
      target: { value: "2500000" },
    });

    expect(localStorage.getItem("tessera_issuer_wizard_state")).toContain("Harbor Logistics");

    unmount();
    render(<IssuerWizard />);

    expect(screen.getByDisplayValue("Harbor Logistics")).toBeInTheDocument();
  });

  it("shows deployment script actions once the workflow is complete", async () => {
    render(<IssuerWizard />);

    fireEvent.click(screen.getByRole("button", { name: /start onboarding/i }));

    const validKey = Keypair.random().publicKey();
    const complianceKey = Keypair.random().publicKey();

    fireEvent.change(screen.getByLabelText(/asset name/i), { target: { value: "Northwind Finance" } });
    fireEvent.change(screen.getByLabelText(/ticker symbol/i), { target: { value: "NWF" } });
    fireEvent.change(screen.getByLabelText(/issuer public key/i), { target: { value: validKey } });
    fireEvent.change(screen.getByLabelText(/total supply/i), { target: { value: "5000000" } });
    fireEvent.click(screen.getByRole("button", { name: /next/i }));

    await waitFor(() => {
      expect(screen.getByText(/compliance configuration/i)).toBeInTheDocument();
    });

    fireEvent.change(screen.getByLabelText(/compliance officer key/i), { target: { value: complianceKey } });
    fireEvent.change(screen.getByLabelText(/jurisdiction/i), { target: { value: "US" } });
    fireEvent.change(screen.getByLabelText(/allowlist seed/i), { target: { value: validKey } });
    fireEvent.click(screen.getByRole("button", { name: /next/i }));

    await waitFor(() => {
      expect(screen.getByText(/deployment script/i)).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: /trigger wallet deployment/i }));
    expect(screen.getByText(/wallet deployment requested/i)).toBeInTheDocument();
  });
});
