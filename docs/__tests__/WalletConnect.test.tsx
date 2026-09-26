import React from 'react';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { WalletConnect } from '../components/WalletConnect';
import * as FreighterAPI from '@stellar/freighter-api';

jest.mock('@stellar/freighter-api');

describe('WalletConnect', () => {
  beforeEach(() => {
    jest.clearAllMocks();
  });

  it('shows not installed message if wallet is not installed', async () => {
    (FreighterAPI.isConnected as jest.Mock).mockResolvedValue(false);
    
    render(<WalletConnect />);
    
    await waitFor(() => {
      expect(screen.getByText('Freighter Wallet is not installed.')).toBeInTheDocument();
    });
  });

  it('shows connect button if wallet is installed but not connected', async () => {
    (FreighterAPI.isConnected as jest.Mock).mockResolvedValue(true);
    (FreighterAPI.isAllowed as jest.Mock).mockResolvedValue(false);
    
    render(<WalletConnect />);
    
    await waitFor(() => {
      expect(screen.getByText('Connect Wallet')).toBeInTheDocument();
    });
  });

  it('connects and displays address/network on success', async () => {
    (FreighterAPI.isConnected as jest.Mock).mockResolvedValue(true);
    (FreighterAPI.isAllowed as jest.Mock).mockResolvedValue(false);
    (FreighterAPI.requestAccess as jest.Mock).mockResolvedValue(true);
    (FreighterAPI.getAddress as jest.Mock).mockResolvedValue('GBXXX123');
    (FreighterAPI.getNetworkDetails as jest.Mock).mockResolvedValue({ network: 'TESTNET' });
    
    render(<WalletConnect />);
    
    const connectButton = await screen.findByText('Connect Wallet');
    fireEvent.click(connectButton);
    
    await waitFor(() => {
      expect(screen.getByTestId('wallet-address')).toHaveTextContent('Address: GBXXX123');
      expect(screen.getByTestId('wallet-network')).toHaveTextContent('Network: TESTNET');
    });
    
    expect(screen.queryByText('Warning: You are not on Testnet!')).not.toBeInTheDocument();
  });

  it('shows warning when not on Testnet', async () => {
    (FreighterAPI.isConnected as jest.Mock).mockResolvedValue(true);
    (FreighterAPI.isAllowed as jest.Mock).mockResolvedValue(false);
    (FreighterAPI.requestAccess as jest.Mock).mockResolvedValue(true);
    (FreighterAPI.getAddress as jest.Mock).mockResolvedValue('GBXXX123');
    (FreighterAPI.getNetworkDetails as jest.Mock).mockResolvedValue({ network: 'PUBLIC' });
    
    render(<WalletConnect />);
    
    const connectButton = await screen.findByText('Connect Wallet');
    fireEvent.click(connectButton);
    
    await waitFor(() => {
      expect(screen.getByText('Warning: You are not on Testnet!')).toBeInTheDocument();
    });
  });
});
