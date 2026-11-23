/**
 * Typed client for the SEP-24 (Hosted Deposit and Withdrawal) endpoints.
 *
 * All endpoints live at the API root (not `/v1`) so an anchor can advertise
 * them directly from its `stellar.toml`. Import `API_BASE_URL` from `lib/api`
 * so the same `NEXT_PUBLIC_API_BASE_URL` convention applies everywhere.
 */

import { API_BASE_URL } from "@/lib/api";

export type Sep24TransactionStatus =
  | "incomplete"
  | "pending_user_transfer_start"
  | "pending_external"
  | "completed"
  | "error";

export type Sep24KycStatus =
  | "not_started"
  | "pending"
  | "needs_review"
  | "approved"
  | "reverification_required";

/** SEP-24 transaction object (the global anonymizer may mask KYC fields). */
export interface Sep24Transaction {
  id: string;
  kind: "deposit";
  account: string;
  asset_code: string;
  status: Sep24TransactionStatus;
  status_history: string[];
  amount_in: string | null;
  amount_out: string | null;
  deposit_memo: string | null;
  deposit_memo_type: string | null;
  external_extra: Record<string, unknown> | null;
  kyc_status: Sep24KycStatus;
  first_name: string | null;
  last_name: string | null;
  dob: string | null;
  country: string | null;
  id_type: string | null;
  id_number: string | null;
  tax_id: string | null;
  email: string | null;
  lang: string;
  kyc_verified_at: string | null;
  created_at: string;
  updated_at: string;
  /** Present only when the anchor's signing service succeeded. */
  signed_transaction?: string;
}

export interface Sep24DepositResponse {
  id: string;
  url: string;
  token: string;
  type: "interactive";
}

export interface Sep24KycPayload {
  first_name: string;
  last_name: string;
  dob: string;
  country: string;
  id_type: string;
  id_number: string;
  tax_id: string;
}

interface KycResponse {
  transaction: Sep24Transaction;
  allowlist: "registered" | "needs_review";
}

interface TransactionResponse {
  transaction: Sep24Transaction;
}

interface ListResponse {
  transactions: Sep24Transaction[];
}

/** Error thrown for non-2xx responses or malformed JSON bodies. */
export class Sep24ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
  ) {
    super(message);
    this.name = "Sep24ApiError";
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const url = `${API_BASE_URL.replace(/\/$/, "")}${path}`;
  const headers = new Headers(init?.headers);
  headers.set("Accept", "application/json");
  if (init?.body) headers.set("Content-Type", "application/json");

  let response: Response;
  try {
    response = await fetch(url, { ...init, headers });
  } catch (cause) {
    throw new Sep24ApiError(
      `Could not reach the SEP-24 API at ${url}: ${cause instanceof Error ? cause.message : String(cause)}`,
      0,
    );
  }

  if (!response.ok) {
    let detail = response.statusText;
    try {
      const body = (await response.json()) as { message?: string; error?: string };
      detail = body.message ?? body.error ?? detail;
    } catch {
      // Non-JSON error body — keep the HTTP status text.
    }
    throw new Sep24ApiError(detail || `HTTP ${response.status}`, response.status);
  }

  try {
    return (await response.json()) as T;
  } catch {
    throw new Sep24ApiError("SEP-24 API returned a non-JSON body", response.status);
  }
}

export const sep24 = {
  /** Start an interactive deposit. Returns the docs URL and one-time token. */
  initiateDeposit(payload: {
    account: string;
    asset_code: string;
    amount?: string;
    lang?: string;
    email?: string;
  }): Promise<Sep24DepositResponse> {
    return request<Sep24DepositResponse>(
      "/sep24/transactions/deposit/interactive",
      { method: "POST", body: JSON.stringify(payload) },
    );
  },

  /** Fetch a single transaction. The token is the one-time secret from deposit. */
  getTransaction(id: string, token: string): Promise<Sep24Transaction> {
    const query = new URLSearchParams({ id, token });
    return request<TransactionResponse>(`/sep24/transaction?${query.toString()}`).then(
      (body) => body.transaction,
    );
  },

  /** Submit KYC and register the compliance allowlist entry. */
  submitKyc(id: string, payload: Sep24KycPayload): Promise<KycResponse> {
    return request<KycResponse>(`/sep24/transactions/${encodeURIComponent(id)}/kyc`, {
      method: "POST",
      body: JSON.stringify(payload),
    });
  },

  /** Internal helper: tell the anchor the external transfer was observed. */
  confirmExternal(id: string): Promise<Sep24Transaction> {
    return request<TransactionResponse>(
      `/sep24/transactions/${encodeURIComponent(id)}/confirm_external`,
      { method: "POST" },
    ).then((body) => body.transaction);
  },

  /** List an account's SEP-24 transactions (optionally scoped to one token). */
  listByAccount(account: string, token?: string): Promise<Sep24Transaction[]> {
    const query = new URLSearchParams({ account });
    if (token) query.set("token", token);
    return request<ListResponse>(`/sep24/transactions?${query.toString()}`).then(
      (body) => body.transactions,
    );
  },
};
