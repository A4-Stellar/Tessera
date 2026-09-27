import { useCallback } from 'react';
import { signTransaction } from '@stellar/freighter-api';

export function useSorobanContract() {
  const invoke = useCallback(async (xdr: string, network: string) => {
    try {
      const result = await signTransaction(xdr, { networkPassphrase: network });
      if (result.error) throw new Error(result.error.message);
      return result.signedTxXdr;
    } catch (error) {
      throw new Error(error instanceof Error ? error.message : 'Failed to sign transaction');
    }
  }, []);

  return { invoke };
}
