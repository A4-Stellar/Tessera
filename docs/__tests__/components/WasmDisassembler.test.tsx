import { fireEvent, render, screen, within } from "@testing-library/react";
import { axe } from "jest-axe";
import { WasmDisassembler } from "@/components/WasmDisassembler";
import { buildSorobanWasm } from "../fixtures/sorobanWasm";

function upload(bytes: Uint8Array, fileName = "hello.wasm") {
  const input = screen.getByLabelText(/compiled contract/i);
  fireEvent.change(input, { target: { files: [new File([bytes.slice()], fileName, { type: "application/wasm" })] } });
}

describe("WasmDisassembler", () => {
  it("shows the decoded contract interface for an uploaded module", async () => {
    const { container } = render(<WasmDisassembler />);
    upload(buildSorobanWasm());

    expect(await screen.findByText("fn hello(to: Address) -> Vec<Symbol>")).toBeInTheDocument();
    const imports = screen.getByRole("region", { name: /imported host functions/i });
    expect(within(imports).getByText("vec::vec_new_from_linear_memory")).toBeInTheDocument();
    expect(screen.getByText("protocol 25")).toBeInTheDocument();
    expect(screen.getByText("1.98.0")).toBeInTheDocument();
    expect(await axe(container)).toHaveNoViolations();
  });

  it("searches and highlights instructions in the WAT view", async () => {
    render(<WasmDisassembler />);
    upload(buildSorobanWasm());

    fireEvent.click(await screen.findByRole("button", { name: "WAT" }));
    const viewer = screen.getByRole("region", { name: /disassembled webassembly text/i });
    expect(within(viewer).getByText('"hello"')).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText(/search instructions/i), { target: { value: "CALL $import0" } });
    expect(await screen.findByText(/match 1 of 1 lines/i)).toBeInTheDocument();
    // The match spans the instruction and identifier tokens, so it is marked piecewise.
    const marked = Array.from(viewer.querySelectorAll("mark"), (mark) => mark.textContent).join("");
    expect(marked).toBe("call $import0");

    fireEvent.change(screen.getByLabelText(/search instructions/i), { target: { value: "i32.store" } });
    expect(await screen.findByText("No matching lines")).toBeInTheDocument();
  });

  it("reports files that are not WebAssembly", async () => {
    render(<WasmDisassembler />);
    upload(new TextEncoder().encode("not wasm"), "notes.txt");

    expect(await screen.findByRole("alert")).toHaveTextContent(/not a webassembly module/i);
  });
});
