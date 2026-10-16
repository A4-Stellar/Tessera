import {
  connectWallet,
  detectInstalledWallets,
  getWalletAdapter,
  hideWalletInstallModal,
  showWalletInstallModal,
  WalletNotInstalledError,
  type WalletAdapter,
  type WalletId,
} from "../lib/wallets/adapter_suite";

jest.mock("@stellar/freighter-api", () => ({
  __esModule: true,
  default: {},
}));

const FREIGHTER_ID = "GALCB4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAMPLE";
const ALBEDO_ID = "GALBEDOPKEXAMPLE000000000000000000000000000";
const RABET_ID = "GRABET0EXAMPLE0000000000000000000000000000";
const XBULL_ID = "GXBULL0EXAMPLE0000000000000000000000000000";
const TESTNET_PASSPHRASE = "Test SDF Network ; September 2015";

type WalletGlobals = {
  freighter?: {
    isConnected: jest.Mock;
    requestAccess: jest.Mock;
    getPublicKey: jest.Mock;
    signTransaction: jest.Mock;
    disconnect: jest.Mock;
  };
  freighterApi?: unknown;
  rabet?: { connect: jest.Mock; sign: jest.Mock; disconnect: jest.Mock };
  xBullSDK?: {
    connect: jest.Mock;
    getPublicKey: jest.Mock;
    signXDR: jest.Mock;
    disconnect: jest.Mock;
  };
  albedo?: jest.Mock;
  lobstrSignerExtension?: unknown;
  postMessage?: jest.Mock;
};

function gw(): WalletGlobals {
  return window as unknown as WalletGlobals;
}

interface LobstrMock {
  received: Array<Record<string, unknown>>;
}

function lobstrRespond(payload: Record<string, unknown>): void {
  window.dispatchEvent(
    new MessageEvent("message", {
      source: window,
      data: { source: "LOBSTR_EXTERNAL_MSG_RESPONSE", ...payload },
    })
  );
}

function installLobstr(lobstr: LobstrMock): void {
  gw().lobstrSignerExtension = { isConnected: true };
  gw().postMessage = jest.fn((message: Record<string, unknown>) => {
    lobstr.received.push(message);
    const messageId = message.messageId;
    if (message.type === "REQUEST_ACCESS") {
      lobstrRespond({
        messagedId: messageId,
        publicKey: FREIGHTER_ID,
        connectionKey: "lobstr-session-42",
        isConnected: true,
      });
    } else if (message.type === "SIGN") {
      lobstrRespond({
        messagedId: messageId,
        signedData: "AAAA-SIGNED-LOBSTR",
        signerAddress: FREIGHTER_ID,
      });
    } else {
      lobstrRespond({ messagedId: messageId, isConnected: true });
    }
  });
}

function installAdapters(): void {
  Object.assign(window, {
    freighter: {
      isConnected: jest.fn().mockResolvedValue(true),
      requestAccess: jest.fn().mockResolvedValue({ address: FREIGHTER_ID }),
      getPublicKey: jest.fn().mockResolvedValue(FREIGHTER_ID),
      signTransaction: jest.fn().mockResolvedValue({ signedTxXdr: "AAAA-SIGNED-FREIGHTER" }),
      disconnect: jest.fn().mockResolvedValue(undefined),
    },
    rabet: {
      connect: jest.fn().mockResolvedValue({ publicKey: RABET_ID, error: "" }),
      sign: jest.fn().mockResolvedValue({ xdr: "AAAA-SIGNED-RABET", error: "" }),
      disconnect: jest.fn().mockResolvedValue({ error: "" }),
    },
    xBullSDK: {
      connect: jest.fn().mockResolvedValue(undefined),
      getPublicKey: jest.fn().mockResolvedValue(XBULL_ID),
      signXDR: jest.fn().mockResolvedValue("AAAA-SIGNED-XBULL"),
      disconnect: jest.fn().mockResolvedValue(undefined),
    },
    albedo: jest
      .fn()
      .mockResolvedValueOnce({ pubkey: ALBEDO_ID })
      .mockResolvedValueOnce({ signed_envelope_xdr: "AAAA-SIGNED-ALBEDO" }),
    lobstrSignerExtension: undefined,
  });
}

function uninstallAdapters(): void {
  for (const key of [
    "freighter",
    "freighterApi",
    "rabet",
    "xBullSDK",
    "albedo",
    "lobstrSignerExtension",
  ]) {
    delete (window as unknown as Record<string, unknown>)[key];
  }
}

function modalRoot(): Element | null {
  return document.querySelector('[data-testid="wallet-install-modal"]');
}

async function expectRoundTrip(adapter: WalletAdapter, expectedId: string): Promise<void> {
  const address = await adapter.connect();
  expect(address).toBe(expectedId);
  const signed = await adapter.signTransaction("AAAA-ENVELOPE", {
    networkPassphrase: TESTNET_PASSPHRASE,
  });
  expect(typeof signed).toBe("string");
  expect(signed.startsWith("AAAA-SIGNED")).toBe(true);
}

describe("adapter_suite", () => {
  beforeAll(() => {
    Object.defineProperty(window, "postMessage", {
      configurable: true,
      writable: true,
      value: jest.fn(),
    });
  });

  beforeEach(() => {
    installAdapters();
    sessionStorage.clear();
    hideWalletInstallModal();
  });

  afterEach(() => {
    hideWalletInstallModal();
    uninstallAdapters();
    sessionStorage.clear();
    jest.clearAllMocks();
  });

  it("exposes the full WalletAdapter interface for all five wallets", () => {
    const ids: WalletId[] = ["freighter", "albedo", "rabet", "xbull", "lobstr"];
    for (const id of ids) {
      const adapter = getWalletAdapter(id);
      expect(adapter.id).toBe(id);
      expect(typeof adapter.name).toBe("string");
      expect(adapter.installUrl).toMatch(/^https:\/\//);
      expect(typeof adapter.isInstalled).toBe("function");
      expect(typeof adapter.connect).toBe("function");
      expect(typeof adapter.disconnect).toBe("function");
      expect(typeof adapter.signTransaction).toBe("function");
      expect(typeof adapter.getPublicKey).toBe("function");
    }
  });

  it("detects every injected wallet", () => {
    expect(detectInstalledWallets()).toEqual(["freighter", "albedo", "rabet", "xbull"]);
  });

  it("detects nothing when no wallet is injected", () => {
    uninstallAdapters();
    expect(detectInstalledWallets()).toEqual([]);
  });

  describe("freighter", () => {
    it("connects, signs and disconnects", async () => {
      const adapter = getWalletAdapter("freighter");
      await expectRoundTrip(adapter, FREIGHTER_ID);
      expect(gw().freighter!.requestAccess).toHaveBeenCalled();
      expect(gw().freighter!.signTransaction).toHaveBeenCalledWith(
        "AAAA-ENVELOPE",
        expect.objectContaining({
          networkPassphrase: TESTNET_PASSPHRASE,
          address: FREIGHTER_ID,
        })
      );
      expect(adapter.getPublicKey()).toBe(FREIGHTER_ID);
      await adapter.disconnect();
      expect(gw().freighter!.disconnect).toHaveBeenCalled();
      expect(adapter.getPublicKey()).toBeNull();
    });

    it("throws WalletNotInstalledError when the extension is missing", async () => {
      uninstallAdapters();
      await expect(getWalletAdapter("freighter").connect()).rejects.toBeInstanceOf(
        WalletNotInstalledError
      );
      await expect(getWalletAdapter("freighter").connect()).rejects.toMatchObject({
        walletId: "freighter",
      });
    });
  });

  describe("albedo", () => {
    it("connects, signs and disconnects", async () => {
      const adapter = getWalletAdapter("albedo");
      const address = await adapter.connect();
      expect(address).toBe(ALBEDO_ID);
      expect(gw().albedo).toHaveBeenCalledWith({ intent: "publickey" });
      const signed = await adapter.signTransaction("AAAA-ENVELOPE", {
        networkPassphrase: TESTNET_PASSPHRASE,
      });
      expect(signed).toBe("AAAA-SIGNED-ALBEDO");
      expect(gw().albedo).toHaveBeenCalledWith(
        expect.objectContaining({
          intent: "tx",
          xdr: "AAAA-ENVELOPE",
          pubkey: ALBEDO_ID,
        })
      );
      expect(adapter.getPublicKey()).toBe(ALBEDO_ID);
      await adapter.disconnect();
      expect(adapter.getPublicKey()).toBeNull();
    });

    it("throws WalletNotInstalledError when the script is missing", async () => {
      uninstallAdapters();
      await expect(getWalletAdapter("albedo").connect()).rejects.toBeInstanceOf(
        WalletNotInstalledError
      );
    });
  });

  describe("rabet", () => {
    it("connects, signs on testnet and disconnects", async () => {
      const adapter = getWalletAdapter("rabet");
      const address = await adapter.connect();
      expect(address).toBe(RABET_ID);
      expect(gw().rabet!.connect).toHaveBeenCalled();
      const signed = await adapter.signTransaction("AAAA-ENVELOPE", {
        networkPassphrase: TESTNET_PASSPHRASE,
      });
      expect(signed).toBe("AAAA-SIGNED-RABET");
      expect(gw().rabet!.sign).toHaveBeenCalledWith("AAAA-ENVELOPE", "testnet");
      await adapter.disconnect();
      expect(gw().rabet!.disconnect).toHaveBeenCalled();
      expect(adapter.getPublicKey()).toBeNull();
    });

    it("maps a mainnet passphrase to mainnet", async () => {
      const adapter = getWalletAdapter("rabet");
      await adapter.connect();
      await adapter.signTransaction("AAAA-ENVELOPE", {
        networkPassphrase: "Public Global Stellar Network ; September 2015",
      });
      expect(gw().rabet!.sign).toHaveBeenCalledWith("AAAA-ENVELOPE", "mainnet");
    });

    it("throws WalletNotInstalledError when the extension is missing", async () => {
      uninstallAdapters();
      await expect(getWalletAdapter("rabet").connect()).rejects.toBeInstanceOf(
        WalletNotInstalledError
      );
    });
  });

  describe("xbull", () => {
    it("connects, signs with passphrase + publicKey and disconnects", async () => {
      const adapter = getWalletAdapter("xbull");
      const address = await adapter.connect();
      expect(address).toBe(XBULL_ID);
      expect(gw().xBullSDK!.connect).toHaveBeenCalledWith({
        canRequestPublicKey: true,
        canRequestSign: true,
      });
      const signed = await adapter.signTransaction("AAAA-ENVELOPE", {
        networkPassphrase: TESTNET_PASSPHRASE,
      });
      expect(signed).toBe("AAAA-SIGNED-XBULL");
      expect(gw().xBullSDK!.signXDR).toHaveBeenCalledWith("AAAA-ENVELOPE", {
        network: TESTNET_PASSPHRASE,
        publicKey: XBULL_ID,
      });
      expect(adapter.getPublicKey()).toBe(XBULL_ID);
      await adapter.disconnect();
      expect(gw().xBullSDK!.disconnect).toHaveBeenCalled();
      expect(adapter.getPublicKey()).toBeNull();
    });

    it("throws WalletNotInstalledError when the SDK is missing", async () => {
      uninstallAdapters();
      await expect(getWalletAdapter("xbull").connect()).rejects.toBeInstanceOf(
        WalletNotInstalledError
      );
    });
  });

  describe("lobstr", () => {
    let lobstr: LobstrMock;

    beforeEach(() => {
      lobstr = { received: [] };
      installLobstr(lobstr);
    });

    it("is detected once the extension global exists", () => {
      expect(detectInstalledWallets()).toContain("lobstr");
      expect(getWalletAdapter("lobstr").isInstalled()).toBe(true);
    });

    it("completes the REQUEST_ACCESS handshake and stores the connection key", async () => {
      const adapter = getWalletAdapter("lobstr");
      const address = await adapter.connect();
      expect(address).toBe(FREIGHTER_ID);
      const access = lobstr.received.find((message) => message.type === "REQUEST_ACCESS");
      expect(access).toBeDefined();
      expect(access!.source).toBe("LOBSTR_EXTERNAL_MSG_REQUEST");
      expect(access!.version).toBe(1);
      expect(sessionStorage.getItem("LOBSTR_CONNECTION_KEY")).toBe("lobstr-session-42");
      expect(adapter.getPublicKey()).toBe(FREIGHTER_ID);
    });

    it("signs a transaction through the postMessage protocol", async () => {
      const adapter = getWalletAdapter("lobstr");
      await adapter.connect();
      const signed = await adapter.signTransaction("AAAA-ENVELOPE", {
        networkPassphrase: TESTNET_PASSPHRASE,
      });
      expect(signed).toBe("AAAA-SIGNED-LOBSTR");
      const signMessage = lobstr.received.find((message) => message.type === "SIGN");
      expect(signMessage).toBeDefined();
      expect(signMessage!.dataToSign).toBe("AAAA-ENVELOPE");
      expect(signMessage!.connectionKey).toBe("lobstr-session-42");
      expect(signMessage!.signType).toBe("transaction");
    });

    it("disconnects by clearing the stored connection key", async () => {
      const adapter = getWalletAdapter("lobstr");
      await adapter.connect();
      expect(sessionStorage.getItem("LOBSTR_CONNECTION_KEY")).toBe("lobstr-session-42");
      await adapter.disconnect();
      expect(sessionStorage.getItem("LOBSTR_CONNECTION_KEY")).toBeNull();
      expect(adapter.getPublicKey()).toBeNull();
    });

    it("throws WalletNotInstalledError when the extension is missing", async () => {
      delete gw().lobstrSignerExtension;
      await expect(getWalletAdapter("lobstr").connect()).rejects.toBeInstanceOf(
        WalletNotInstalledError
      );
    });
  });

  describe("connectWallet", () => {
    it("connects the first detected wallet when no id is given", async () => {
      const result = await connectWallet();
      expect(result).toEqual({ id: "freighter", address: FREIGHTER_ID });
      expect(modalRoot()).toBeNull();
    });

    it("connects the explicitly requested wallet only", async () => {
      const result = await connectWallet("rabet");
      expect(result).toEqual({ id: "rabet", address: RABET_ID });
      expect(gw().rabet!.connect).toHaveBeenCalled();
      expect(gw().freighter!.requestAccess).not.toHaveBeenCalled();
    });

    it("rejects and opens the install modal when no wallet is installed", async () => {
      uninstallAdapters();
      await expect(connectWallet()).rejects.toBeInstanceOf(WalletNotInstalledError);
      expect(modalRoot()).not.toBeNull();
      expect(document.querySelector('[role="dialog"]')).not.toBeNull();
    });

    it("opens the install modal when the requested wallet is missing", async () => {
      uninstallAdapters();
      await expect(connectWallet("freighter")).rejects.toBeInstanceOf(WalletNotInstalledError);
      expect(modalRoot()).not.toBeNull();
      expect(
        document.body.textContent
      ).toContain("Freighter was not detected in this browser");
    });

    it("wraps connect-time failures without opening the modal", async () => {
      gw().rabet!.connect.mockRejectedValueOnce(new Error("user refused"));
      await expect(connectWallet("rabet")).rejects.toThrow("user refused");
      expect(modalRoot()).toBeNull();
    });
  });

  describe("install modal", () => {
    const links: Record<string, string> = {
      freighter: "https://www.freighter.app/",
      albedo: "https://albedo.link/",
      rabet: "https://rabet.io/",
      xbull: "https://xbull.app/",
      lobstr: "https://lobstr.co/",
    };

    it("renders an accessible dialog with every wallet option", () => {
      showWalletInstallModal();
      const root = modalRoot();
      expect(root).not.toBeNull();
      const dialog = root!.querySelector('[role="dialog"]');
      expect(dialog).not.toBeNull();
      expect(dialog!.getAttribute("aria-modal")).toBe("true");
      for (const name of ["Freighter", "Albedo", "Rabet", "xBull", "LOBSTR"]) {
        expect(root!.textContent).toContain(name);
      }
    });

    it.each(Object.entries(links))(
      "links %s to its official install page in a new tab",
      (id, href) => {
        uninstallAdapters();
        showWalletInstallModal();
        const link = document.querySelector(
          `[data-testid="wallet-install-link-${id}"]`
        ) as HTMLAnchorElement;
        expect(link).not.toBeNull();
        expect(link.getAttribute("href")).toBe(href);
        expect(link.getAttribute("target")).toBe("_blank");
        expect(link.getAttribute("rel")).toContain("noopener");
        hideWalletInstallModal();
      }
    );

    it("shows Detected badges for installed wallets and links for the rest", () => {
      showWalletInstallModal();
      for (const id of ["freighter", "albedo", "rabet", "xbull"]) {
        expect(document.querySelector(`[data-testid="wallet-detected-${id}"]`)).not.toBeNull();
        expect(document.querySelector(`[data-testid="wallet-install-link-${id}"]`)).toBeNull();
      }
      expect(document.querySelector('[data-testid="wallet-detected-lobstr"]')).toBeNull();
      expect(document.querySelector('[data-testid="wallet-install-link-lobstr"]')).not.toBeNull();
    });

    it("closes when the Close button is clicked", () => {
      showWalletInstallModal();
      const closeButton = document.querySelector(
        '[data-testid="wallet-install-modal-close"]'
      ) as HTMLButtonElement;
      expect(closeButton).not.toBeNull();
      closeButton.click();
      expect(modalRoot()).toBeNull();
    });

    it("closes when Escape is pressed", () => {
      showWalletInstallModal();
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
      expect(modalRoot()).toBeNull();
    });

    it("closes when the backdrop is clicked", () => {
      showWalletInstallModal();
      const overlay = modalRoot() as HTMLElement;
      fireEventClick(overlay);
      expect(modalRoot()).toBeNull();
    });

    it("does not close when the dialog interior is clicked", () => {
      showWalletInstallModal();
      const dialog = modalRoot()!.querySelector('[role="dialog"]') as HTMLElement;
      dialog.click();
      expect(modalRoot()).not.toBeNull();
      hideWalletInstallModal();
    });

    it("closes via hideWalletInstallModal", () => {
      showWalletInstallModal();
      hideWalletInstallModal();
      expect(modalRoot()).toBeNull();
    });

    it("replaces an existing modal instead of stacking", () => {
      uninstallAdapters();
      showWalletInstallModal();
      showWalletInstallModal("albedo");
      expect(document.querySelectorAll('[data-testid="wallet-install-modal"]')).toHaveLength(1);
      expect(document.body.textContent).toContain("Albedo was not detected");
    });
  });
});

function fireEventClick(element: HTMLElement): void {
  element.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
}
