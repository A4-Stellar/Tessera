"use client";

import { useEffect, useMemo, useState, useCallback } from "react";
import { API_BASE_URL } from "@/lib/api";

interface Proposal {
  id: string;
  title: string;
  description: string;
  proposer: string;
  start_time: number;
  end_time: number;
  quorum: string;
  votes_for: string;
  votes_against: string;
  votes_abstain: string;
  executed: boolean;
  status: "active" | "passed" | "rejected" | "executed" | "pending";
}

interface VoteWeight {
  address: string;
  weight: string;
}

interface GovernancePortalProps {
  /** Contract address of the governance contract */
  contractAddress: string;
  /** Overrides the configured API base URL */
  apiBaseUrl?: string;
  /** Freighter wallet API instance (injected by Freighter extension) */
  freighter?: any;
}

type Tab = "proposals" | "vote" | "delegate" | "history";

const STATUS_COLORS: Record<string, string> = {
  active: "text-green-400 bg-green-500/10 border-green-500/20",
  passed: "text-blue-400 bg-blue-500/10 border-blue-500/20",
  rejected: "text-red-400 bg-red-500/10 border-red-500/20",
  executed: "text-purple-400 bg-purple-500/10 border-purple-500/20",
  pending: "text-yellow-400 bg-yellow-500/10 border-yellow-500/20",
};

function formatTimestamp(timestamp: number): string {
  return new Date(timestamp * 1000).toLocaleString();
}

function formatAddress(address: string): string {
  return address.length > 12
    ? `${address.slice(0, 6)}…${address.slice(-4)}`
    : address;
}

function calculateProgress(votesFor: string, votesAgainst: string, quorum: string): number {
  const total = BigInt(votesFor) + BigInt(votesAgainst);
  const quorumBI = BigInt(quorum);
  if (quorumBI === 0n) return 0;
  return Math.min(100, Number((total * 100n) / quorumBI));
}

function calculateApproval(votesFor: string, votesAgainst: string): number {
  const total = BigInt(votesFor) + BigInt(votesAgainst);
  if (total === 0n) return 0;
  return Number((BigInt(votesFor) * 100n) / total);
}

export function GovernancePortal({
  contractAddress,
  apiBaseUrl,
  freighter: externalFreighter,
}: GovernancePortalProps) {
  const baseUrl = apiBaseUrl ?? API_BASE_URL;
  const [tab, setTab] = useState<Tab>("proposals");
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [selectedProposal, setSelectedProposal] = useState<Proposal | null>(null);
  const [voteWeight, setVoteWeight] = useState<VoteWeight | null>(null);
  const [connectedAddress, setConnectedAddress] = useState<string | null>(null);
  const [freighterApi, setFreighterApi] = useState<any>(externalFreighter ?? null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [voting, setVoting] = useState(false);
  const [delegating, setDelegating] = useState(false);

  useEffect(() => {
    if (typeof window !== "undefined" && (window as any).freighter) {
      setFreighterApi((window as any).freighter);
    }
  }, []);

  const fetchProposals = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const response = await fetch(
        `${baseUrl.replace(/\/+$/, "")}/governance/${contractAddress}/proposals`
      );
      if (!response.ok) {
        throw new Error(`Failed to fetch proposals (${response.status})`);
      }
      const data = await response.json();
      setProposals(data.proposals || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load proposals");
    } finally {
      setLoading(false);
    }
  }, [baseUrl, contractAddress]);

  const fetchVoteWeight = useCallback(async (address: string) => {
    try {
      const response = await fetch(
        `${baseUrl.replace(/\/+$/, "")}/governance/${contractAddress}/vote-weight/${address}`
      );
      if (response.ok) {
        const data = await response.json();
        setVoteWeight({ address, weight: data.weight });
      }
    } catch {
      // Ignore vote weight fetch errors
    }
  }, [baseUrl, contractAddress]);

  useEffect(() => {
    fetchProposals();
  }, [fetchProposals]);

  useEffect(() => {
    if (connectedAddress) {
      fetchVoteWeight(connectedAddress);
    }
  }, [connectedAddress, fetchVoteWeight]);

  const connectWallet = async () => {
    if (!freighterApi) {
      setError("Freighter wallet not detected. Please install the Freighter extension.");
      return;
    }
    try {
      const address = await freighterApi.getAddress();
      setConnectedAddress(address);
      await fetchVoteWeight(address);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to connect wallet");
    }
  };

  const castVote = async (proposalId: string, support: number) => {
    if (!freighterApi || !connectedAddress) {
      setError("Wallet not connected");
      return;
    }

    setVoting(true);
    setError(null);

    try {
      const tx = {
        contractAddress,
        functionName: "cast_vote",
        args: [proposalId, support],
        fee: "100",
      };

      const signedTx = await freighterApi.signTransaction(tx);
      const response = await fetch(
        `${baseUrl.replace(/\/+$/, "")}/transactions/submit`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ xdr: signedTx }),
        }
      );

      if (!response.ok) {
        throw new Error(`Vote submission failed (${response.status})`);
      }

      setSelectedProposal(null);
      await fetchProposals();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to cast vote");
    } finally {
      setVoting(false);
    }
  };

  const delegateVote = async (delegateTo: string) => {
    if (!freighterApi || !connectedAddress) {
      setError("Wallet not connected");
      return;
    }

    setDelegating(true);
    setError(null);

    try {
      const tx = {
        contractAddress,
        functionName: "delegate_vote",
        args: [delegateTo],
        fee: "100",
      };

      const signedTx = await freighterApi.signTransaction(tx);
      const response = await fetch(
        `${baseUrl.replace(/\/+$/, "")}/transactions/submit`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ xdr: signedTx }),
        }
      );

      if (!response.ok) {
        throw new Error(`Delegation failed (${response.status})`);
      }

      await fetchVoteWeight(connectedAddress);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delegate vote");
    } finally {
      setDelegating(false);
    }
  };

  const activeProposals = proposals.filter((p) => p.status === "active");
  const pastProposals = proposals.filter((p) => p.status !== "active");

  const handleTabKeyDown = (e: React.KeyboardEvent<HTMLButtonElement>) => {
    const tabs: Tab[] = ["proposals", "vote", "delegate", "history"];
    const currentIndex = tabs.indexOf(tab);
    if (e.key === "ArrowRight") {
      e.preventDefault();
      setTab(tabs[(currentIndex + 1) % tabs.length]);
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      setTab(tabs[(currentIndex - 1 + tabs.length) % tabs.length]);
    }
  };

  return (
    <div className="rounded-xl border border-white/10 bg-white/[0.03] p-4">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-lg font-semibold text-base-100">
          Governance Portal: {formatAddress(contractAddress)}
        </h2>
        {!connectedAddress ? (
          <button
            onClick={connectWallet}
            className="rounded-md border border-brand-500/40 bg-brand-500/15 px-3 py-1.5 text-sm font-medium text-brand-300 hover:bg-brand-500/25 transition-colors"
          >
            Connect Freighter
          </button>
        ) : (
          <div className="flex items-center gap-2 text-sm">
            <span className="text-base-300">Connected:</span>
            <span className="font-mono text-base-100">{formatAddress(connectedAddress)}</span>
            {voteWeight && (
              <>
                <span className="text-base-300">|</span>
                <span className="text-base-300">Weight:</span>
                <span className="font-mono text-brand-300">{voteWeight.weight}</span>
              </>
            )}
          </div>
        )}
      </div>

      {error && (
        <div
          role="alert"
          className="mb-4 rounded-md bg-red-500/10 border border-red-500/20 p-3 text-sm text-red-300"
        >
          {error}
        </div>
      )}

      <div
        className="mb-4 flex gap-2 border-b border-white/10"
        role="tablist"
        aria-label="Governance portal sections"
      >
        {["proposals", "vote", "delegate", "history"].map((t) => (
          <button
            key={t}
            id={`tab-${t}`}
            type="button"
            role="tab"
            aria-selected={tab === t}
            aria-controls={`panel-${t}`}
            tabIndex={tab === t ? 0 : -1}
            onClick={() => setTab(t as Tab)}
            onKeyDown={handleTabKeyDown}
            className={`rounded-t-md px-3 py-1.5 text-sm font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 ${
              tab === t
                ? "border-b-2 border-brand-500 text-brand-300"
                : "text-base-300 hover:text-base-100"
            }`}
          >
            {t.charAt(0).toUpperCase() + t.slice(1)}
          </button>
        ))}
      </div>

      <div aria-live="polite">
        {tab === "proposals" && (
          <div
            id="panel-proposals"
            role="tabpanel"
            aria-labelledby="tab-proposals"
          >
            {loading ? (
              <p className="text-sm text-base-300">Loading proposals…</p>
            ) : activeProposals.length === 0 ? (
              <p className="text-sm text-base-300">No active proposals at this time.</p>
            ) : (
              <div className="space-y-4">
                {activeProposals.map((proposal) => (
                  <ProposalCard
                    key={proposal.id}
                    proposal={proposal}
                    onSelect={setSelectedProposal}
                    connectedAddress={connectedAddress}
                    voteWeight={voteWeight?.weight}
                  />
                ))}
              </div>
            )}
          </div>
        )}

        {tab === "vote" && (
          <div
            id="panel-vote"
            role="tabpanel"
            aria-labelledby="tab-vote"
          >
            {selectedProposal ? (
              <VotePanel
                proposal={selectedProposal}
                connectedAddress={connectedAddress}
                voteWeight={voteWeight?.weight}
                onVote={castVote}
                voting={voting}
                onClose={() => setSelectedProposal(null)}
              />
            ) : (
              <p className="text-sm text-base-300 text-center py-8">
                Select a proposal from the Proposals tab to vote.
              </p>
            )}
          </div>
        )}

        {tab === "delegate" && (
          <div
            id="panel-delegate"
            role="tabpanel"
            aria-labelledby="tab-delegate"
          >
            <DelegatePanel
              connectedAddress={connectedAddress}
              voteWeight={voteWeight?.weight}
              onDelegate={delegateVote}
              delegating={delegating}
            />
          </div>
        )}

        {tab === "history" && (
          <div
            id="panel-history"
            role="tabpanel"
            aria-labelledby="tab-history"
          >
            {loading ? (
              <p className="text-sm text-base-300">Loading history…</p>
            ) : pastProposals.length === 0 ? (
              <p className="text-sm text-base-300">No past proposals.</p>
            ) : (
              <div className="space-y-3">
                {pastProposals.map((proposal) => (
                  <PastProposalRow key={proposal.id} proposal={proposal} />
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

interface ProposalCardProps {
  proposal: Proposal;
  onSelect: (p: Proposal) => void;
  connectedAddress: string | null;
  voteWeight: string | undefined;
}

function ProposalCard({
  proposal,
  onSelect,
  connectedAddress,
  voteWeight,
}: ProposalCardProps) {
  const progress = calculateProgress(
    proposal.votes_for,
    proposal.votes_against,
    proposal.quorum
  );
  const approval = calculateApproval(proposal.votes_for, proposal.votes_against);
  const timeRemaining = proposal.end_time * 1000 - Date.now();
  const isExpired = timeRemaining <= 0;

  return (
    <div
      className="rounded-lg border border-white/10 bg-white/[0.02] p-4 hover:border-white/20 transition-colors"
    >
      <div className="flex flex-wrap items-start justify-between gap-2 mb-3">
        <h3 className="font-semibold text-base-100">{proposal.title}</h3>
        <span
          className={`rounded-full px-2 py-0.5 text-xs font-medium ${STATUS_COLORS[proposal.status]}`}
        >
          {proposal.status.charAt(0).toUpperCase() + proposal.status.slice(1)}
        </span>
      </div>

      <p className="text-sm text-base-300 mb-3 line-clamp-2">{proposal.description}</p>

      <div className="grid grid-cols-2 gap-3 mb-3 text-sm">
        <div>
          <span className="text-base-300">Proposer: </span>
          <span className="font-mono text-base-100">{formatAddress(proposal.proposer)}</span>
        </div>
        <div>
          <span className="text-base-300">Quorum: </span>
          <span className="font-mono text-base-100">{proposal.quorum}</span>
        </div>
        <div>
          <span className="text-base-300">For: </span>
          <span className="font-mono text-green-400">{proposal.votes_for}</span>
        </div>
        <div>
          <span className="text-base-300">Against: </span>
          <span className="font-mono text-red-400">{proposal.votes_against}</span>
        </div>
      </div>

      <div className="mb-3">
        <div className="flex justify-between text-xs mb-1">
          <span className="text-base-300">Quorum Progress</span>
          <span className="text-base-100">{progress.toFixed(1)}%</span>
        </div>
        <div className="h-2 bg-white/5 rounded-full overflow-hidden">
          <div
            className="h-full bg-gradient-to-r from-green-500 to-blue-500 transition-all"
            style={{ width: `${progress}%` }}
          />
        </div>
      </div>

      <div className="mb-3">
        <div className="flex justify-between text-xs mb-1">
          <span className="text-base-300">Approval Rate</span>
          <span className="text-base-100">{approval.toFixed(1)}%</span>
        </div>
        <div className="h-2 bg-white/5 rounded-full overflow-hidden">
          <div
            className="h-full bg-gradient-to-r from-green-500 to-red-500 transition-all"
            style={{ width: `${approval}%` }}
          />
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2 text-xs text-base-300 mb-3">
        <span>Ends: {formatTimestamp(proposal.end_time)}</span>
        {isExpired && (
          <span className="text-red-400">(Expired)</span>
        )}
        {!isExpired && timeRemaining > 0 && (
          <span className="text-yellow-400">
            {Math.ceil(timeRemaining / 1000 / 60 / 60)}h remaining
          </span>
        )}
      </div>

      <button
        onClick={() => onSelect(proposal)}
        disabled={!connectedAddress || isExpired || proposal.executed}
        className={`w-full rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${
          !connectedAddress
            ? "border border-white/10 bg-white/5 text-base-300 cursor-not-allowed"
            : isExpired || proposal.executed
            ? "border border-white/10 bg-white/5 text-base-300 cursor-not-allowed"
            : "border border-brand-500/40 bg-brand-500/15 text-brand-300 hover:bg-brand-500/25"
        }`}
      >
        {connectedAddress ? (isExpired ? "Voting Ended" : "Vote") : "Connect Wallet to Vote"}
      </button>
    </div>
  );
}

interface VotePanelProps {
  proposal: Proposal;
  connectedAddress: string | null;
  voteWeight: string | undefined;
  onVote: (proposalId: string, support: number) => void;
  voting: boolean;
  onClose: () => void;
}

function VotePanel({
  proposal,
  connectedAddress,
  voteWeight,
  onVote,
  voting,
  onClose,
}: VotePanelProps) {
  return (
    <div className="rounded-lg border border-white/10 bg-white/[0.02] p-4 space-y-4">
      <div className="flex items-start justify-between">
        <h3 className="font-semibold text-base-100">{proposal.title}</h3>
        <button
          onClick={onClose}
          className="text-base-300 hover:text-base-100"
          aria-label="Close vote panel"
        >
          ✕
        </button>
      </div>

      <p className="text-sm text-base-300">{proposal.description}</p>

      <div className="grid grid-cols-2 gap-3 text-sm">
        <div className="rounded-md border border-white/10 p-3">
          <div className="text-xs text-base-300">Your Vote Weight</div>
          <div className="font-mono text-brand-300">{voteWeight || "0"}</div>
        </div>
        <div className="rounded-md border border-white/10 p-3">
          <div className="text-xs text-base-300">Quorum Required</div>
          <div className="font-mono text-base-100">{proposal.quorum}</div>
        </div>
      </div>

      <div className="flex gap-2">
        <button
          onClick={() => onVote(proposal.id, 1)}
          disabled={voting || !connectedAddress}
          className="flex-1 rounded-md border border-green-500/40 bg-green-500/15 px-4 py-2 text-sm font-medium text-green-300 hover:bg-green-500/25 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {voting ? "Voting…" : "Vote FOR"}
        </button>
        <button
          onClick={() => onVote(proposal.id, 0)}
          disabled={voting || !connectedAddress}
          className="flex-1 rounded-md border border-red-500/40 bg-red-500/15 px-4 py-2 text-sm font-medium text-red-300 hover:bg-red-500/25 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {voting ? "Voting…" : "Vote AGAINST"}
        </button>
        <button
          onClick={() => onVote(proposal.id, 2)}
          disabled={voting || !connectedAddress}
          className="flex-1 rounded-md border border-yellow-500/40 bg-yellow-500/15 px-4 py-2 text-sm font-medium text-yellow-300 hover:bg-yellow-500/25 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {voting ? "Voting…" : "Abstain"}
        </button>
      </div>
    </div>
  );
}

interface DelegatePanelProps {
  connectedAddress: string | null;
  voteWeight: string | undefined;
  onDelegate: (delegateTo: string) => void;
  delegating: boolean;
}

function DelegatePanel({
  connectedAddress,
  voteWeight,
  onDelegate,
  delegating,
}: DelegatePanelProps) {
  const [delegateAddress, setDelegateAddress] = useState("");

  if (!connectedAddress) {
    return (
      <div className="text-center py-8 text-base-300">
        <p>Connect your wallet to delegate voting power.</p>
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-white/10 bg-white/[0.02] p-4 space-y-4 max-w-md">
      <h3 className="font-semibold text-base-100">Delegate Voting Power</h3>

      <div className="rounded-md border border-white/10 p-3">
        <div className="text-xs text-base-300">Current Vote Weight</div>
        <div className="font-mono text-brand-300">{voteWeight || "0"}</div>
      </div>

      <div className="space-y-2">
        <label className="block text-sm text-base-300">
          Delegate to Address
        </label>
        <input
          type="text"
          value={delegateAddress}
          onChange={(e) => setDelegateAddress(e.target.value)}
          placeholder="Enter Stellar address to delegate to"
          className="w-full rounded-md border border-white/10 bg-white/5 px-3 py-2 text-sm text-base-100 placeholder:text-base-300 focus:border-brand-500 focus:outline-none focus:ring-1 focus:ring-brand-500"
        />
        <button
          onClick={() => onDelegate(delegateAddress)}
          disabled={delegating || !delegateAddress}
          className="w-full rounded-md border border-brand-500/40 bg-brand-500/15 px-4 py-2 text-sm font-medium text-brand-300 hover:bg-brand-500/25 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {delegating ? "Delegating…" : "Delegate Votes"}
        </button>
      </div>

      <p className="text-xs text-base-300">
        Delegating transfers your voting power to another address. You can
        undelegate at any time by delegating to yourself.
      </p>
    </div>
  );
}

interface PastProposalRowProps {
  proposal: Proposal;
}

function PastProposalRow({ proposal }: PastProposalRowProps) {
  const approval = calculateApproval(proposal.votes_for, proposal.votes_against);

  return (
    <div className="rounded-lg border border-white/10 bg-white/[0.02] p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex-1 min-w-[200px]">
          <h4 className="font-medium text-base-100">{proposal.title}</h4>
          <p className="text-xs text-base-300 truncate">{proposal.description}</p>
        </div>
        <span
          className={`rounded-full px-2 py-0.5 text-xs font-medium ${STATUS_COLORS[proposal.status]}`}
        >
          {proposal.status.charAt(0).toUpperCase() + proposal.status.slice(1)}
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-4 mt-2 text-xs text-base-300">
        <span>For: <span className="font-mono text-green-400">{proposal.votes_for}</span></span>
        <span>Against: <span className="font-mono text-red-400">{proposal.votes_against}</span></span>
        <span>Approval: <span className="font-mono text-base-100">{approval.toFixed(1)}%</span></span>
        <span>Ended: <span className="font-mono text-base-100">{formatTimestamp(proposal.end_time)}</span></span>
      </div>
    </div>
  );
}

export default GovernancePortal;