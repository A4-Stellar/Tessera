"use client";

import React, { useState, useEffect, useRef } from "react";
import {
  NotificationPreferences,
  getStoredPreferences,
  saveStoredPreferences,
  getNotificationPermissionStatus,
  subscribeToPushNotifications,
  unsubscribeFromPushNotifications,
  sendTestNotification,
  isPushNotificationSupported,
} from "@/lib/push-notifications";

interface NotificationModalProps {
  isOpen: boolean;
  onClose: () => void;
}

export function NotificationModal({ isOpen, onClose }: NotificationModalProps) {
  const [supported, setSupported] = useState(true);
  const [permission, setPermission] = useState<NotificationPermission>("default");
  const [preferences, setPreferences] = useState<NotificationPreferences>(getStoredPreferences);
  const [isSubscribed, setIsSubscribed] = useState(false);
  const [loading, setLoading] = useState(false);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const modalRef = useRef<HTMLDivElement>(null);
  const closeBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    setSupported(isPushNotificationSupported());
    setPermission(getNotificationPermissionStatus());
    setPreferences(getStoredPreferences());

    if (typeof window !== "undefined" && "serviceWorker" in navigator) {
      navigator.serviceWorker.ready.then((reg) => {
        reg.pushManager?.getSubscription().then((sub) => {
          setIsSubscribed(!!sub);
        });
      });
    }
  }, [isOpen]);

  // Trap focus and close on Escape
  useEffect(() => {
    if (!isOpen) return;

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        onClose();
        return;
      }

      if (e.key === "Tab" && modalRef.current) {
        const focusable = modalRef.current.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
        );
        if (focusable.length === 0) return;
        const first = focusable[0];
        const last = focusable[focusable.length - 1];

        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }

    document.addEventListener("keydown", handleKeyDown);
    closeBtnRef.current?.focus();
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const toggleCategory = (key: keyof NotificationPreferences) => {
    const updated = { ...preferences, [key]: !preferences[key] };
    setPreferences(updated);
    saveStoredPreferences(updated);
  };

  const handleSubscribe = async () => {
    setLoading(true);
    setStatusMessage(null);
    try {
      const sub = await subscribeToPushNotifications(undefined, preferences);
      if (sub) {
        setIsSubscribed(true);
        setPermission("granted");
        setStatusMessage("Successfully subscribed to real-time alerts!");
      }
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : "Failed to subscribe";
      setStatusMessage(msg);
      setPermission(getNotificationPermissionStatus());
    } finally {
      setLoading(false);
    }
  };

  const handleUnsubscribe = async () => {
    setLoading(true);
    setStatusMessage(null);
    try {
      const success = await unsubscribeFromPushNotifications();
      if (success) {
        setIsSubscribed(false);
        setStatusMessage("Push notifications disabled.");
      }
    } catch {
      setStatusMessage("Failed to unsubscribe.");
    } finally {
      setLoading(false);
    }
  };

  const handleTest = async (category: "dividends" | "compliance" | "system") => {
    try {
      await sendTestNotification(category);
      setStatusMessage(`Test ${category} notification sent! Check OS notification center.`);
    } catch {
      setStatusMessage("Failed to trigger test notification. Please enable permissions first.");
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="notification-modal-title"
      className="fixed inset-0 z-50 flex items-center justify-center p-4 sm:p-6"
    >
      {/* Backdrop */}
      <div
        className="fixed inset-0 bg-black/75 backdrop-blur-sm transition-opacity"
        onClick={onClose}
        aria-hidden="true"
      />

      {/* Modal Dialog */}
      <div
        ref={modalRef}
        className="relative w-full max-w-lg rounded-2xl border border-white/10 bg-base-900 p-6 shadow-2xl transition-all sm:p-8"
      >
        {/* Header */}
        <div className="flex items-start justify-between gap-4">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-brand-500/10 text-brand-400">
              <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" strokeLinecap="round" strokeLinejoin="round" />
                <path d="M13.73 21a2 2 0 0 1-3.46 0" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </div>
            <div>
              <h2 id="notification-modal-title" className="text-lg font-bold text-base-50">
                PWA Push Notifications
              </h2>
              <p className="text-xs text-base-400">
                Receive real-time alerts even when Tessera documentation is closed.
              </p>
            </div>
          </div>
          <button
            ref={closeBtnRef}
            type="button"
            onClick={onClose}
            aria-label="Close notification settings"
            className="rounded-lg p-2 text-base-400 hover:bg-white/5 hover:text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
          >
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M18 6 6 18M6 6l12 12" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        {/* Browser Support Check */}
        {!supported && (
          <div className="mt-4 rounded-xl border border-amber-500/20 bg-amber-500/10 p-3 text-xs text-amber-300">
            Push notifications are not supported in this browser or environment.
          </div>
        )}

        {/* Status Indicator */}
        <div className="mt-4 flex items-center justify-between rounded-xl border border-white/5 bg-base-950/60 px-4 py-3 text-xs">
          <span className="text-base-400">Browser Permission:</span>
          <span
            className={`inline-flex items-center gap-1.5 font-medium ${
              permission === "granted"
                ? "text-brand-400"
                : permission === "denied"
                ? "text-red-400"
                : "text-amber-400"
            }`}
          >
            <span
              className={`h-2 w-2 rounded-full ${
                permission === "granted"
                  ? "bg-brand-400"
                  : permission === "denied"
                  ? "bg-red-400"
                  : "bg-amber-400"
              }`}
            />
            {permission === "granted"
              ? isSubscribed
                ? "Subscribed & Active"
                : "Permission Granted"
              : permission === "denied"
              ? "Blocked by Browser"
              : "Not Configured"}
          </span>
        </div>

        {/* Customizable Alert Preferences */}
        <div className="mt-6 space-y-3">
          <label className="text-xs font-semibold tracking-wider uppercase text-base-400">
            Customizable Alert Preferences
          </label>

          {/* Dividends */}
          <div
            onClick={() => toggleCategory("dividends")}
            className="flex cursor-pointer items-start justify-between rounded-xl border border-white/5 bg-base-950/40 p-3.5 transition-colors hover:border-brand-500/30 hover:bg-base-950/80"
          >
            <div className="space-y-0.5 pr-4">
              <div className="flex items-center gap-2">
                <span className="text-sm font-semibold text-base-100">Dividend Payouts</span>
                <span className="rounded bg-brand-500/20 px-1.5 py-0.5 text-[10px] font-medium text-brand-300">
                  USDC / XLM
                </span>
              </div>
              <p className="text-xs text-base-400">
                Instant alerts for dividend declarations, automated AMM DRIP reinvestment, and claim availability.
              </p>
            </div>
            <input
              type="checkbox"
              checked={preferences.dividends}
              onChange={() => {}}
              aria-label="Toggle Dividend Payout notifications"
              className="mt-1 h-4 w-4 rounded border-white/20 bg-base-800 text-brand-500 focus:ring-brand-400 focus:ring-offset-base-950"
            />
          </div>

          {/* Compliance */}
          <div
            onClick={() => toggleCategory("compliance")}
            className="flex cursor-pointer items-start justify-between rounded-xl border border-white/5 bg-base-950/40 p-3.5 transition-colors hover:border-brand-500/30 hover:bg-base-950/80"
          >
            <div className="space-y-0.5 pr-4">
              <div className="flex items-center gap-2">
                <span className="text-sm font-semibold text-base-100">Compliance & KYC Updates</span>
                <span className="rounded bg-indigo-500/20 px-1.5 py-0.5 text-[10px] font-medium text-indigo-300">
                  On-Chain Gate
                </span>
              </div>
              <p className="text-xs text-base-400">
                Notifications on KYC approval status, accreditation renewals, and jurisdiction transfer gate updates.
              </p>
            </div>
            <input
              type="checkbox"
              checked={preferences.compliance}
              onChange={() => {}}
              aria-label="Toggle Compliance notifications"
              className="mt-1 h-4 w-4 rounded border-white/20 bg-base-800 text-brand-500 focus:ring-brand-400 focus:ring-offset-base-950"
            />
          </div>

          {/* System Upgrades */}
          <div
            onClick={() => toggleCategory("system")}
            className="flex cursor-pointer items-start justify-between rounded-xl border border-white/5 bg-base-950/40 p-3.5 transition-colors hover:border-brand-500/30 hover:bg-base-950/80"
          >
            <div className="space-y-0.5 pr-4">
              <div className="flex items-center gap-2">
                <span className="text-sm font-semibold text-base-100">System & Governance Upgrades</span>
                <span className="rounded bg-amber-500/20 px-1.5 py-0.5 text-[10px] font-medium text-amber-300">
                  Protocol
                </span>
              </div>
              <p className="text-xs text-base-400">
                Notices for Soroban contract upgrades, timelock resolutions, and emergency circuit breaker events.
              </p>
            </div>
            <input
              type="checkbox"
              checked={preferences.system}
              onChange={() => {}}
              aria-label="Toggle System Upgrade notifications"
              className="mt-1 h-4 w-4 rounded border-white/20 bg-base-800 text-brand-500 focus:ring-brand-400 focus:ring-offset-base-950"
            />
          </div>
        </div>

        {/* Feedback Message */}
        {statusMessage && (
          <div className="mt-4 rounded-xl border border-brand-500/20 bg-brand-500/10 px-3.5 py-2.5 text-xs text-brand-300">
            {statusMessage}
          </div>
        )}

        {/* Primary Controls */}
        <div className="mt-6 flex flex-col gap-2.5 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={loading || !supported}
              onClick={isSubscribed ? handleUnsubscribe : handleSubscribe}
              className={`rounded-xl px-4 py-2 text-xs font-semibold shadow-lg transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 ${
                isSubscribed
                  ? "border border-red-500/30 bg-red-500/10 text-red-300 hover:bg-red-500/20"
                  : "bg-gradient-to-r from-brand-500 to-brand-600 text-base-950 hover:from-brand-400 hover:to-brand-500"
              }`}
            >
              {loading ? "Processing..." : isSubscribed ? "Disable Notifications" : "Enable Push Notifications"}
            </button>
          </div>

          {/* Test Dispatchers */}
          <div className="flex items-center gap-1.5 text-xs">
            <span className="text-[11px] text-base-400">Test:</span>
            <button
              type="button"
              onClick={() => handleTest("dividends")}
              className="rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-[11px] text-base-300 hover:bg-white/10 hover:text-base-100"
            >
              Dividend
            </button>
            <button
              type="button"
              onClick={() => handleTest("compliance")}
              className="rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-[11px] text-base-300 hover:bg-white/10 hover:text-base-100"
            >
              Compliance
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

export default NotificationModal;
