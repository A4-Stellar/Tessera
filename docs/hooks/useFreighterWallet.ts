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
      try {
        if (typeof window !== 'undefined') {
          const connected = await isConnected();
          if (connected.error) throw new Error(connected.error.message);
        setIsInstalled(connected.isConnected);
        if (connected.isConnected) {
          const allowed = await isAllowed();
          if (allowed.error) throw new Error(allowed.error.message);
          if (allowed.isAllowed) {
            try {
              const addr = await getAddress();
              if (addr.error) throw new Error(addr.error.message);
              setAddress(addr.address);
              const net = await getNetworkDetails();
              if (net.error) throw new Error(net.error.message);
              setNetwork(net.network);
            } catch (err) {
              console.error(err);
            }
          }
        }
        }
      } catch (err) {
        console.error(err);
      }
    };
    checkInstallation();

    // Poll for account or network changes
    intervalId = setInterval(async () => {
      if (typeof window !== 'undefined') {
        const connected = await isConnected();
        if (connected.error) return;
        if (connected.isConnected) {
          const allowed = await isAllowed();
          if (allowed.error) return;
          if (allowed.isAllowed) {
            try {
              const addr = await getAddress();
              if (addr.error) throw new Error(addr.error.message);
              const net = await getNetworkDetails();
              if (net.error) throw new Error(net.error.message);
              setAddress((prev) => {
                if (prev !== addr.address) return addr.address;
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
      const access = await requestAccess();
      if (access.error) throw new Error(access.error.message);
      if (access.address) {
        const addr = await getAddress();
        if (addr.error) throw new Error(addr.error.message);
        setAddress(addr.address);
        const net = await getNetworkDetails();
        if (net.error) throw new Error(net.error.message);
        setNetwork(net.network);
      } else {
        setError('Connection rejected');
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to connect');
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
