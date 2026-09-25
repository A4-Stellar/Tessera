import { useState, useEffect, useCallback } from 'react';
import {
  isConnected,
  isAllowed,
  requestAccess,
  getAddress,
  getNetworkDetails,
} from '@stellar/freighter-api';

export function useFreighterWallet() {
  const [address, setAddress] = useState<string | null>(null);
  const [network, setNetwork] = useState<string | null>(null);
  const [isInstalled, setIsInstalled] = useState<boolean>(false);
  const [isConnecting, setIsConnecting] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let intervalId: NodeJS.Timeout;
    
    const checkInstallation = async () => {
      if (typeof window !== 'undefined') {
        const connected = await isConnected();
        setIsInstalled(connected);
        if (connected) {
          const allowed = await isAllowed();
          if (allowed) {
            try {
              const addr = await getAddress();
              setAddress(addr);
              const net = await getNetworkDetails();
              setNetwork(net.network);
            } catch (err) {
              console.error(err);
            }
          }
        }
      }
    };
    checkInstallation();

    // Poll for account or network changes
    intervalId = setInterval(async () => {
      if (typeof window !== 'undefined') {
        const connected = await isConnected();
        if (connected) {
          const allowed = await isAllowed();
          if (allowed) {
            try {
              const addr = await getAddress();
              const net = await getNetworkDetails();
              setAddress((prev) => {
                if (prev !== addr) return addr;
                return prev;
              });
              setNetwork((prev) => {
                if (prev !== net.network) return net.network;
                return prev;
              });
            } catch (err) {
              console.error(err);
            }
          }
        }
      }
    }, 3000);

    return () => clearInterval(intervalId);
  }, []);

  const connect = useCallback(async () => {
    setIsConnecting(true);
    setError(null);
    try {
      const allowed = await requestAccess();
      if (allowed) {
        const addr = await getAddress();
        setAddress(addr);
        const net = await getNetworkDetails();
        setNetwork(net.network);
      } else {
        setError('Connection rejected');
      }
    } catch (e: any) {
      setError(e.message || 'Failed to connect');
    } finally {
      setIsConnecting(false);
    }
  }, []);

  return {
    address,
    network,
    isInstalled,
    isConnecting,
    error,
    connect,
  };
}
