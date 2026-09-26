import React from 'react';
import { useFreighterWallet } from '../hooks/useFreighterWallet';

export const WalletConnect = () => {
  const { address, network, isInstalled, isConnecting, error, connect } = useFreighterWallet();

  const isTestnet = network === 'TESTNET';

  return (
    <div className="wallet-connect">
      {!isInstalled ? (
        <p>Freighter Wallet is not installed.</p>
      ) : (
        <div>
          {!address ? (
            <button onClick={connect} disabled={isConnecting}>
              {isConnecting ? 'Connecting...' : 'Connect Wallet'}
            </button>
          ) : (
            <div>
              <p data-testid="wallet-address">Address: {address}</p>
              <p data-testid="wallet-network">Network: {network}</p>
              {!isTestnet && <p className="network-warning">Warning: You are not on Testnet!</p>}
            </div>
          )}
          {error && <p className="error" data-testid="wallet-error">{error}</p>}
        </div>
      )}
    </div>
  );
};
