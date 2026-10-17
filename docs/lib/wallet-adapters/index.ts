// Issue #203: Multi-Wallet Browser Extension Compatibility Suite
// Unified adapter interface for Stellar wallet extensions

export interface StellarWalletAdapter {
  /** Wallet name */
  name: string;
  /** Wallet icon URL */
  icon: string;
  /** Check if wallet is installed */
  isInstalled(): boolean;
  /** Connect to wallet */
  connect(): Promise<ConnectResult>;
  /** Disconnect from wallet */
  disconnect(): Promise<void>;
  /** Sign a Stellar transaction */
  signTransaction(xdr: string, network: string): Promise<SignResult>;
  /** Sign arbitrary data blob */
  signBlob(blob: string): Promise<SignResult>;
  /** Get current public key */
  getPublicKey(): Promise<string | null>;
}

export interface ConnectResult {
  publicKey: string;
  network?: string;
}

export interface SignResult {
  signedXdr: string;
  publicKey: string;
}

/**
 * Base wallet adapter with common functionality
 */
export abstract class BaseWalletAdapter implements StellarWalletAdapter {
  abstract name: string;
  abstract icon: string;
  protected publicKey: string | null = null;

  abstract isInstalled(): boolean;
  protected abstract connectWallet(): Promise<ConnectResult>;
  protected abstract signTransactionWithWallet(
    xdr: string,
    network: string,
  ): Promise<SignResult>;
  protected abstract signBlobWithWallet(blob: string): Promise<SignResult>;
  protected abstract disconnectWallet(): Promise<void>;

  async connect(): Promise<ConnectResult> {
    const result = await this.connectWallet();
    this.publicKey = result.publicKey;
    return result;
  }

  async disconnect(): Promise<void> {
    await this.disconnectWallet();
    this.publicKey = null;
  }

  async signTransaction(xdr: string, network: string): Promise<SignResult> {
    if (!this.publicKey) {
      throw new Error(`${this.name}: Not connected`);
    }
    return this.signTransactionWithWallet(xdr, network);
  }

  async signBlob(blob: string): Promise<SignResult> {
    if (!this.publicKey) {
      throw new Error(`${this.name}: Not connected`);
    }
    return this.signBlobWithWallet(blob);
  }

  async getPublicKey(): Promise<string | null> {
    return this.publicKey;
  }
}

/**
 * Freighter Wallet Adapter
 */
export class FreighterAdapter extends BaseWalletAdapter {
  name = 'Freighter';
  icon = 'https://www.freighter.app/icon-128.png';

  isInstalled(): boolean {
    return typeof window !== 'undefined' && 'freighter' in window;
  }

  protected async connectWallet(): Promise<ConnectResult> {
    const freighter = (window as any).freighter;
    if (!freighter) {
      throw new Error('Freighter not installed');
    }

    const publicKey = await freighter.getPublicKey();
    const network = await freighter.getNetwork();

    return { publicKey, network };
  }

  protected async signTransactionWithWallet(
    xdr: string,
    network: string,
  ): Promise<SignResult> {
    const freighter = (window as any).freighter;
    const signedXdr = await freighter.signTransaction(xdr, network);
    return { signedXdr, publicKey: this.publicKey! };
  }

  protected async signBlobWithWallet(blob: string): Promise<SignResult> {
    const freighter = (window as any).freighter;
    const signedBlob = await freighter.signAuthEntry(blob);
    return { signedXdr: signedBlob, publicKey: this.publicKey! };
  }

  protected async disconnectWallet(): Promise<void> {
    // Freighter doesn't have explicit disconnect
    this.publicKey = null;
  }
}

/**
 * Albedo Wallet Adapter
 */
export class AlbedoAdapter extends BaseWalletAdapter {
  name = 'Albedo';
  icon = 'https://albedo.link/icon-128.png';

  isInstalled(): boolean {
    return typeof window !== 'undefined' && 'albedo' in window;
  }

  protected async connectWallet(): Promise<ConnectResult> {
    const albedo = (window as any).albedo;
    if (!albedo) {
      throw new Error('Albedo not installed');
    }

    const result = await albedo.publicKey();
    return { publicKey: result.pubkey };
  }

  protected async signTransactionWithWallet(
    xdr: string,
    network: string,
  ): Promise<SignResult> {
    const albedo = (window as any).albedo;
    const result = await albedo.tx({
      xdr,
      network: network === 'PUBLIC' ? 'public' : 'testnet',
    });
    return { signedXdr: result.signed_envelope_xdr, publicKey: this.publicKey! };
  }

  protected async signBlobWithWallet(blob: string): Promise<SignResult> {
    const albedo = (window as any).albedo;
    const result = await albedo.signMessage({ message: blob });
    return { signedXdr: result.signature, publicKey: this.publicKey! };
  }

  protected async disconnectWallet(): Promise<void> {
    this.publicKey = null;
  }
}

/**
 * Rabet Wallet Adapter
 */
export class RabetAdapter extends BaseWalletAdapter {
  name = 'Rabet';
  icon = 'https://rabet.io/icon-128.png';

  isInstalled(): boolean {
    return typeof window !== 'undefined' && 'rabet' in window;
  }

  protected async connectWallet(): Promise<ConnectResult> {
    const rabet = (window as any).rabet;
    if (!rabet) {
      throw new Error('Rabet not installed');
    }

    await rabet.connect();
    const publicKey = await rabet.getPublicKey();
    return { publicKey };
  }

  protected async signTransactionWithWallet(
    xdr: string,
    network: string,
  ): Promise<SignResult> {
    const rabet = (window as any).rabet;
    const result = await rabet.sign(xdr, network);
    return { signedXdr: result.xdr, publicKey: this.publicKey! };
  }

  protected async signBlobWithWallet(blob: string): Promise<SignResult> {
    const rabet = (window as any).rabet;
    const result = await rabet.signMessage(blob);
    return { signedXdr: result.signature, publicKey: this.publicKey! };
  }

  protected async disconnectWallet(): Promise<void> {
    const rabet = (window as any).rabet;
    await rabet.disconnect();
  }
}

/**
 * xBull Wallet Adapter
 */
export class XBullAdapter extends BaseWalletAdapter {
  name = 'xBull';
  icon = 'https://xbull.app/icon-128.png';

  isInstalled(): boolean {
    return typeof window !== 'undefined' && 'xBullSDK' in window;
  }

  protected async connectWallet(): Promise<ConnectResult> {
    const xBull = (window as any).xBullSDK;
    if (!xBull) {
      throw new Error('xBull not installed');
    }

    const result = await xBull.connect();
    return { publicKey: result.publicKey };
  }

  protected async signTransactionWithWallet(
    xdr: string,
    network: string,
  ): Promise<SignResult> {
    const xBull = (window as any).xBullSDK;
    const result = await xBull.signXDR(xdr, { network });
    return { signedXdr: result.signedXDR, publicKey: this.publicKey! };
  }

  protected async signBlobWithWallet(blob: string): Promise<SignResult> {
    const xBull = (window as any).xBullSDK;
    const result = await xBull.signMessage(blob);
    return { signedXdr: result.signature, publicKey: this.publicKey! };
  }

  protected async disconnectWallet(): Promise<void> {
    const xBull = (window as any).xBullSDK;
    await xBull.disconnect();
  }
}

/**
 * Lobstr Wallet Adapter
 */
export class LobstrAdapter extends BaseWalletAdapter {
  name = 'Lobstr';
  icon = 'https://lobstr.co/icon-128.png';

  isInstalled(): boolean {
    return typeof window !== 'undefined' && 'lobstrWallet' in window;
  }

  protected async connectWallet(): Promise<ConnectResult> {
    const lobstr = (window as any).lobstrWallet;
    if (!lobstr) {
      throw new Error('Lobstr not installed');
    }

    const publicKey = await lobstr.getPublicKey();
    return { publicKey };
  }

  protected async signTransactionWithWallet(
    xdr: string,
    network: string,
  ): Promise<SignResult> {
    const lobstr = (window as any).lobstrWallet;
    const result = await lobstr.signTransaction(xdr, network);
    return { signedXdr: result.signedXdr, publicKey: this.publicKey! };
  }

  protected async signBlobWithWallet(blob: string): Promise<SignResult> {
    const lobstr = (window as any).lobstrWallet;
    const result = await lobstr.signMessage(blob);
    return { signedXdr: result.signature, publicKey: this.publicKey! };
  }

  protected async disconnectWallet(): Promise<void> {
    this.publicKey = null;
  }
}

/**
 * Auto-detect installed wallet extensions
 */
export function detectInstalledWallets(): StellarWalletAdapter[] {
  const adapters: StellarWalletAdapter[] = [
    new FreighterAdapter(),
    new AlbedoAdapter(),
    new RabetAdapter(),
    new XBullAdapter(),
    new LobstrAdapter(),
  ];

  return adapters.filter((adapter) => adapter.isInstalled());
}

/**
 * Get adapter by name
 */
export function getWalletAdapter(name: string): StellarWalletAdapter | null {
  const adapters = {
    freighter: new FreighterAdapter(),
    albedo: new AlbedoAdapter(),
    rabet: new RabetAdapter(),
    xbull: new XBullAdapter(),
    lobstr: new LobstrAdapter(),
  };

  return adapters[name.toLowerCase() as keyof typeof adapters] || null;
}
