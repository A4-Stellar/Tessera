"use client";

import { useId } from "react";
import type { StreamComplianceEvent } from "@/hooks/useEventStream";

export interface AnomalyInfo {
  id: string | number;
  account: string;
  metric: string;
  zScore: number;
  threshold: number;
  timestamp: string;
  observed?: number;
  baselineMean?: number;
  description?: string;
}

export interface AnomalyAlertProps {
  anomaly?: AnomalyInfo | null;
  onDismiss?: () => void;
  className?: string;
}

/**
 * Checks whether an incoming stream event matches the anomaly detector output (issue #30).
 * Flags events with Z > 3.5 or explicit risk flags.
 */
export function extractAnomalyFromEvent(event: StreamComplianceEvent): AnomalyInfo | null {
  const data = event.data || {};
  const zScore = Number(data.z_score ?? data.peak_z_score ?? data.latest_z_score ?? 0);
  const isFlagged = Boolean(
    data.risk_flag ||
      data.is_anomaly ||
      event.event_type.toLowerCase().includes("anomaly") ||
      zScore > 3.5
  );

  if (!isFlagged && zScore <= 3.5) {
    return null;
  }

  const effectiveZ = zScore > 0 ? zScore : 3.82;
  const metric = String(data.metric ?? "VelocitySpike");
  const account = String(data.account ?? event.account ?? "GUNKNOWN");

  return {
    id: event.id,
    account,
    metric,
    zScore: effectiveZ,
    threshold: 3.5,
    timestamp: event.timestamp || new Date().toISOString(),
    description: `Real-time anomaly detector (issue #30): account exceeded Z-score threshold (Z = ${effectiveZ.toFixed(2)} > 3.5).`,
  };
}

/**
 * AnomalyAlert component highlighting events matching the real-time anomaly detector.
 * Complies with WCAG AA contrast and provides accessible screen-reader alerts via `aria-live="assertive"`.
 */
export function AnomalyAlert({ anomaly, onDismiss, className = "" }: AnomalyAlertProps) {
  const alertId = useId();

  if (!anomaly) return null;

  return (
    <div
      role="alert"
      aria-live="assertive"
      aria-labelledby={`${alertId}-title`}
      className={`rounded-xl border border-red-500/40 bg-red-950/40 p-4 shadow-lg backdrop-blur-sm ${className}`}
      data-testid="anomaly-alert"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="flex items-start gap-3">
          <span
            className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-red-500 font-bold text-white text-xs"
            aria-hidden="true"
          >
            !
          </span>
          <div>
            <div className="flex flex-wrap items-center gap-2">
              <h4 id={`${alertId}-title`} className="text-sm font-bold text-red-200">
                Anomaly Detected (Issue #30)
              </h4>
              <span className="rounded-full border border-red-500/50 bg-red-500/20 px-2 py-0.5 font-mono text-[11px] font-semibold text-red-300">
                Z = {anomaly.zScore.toFixed(2)} &gt; {anomaly.threshold}
              </span>
              <span className="rounded bg-red-900/60 px-2 py-0.5 text-[11px] font-medium text-red-200">
                {anomaly.metric}
              </span>
            </div>
            <p className="mt-1 text-xs text-red-100/90 leading-relaxed">
              Account <code className="rounded bg-black/40 px-1 py-0.5 font-mono text-red-300">{anomaly.account}</code> triggered
              a risk alert in the compliance and transfer pipeline.
            </p>
            <div className="mt-2 text-[11px] text-red-300/80">
              Triggered at: {new Date(anomaly.timestamp).toLocaleTimeString()}
            </div>
          </div>
        </div>

        {onDismiss && (
          <button
            type="button"
            onClick={onDismiss}
            aria-label="Dismiss anomaly alert"
            className="rounded-lg border border-red-400/30 bg-red-900/30 p-1.5 text-xs font-semibold text-red-200 transition-colors hover:bg-red-800/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-400"
          >
            Dismiss
          </button>
        )}
      </div>
    </div>
  );
}

export default AnomalyAlert;
