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
