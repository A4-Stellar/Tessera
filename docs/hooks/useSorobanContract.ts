import { useCallback } from 'react';
import { signTransaction } from '@stellar/freighter-api';

export function useSorobanContract() {
  const invoke = useCallback(async (xdr: string, network: string) => {
    try {
      const signedXdr = await signTransaction(xdr, { network });
      return signedXdr;
    } catch (error: any) {
      throw new Error(error.message || 'Failed to sign transaction');
    }
  }, []);

  return { invoke };
}
