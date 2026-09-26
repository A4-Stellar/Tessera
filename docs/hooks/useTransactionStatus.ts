import { useState, useCallback } from 'react';

export function useTransactionStatus() {
  const [status, setStatus] = useState<'idle' | 'pending' | 'success' | 'error'>('idle');
  const [hash, setHash] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const startTransaction = useCallback(() => {
    setStatus('pending');
    setError(null);
    setHash(null);
  }, []);

  const succeedTransaction = useCallback((txHash: string) => {
    setStatus('success');
    setHash(txHash);
  }, []);

  const failTransaction = useCallback((err: string) => {
    setStatus('error');
    setError(err);
  }, []);

  return { status, hash, error, startTransaction, succeedTransaction, failTransaction };
}
