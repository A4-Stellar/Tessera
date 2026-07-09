import type { ReactNode } from "react";

type Variant = "note" | "tip" | "warning" | "danger" | "compliance";

const VARIANTS: Record<Variant, { border: string; bg: string; icon: string; label: string; text: string }> = {
  note: { border: "border-sky-500/25", bg: "bg-sky-500/[0.06]", icon: "ℹ", label: "Note", text: "text-sky-300" },
  tip: { border: "border-brand-500/25", bg: "bg-brand-500/[0.06]", icon: "✓", label: "Tip", text: "text-brand-300" },
  warning: { border: "border-gold-500/25", bg: "bg-gold-500/[0.06]", icon: "!", label: "Warning", text: "text-gold-300" },
  danger: { border: "border-red-500/25", bg: "bg-red-500/[0.06]", icon: "✕", label: "Important", text: "text-red-300" },
  compliance: { border: "border-brand-500/30", bg: "bg-brand-500/[0.07]", icon: "⚖", label: "Compliance", text: "text-brand-300" },
};

interface CalloutBoxProps {
  variant?: Variant;
  title?: string;
  children: ReactNode;
}

/** Highlighted advisory box used throughout the docs. */
export function CalloutBox({ variant = "note", title, children }: CalloutBoxProps) {
  const v = VARIANTS[variant];
  return (
    <div className={`my-6 rounded-xl border ${v.border} ${v.bg} px-4 py-3.5`}>
      <div className={`mb-1 flex items-center gap-2 text-sm font-semibold ${v.text}`}>
        <span className="flex h-5 w-5 items-center justify-center rounded-full border border-current text-xs">
          {v.icon}
        </span>
        {title ?? v.label}
      </div>
      <div className="pl-7 text-sm leading-7 text-base-200/85 [&>*+*]:mt-2 [&_a]:text-brand-400 [&_code]:rounded [&_code]:bg-white/10 [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-[0.85em]">
        {children}
      </div>
    </div>
  );
}

export default CalloutBox;
