"use client";

import { useState, useEffect, useRef } from "react";
import clsx from "clsx";

interface SubscribeModalProps {
  isOpen: boolean;
  onClose: () => void;
}

type SubscriptionType = "email" | "webhook";

const SERVICE_OPTIONS = [
  { id: "tessera-api", label: "Tessera REST API" },
  { id: "soroban-rpc", label: "Soroban Testnet RPC" },
  { id: "tessera-docs", label: "Tessera Documentation" },
];

/**
 * Modal for subscribing to incident notifications via email or webhook.
 *
 * In production, submitting the form would POST to `/api/subscribe`
 * which would persist the subscription and send a confirmation.
 */
export function SubscribeModal({ isOpen, onClose }: SubscribeModalProps) {
  const [type, setType] = useState<SubscriptionType>("email");
  const [email, setEmail] = useState("");
  const [webhookUrl, setWebhookUrl] = useState("");
  const [selectedServices, setSelectedServices] = useState<string[]>(
    SERVICE_OPTIONS.map((s) => s.id)
  );
  const [submitting, setSubmitting] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const modalRef = useRef<HTMLDivElement>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);

  // Trap focus inside modal when open
  useEffect(() => {
    if (isOpen) {
      closeButtonRef.current?.focus();
    }
  }, [isOpen]);

  // Close on Escape key
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape" && isOpen) onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const toggleService = (id: string) => {
    setSelectedServices((prev) =>
      prev.includes(id) ? prev.filter((s) => s !== id) : [...prev, id]
    );
  };

  const validate = (): string | null => {
    if (type === "email") {
      if (!email.trim()) return "Email address is required.";
      if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email))
        return "Please enter a valid email address.";
    } else {
      if (!webhookUrl.trim()) return "Webhook URL is required.";
      try {
        new URL(webhookUrl);
      } catch {
        return "Please enter a valid URL.";
      }
      if (!webhookUrl.startsWith("https://"))
        return "Webhook URL must use HTTPS.";
    }
    if (selectedServices.length === 0)
      return "Select at least one service to monitor.";
    return null;
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);

    const validationError = validate();
    if (validationError) {
      setError(validationError);
      return;
    }

    setSubmitting(true);

    try {
      // In production this would POST to /api/subscribe
      // For the prototype we simulate a 800ms network delay
      await new Promise((res) => setTimeout(res, 800));
      setSubmitted(true);
    } catch {
      setError("Failed to subscribe. Please try again.");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    /* Backdrop */
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/60 backdrop-blur-sm"
      role="dialog"
      aria-modal="true"
      aria-labelledby="subscribe-title"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={modalRef}
        className="relative w-full max-w-md bg-gray-900 rounded-2xl border border-gray-700 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-5 border-b border-gray-800">
          <h2 id="subscribe-title" className="text-lg font-semibold text-gray-100">
            Subscribe to Status Updates
          </h2>
          <button
            ref={closeButtonRef}
            onClick={onClose}
            className="rounded-lg p-1.5 text-gray-400 hover:text-gray-200 hover:bg-gray-800 transition-colors"
            aria-label="Close subscribe modal"
          >
            <svg className="h-5 w-5" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
              <path d="M6.28 5.22a.75.75 0 00-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 101.06 1.06L10 11.06l3.72 3.72a.75.75 0 101.06-1.06L11.06 10l3.72-3.72a.75.75 0 00-1.06-1.06L10 8.94 6.28 5.22z" />
            </svg>
          </button>
        </div>

        {submitted ? (
          /* Success state */
          <div className="flex flex-col items-center gap-4 px-6 py-10 text-center">
            <div className="h-14 w-14 rounded-full bg-green-900/50 border border-green-700 flex items-center justify-center">
              <svg className="h-7 w-7 text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
              </svg>
            </div>
            <h3 className="text-base font-semibold text-gray-100">You&apos;re subscribed!</h3>
            <p className="text-sm text-gray-400">
              {type === "email"
                ? `We'll send incident notifications to ${email}.`
                : "Incident payloads will be sent to your webhook URL."}
            </p>
            <button
              onClick={onClose}
              className="mt-2 rounded-lg px-4 py-2 bg-brand-700 hover:bg-brand-600 text-white text-sm font-medium transition-colors"
            >
              Done
            </button>
          </div>
        ) : (
          <form onSubmit={handleSubmit} noValidate className="px-6 py-5 flex flex-col gap-5">
            {/* Subscription type toggle */}
            <div className="flex rounded-lg bg-gray-800 p-1 gap-1">
              {(["email", "webhook"] as SubscriptionType[]).map((t) => (
                <button
                  key={t}
                  type="button"
                  onClick={() => setType(t)}
                  className={clsx(
                    "flex-1 rounded-md py-1.5 text-sm font-medium transition-colors capitalize",
                    type === t
                      ? "bg-gray-700 text-gray-100 shadow-sm"
                      : "text-gray-400 hover:text-gray-200"
                  )}
                >
                  {t === "email" ? "📧 Email" : "🔗 Webhook"}
                </button>
              ))}
            </div>

            {/* Email or webhook URL input */}
            {type === "email" ? (
              <div className="flex flex-col gap-1.5">
                <label htmlFor="email-input" className="text-sm font-medium text-gray-300">
                  Email address
                </label>
                <input
                  id="email-input"
                  type="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="you@example.com"
                  autoComplete="email"
                  className="rounded-lg bg-gray-800 border border-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-600 focus:outline-none focus:ring-2 focus:ring-brand-500"
                />
              </div>
            ) : (
              <div className="flex flex-col gap-1.5">
                <label htmlFor="webhook-input" className="text-sm font-medium text-gray-300">
                  Webhook URL
                </label>
                <input
                  id="webhook-input"
                  type="url"
                  value={webhookUrl}
                  onChange={(e) => setWebhookUrl(e.target.value)}
                  placeholder="https://hooks.example.com/…"
                  className="rounded-lg bg-gray-800 border border-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-600 focus:outline-none focus:ring-2 focus:ring-brand-500"
                />
                <p className="text-xs text-gray-500">
                  We&apos;ll POST a JSON payload on incident create/update events.
                </p>
              </div>
            )}

            {/* Service selection */}
            <fieldset>
              <legend className="text-sm font-medium text-gray-300 mb-2">
                Notify me about
              </legend>
              <div className="flex flex-col gap-2">
                {SERVICE_OPTIONS.map((svc) => (
                  <label
                    key={svc.id}
                    className="flex items-center gap-3 cursor-pointer group"
                  >
                    <input
                      type="checkbox"
                      checked={selectedServices.includes(svc.id)}
                      onChange={() => toggleService(svc.id)}
                      className="h-4 w-4 rounded border-gray-600 bg-gray-800 text-brand-500 focus:ring-brand-500"
                    />
                    <span className="text-sm text-gray-300 group-hover:text-gray-100 transition-colors">
                      {svc.label}
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>

            {/* Error message */}
            {error && (
              <p role="alert" className="text-sm text-red-400 bg-red-900/30 rounded-lg px-3 py-2">
                {error}
              </p>
            )}

            {/* Submit */}
            <button
              type="submit"
              disabled={submitting}
              className="w-full rounded-lg px-4 py-2.5 bg-brand-600 hover:bg-brand-500 disabled:opacity-60 disabled:cursor-not-allowed text-white text-sm font-semibold transition-colors"
            >
              {submitting ? "Subscribing…" : "Subscribe"}
            </button>

            <p className="text-xs text-gray-600 text-center">
              You can unsubscribe at any time from the confirmation email.
            </p>
          </form>
        )}
      </div>
    </div>
  );
}
