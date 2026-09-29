"use client";

import React, { useState, useMemo, useCallback } from "react";

export type Permission = "ALLOWED" | "ACCREDITED_ONLY" | "BLOCKED";

export type Region =
  | "All"
  | "North America"
  | "Europe/EEA"
  | "Asia-Pacific"
  | "Middle East"
  | "Latin America"
  | "Africa"
  | "High Risk";

export interface Jurisdiction {
  code: string;
  name: string;
  region: Exclude<Region, "All">;
  flag: string;
  isHighRisk?: boolean;
}

export const JURISDICTIONS: Jurisdiction[] = [
  // North America
  { code: "US", name: "United States", region: "North America", flag: "🇺🇸" },
  { code: "CA", name: "Canada", region: "North America", flag: "🇨🇦" },
  { code: "MX", name: "Mexico", region: "North America", flag: "🇲🇽" },

  // Europe / EEA / UK / CH
  { code: "GB", name: "United Kingdom", region: "Europe/EEA", flag: "🇬🇧" },
  { code: "DE", name: "Germany", region: "Europe/EEA", flag: "🇩🇪" },
  { code: "FR", name: "France", region: "Europe/EEA", flag: "🇫🇷" },
  { code: "CH", name: "Switzerland", region: "Europe/EEA", flag: "🇨🇭" },
  { code: "NL", name: "Netherlands", region: "Europe/EEA", flag: "🇳🇱" },
  { code: "IE", name: "Ireland", region: "Europe/EEA", flag: "🇮🇪" },
  { code: "LU", name: "Luxembourg", region: "Europe/EEA", flag: "🇱🇺" },
  { code: "IT", name: "Italy", region: "Europe/EEA", flag: "🇮🇹" },
  { code: "ES", name: "Spain", region: "Europe/EEA", flag: "🇪🇸" },
  { code: "SE", name: "Sweden", region: "Europe/EEA", flag: "🇸🇪" },
  { code: "NO", name: "Norway", region: "Europe/EEA", flag: "🇳🇴" },
  { code: "DK", name: "Denmark", region: "Europe/EEA", flag: "🇩🇰" },
  { code: "FI", name: "Finland", region: "Europe/EEA", flag: "🇫🇮" },
  { code: "BE", name: "Belgium", region: "Europe/EEA", flag: "🇧🇪" },
  { code: "AT", name: "Austria", region: "Europe/EEA", flag: "🇦🇹" },
  { code: "PT", name: "Portugal", region: "Europe/EEA", flag: "🇵🇹" },
  { code: "PL", name: "Poland", region: "Europe/EEA", flag: "🇵🇱" },
  { code: "CZ", name: "Czech Republic", region: "Europe/EEA", flag: "🇨🇿" },
  { code: "GR", name: "Greece", region: "Europe/EEA", flag: "🇬🇷" },
  { code: "CY", name: "Cyprus", region: "Europe/EEA", flag: "🇨🇾" },
  { code: "MT", name: "Malta", region: "Europe/EEA", flag: "🇲🇹" },
  { code: "EE", name: "Estonia", region: "Europe/EEA", flag: "🇪🇪" },
  { code: "LV", name: "Latvia", region: "Europe/EEA", flag: "🇱🇻" },
  { code: "LT", name: "Lithuania", region: "Europe/EEA", flag: "🇱🇹" },

  // Asia-Pacific
  { code: "SG", name: "Singapore", region: "Asia-Pacific", flag: "🇸🇬" },
  { code: "JP", name: "Japan", region: "Asia-Pacific", flag: "🇯🇵" },
  { code: "HK", name: "Hong Kong", region: "Asia-Pacific", flag: "🇭🇰" },
  { code: "AU", name: "Australia", region: "Asia-Pacific", flag: "🇦🇺" },
  { code: "NZ", name: "New Zealand", region: "Asia-Pacific", flag: "🇳🇿" },
  { code: "KR", name: "South Korea", region: "Asia-Pacific", flag: "🇰🇷" },
  { code: "IN", name: "India", region: "Asia-Pacific", flag: "🇮🇳" },
  { code: "TW", name: "Taiwan", region: "Asia-Pacific", flag: "🇹🇼" },
  { code: "MY", name: "Malaysia", region: "Asia-Pacific", flag: "🇲🇾" },
  { code: "ID", name: "Indonesia", region: "Asia-Pacific", flag: "🇮🇩" },
  { code: "TH", name: "Thailand", region: "Asia-Pacific", flag: "🇹🇭" },
  { code: "PH", name: "Philippines", region: "Asia-Pacific", flag: "🇵🇭" },
  { code: "VN", name: "Vietnam", region: "Asia-Pacific", flag: "🇻🇳" },

  // Middle East
  { code: "AE", name: "United Arab Emirates", region: "Middle East", flag: "🇦🇪" },
  { code: "SA", name: "Saudi Arabia", region: "Middle East", flag: "🇸🇦" },
  { code: "QA", name: "Qatar", region: "Middle East", flag: "🇶🇦" },
  { code: "IL", name: "Israel", region: "Middle East", flag: "🇮🇱" },
  { code: "BH", name: "Bahrain", region: "Middle East", flag: "🇧🇭" },
  { code: "KW", name: "Kuwait", region: "Middle East", flag: "🇰🇼" },

  // Latin America
  { code: "BR", name: "Brazil", region: "Latin America", flag: "🇧🇷" },
  { code: "AR", name: "Argentina", region: "Latin America", flag: "🇦🇷" },
  { code: "CL", name: "Chile", region: "Latin America", flag: "🇨🇱" },
  { code: "CO", name: "Colombia", region: "Latin America", flag: "🇨🇴" },
  { code: "PE", name: "Peru", region: "Latin America", flag: "🇵🇪" },
  { code: "PA", name: "Panama", region: "Latin America", flag: "🇵🇦" },

  // Africa
  { code: "NG", name: "Nigeria", region: "Africa", flag: "🇳🇬" },
  { code: "ZA", name: "South Africa", region: "Africa", flag: "🇿🇦" },
  { code: "KE", name: "Kenya", region: "Africa", flag: "🇰🇪" },
  { code: "EG", name: "Egypt", region: "Africa", flag: "🇪🇬" },
  { code: "GH", name: "Ghana", region: "Africa", flag: "🇬🇭" },
  { code: "MU", name: "Mauritius", region: "Africa", flag: "🇲🇺" },

  // FATF Sanctions / High Risk
  { code: "IR", name: "Iran", region: "High Risk", flag: "🇮🇷", isHighRisk: true },
  { code: "KP", name: "North Korea", region: "High Risk", flag: "🇰🇵", isHighRisk: true },
  { code: "SY", name: "Syria", region: "High Risk", flag: "🇸🇾", isHighRisk: true },
  { code: "CU", name: "Cuba", region: "High Risk", flag: "🇨🇺", isHighRisk: true },
  { code: "RU", name: "Russia", region: "High Risk", flag: "🇷🇺", isHighRisk: true },
];

export type PresetKey =
  | "US_SEC_REG_D_S"
  | "EU_MICA"
  | "SINGAPORE_MAS"
  | "GLOBAL_ACCREDITED"
  | "PERMISSIVE_SANDBOX";

interface PresetConfig {
  name: string;
  description: string;
  compute: (source: Jurisdiction, dest: Jurisdiction) => Permission;
}

export const PRESETS: Record<PresetKey, PresetConfig> = {
  US_SEC_REG_D_S: {
    name: "US SEC Reg D / Reg S",
    description:
      "Strict Rule 506(c) accredited investor requirement for US persons; Regulation S cross-border exemption for non-US offshore offerings; FATF high-risk jurisdictions blocked.",
    compute: (source, dest) => {
      if (source.isHighRisk || dest.isHighRisk) return "BLOCKED";
      if (dest.code === "US") return "ACCREDITED_ONLY";
      if (source.code === "US") {
        return dest.code === "US" ? "ACCREDITED_ONLY" : "ALLOWED";
      }
      return "ALLOWED";
    },
  },
  EU_MICA: {
    name: "EU MiCA Passporting",
    description:
      "Harmonized cross-border passporting across all EU/EEA member states; third countries require qualified/accredited classification; high-risk AML jurisdictions blocked.",
    compute: (source, dest) => {
      if (source.isHighRisk || dest.isHighRisk) return "BLOCKED";
      const isEuSource = source.region === "Europe/EEA";
      const isEuDest = dest.region === "Europe/EEA";
      if (isEuSource && isEuDest) return "ALLOWED";
      return "ACCREDITED_ONLY";
    },
  },
  SINGAPORE_MAS: {
    name: "Singapore MAS Framework",
    description:
      "Capital Markets Services (CMS) licensed corridor with accredited investor exemptions under Singapore SFA; regional institutional bridges.",
    compute: (source, dest) => {
      if (source.isHighRisk || dest.isHighRisk) return "BLOCKED";
      if (dest.code === "SG" && source.code === "SG") return "ALLOWED";
      const tier1 = ["SG", "HK", "JP", "AU", "GB", "US", "CH"];
      if (tier1.includes(dest.code)) return "ACCREDITED_ONLY";
      return "ALLOWED";
    },
  },
  GLOBAL_ACCREDITED: {
    name: "Global Institutional",
    description:
      "Strict accredited-only access globally across all non-sanctioned jurisdictions; zero retail permissibility.",
    compute: (source, dest) => {
      if (source.isHighRisk || dest.isHighRisk) return "BLOCKED";
      return "ACCREDITED_ONLY";
    },
  },
  PERMISSIVE_SANDBOX: {
    name: "Permissive Sandbox",
    description:
      "Open access across all standard jurisdictions for testnet deployments and developer experimentation.",
    compute: (source, dest) => {
      if (source.isHighRisk || dest.isHighRisk) return "BLOCKED";
      return "ALLOWED";
    },
  },
};

const REGIONS: Region[] = [
  "All",
  "North America",
  "Europe/EEA",
  "Asia-Pacific",
  "Middle East",
  "Latin America",
  "Africa",
  "High Risk",
];

const PERMISSION_CONFIG: Record<
  Permission,
  { label: string; bg: string; text: string; border: string; icon: string }
> = {
  ALLOWED: {
    label: "Allowed",
    bg: "bg-emerald-500/20 hover:bg-emerald-500/30",
    text: "text-emerald-300",
    border: "border-emerald-500/40",
    icon: "✓",
  },
  ACCREDITED_ONLY: {
    label: "Accredited Only",
    bg: "bg-amber-500/20 hover:bg-amber-500/30",
    text: "text-amber-300",
    border: "border-amber-500/40",
    icon: "★",
  },
  BLOCKED: {
    label: "Blocked",
    bg: "bg-rose-500/20 hover:bg-rose-500/30",
    text: "text-rose-300",
    border: "border-rose-500/40",
    icon: "✕",
  },
};

export default function ComplianceMatrix() {
  const [activePreset, setActivePreset] = useState<PresetKey>("US_SEC_REG_D_S");
  const [selectedRegion, setSelectedRegion] = useState<Region>("All");
  const [searchQuery, setSearchQuery] = useState("");
  const [pageSize, setPageSize] = useState<number>(12);
  const [currentPage, setCurrentPage] = useState<number>(1);
  const [contractAdmin, setContractAdmin] = useState(
    "GDQOE23CFSUMSVQK4Y5JHPPMX73TKMGVUVCKEX6TV7WDZ5HQOFZ6AC5Z"
  );
  const [exportFormat, setExportFormat] = useState<"cli" | "rust" | "json">("cli");
  const [copied, setCopied] = useState(false);

  // Sparse map of user cell overrides: key = `${source}-${dest}`
  const [overrides, setOverrides] = useState<Record<string, Permission>>({});

  // Filtered jurisdictions based on region and search query
  const filteredJurisdictions = useMemo(() => {
    return JURISDICTIONS.filter((j) => {
      const matchesRegion = selectedRegion === "All" || j.region === selectedRegion;
      const q = searchQuery.trim().toLowerCase();
      const matchesSearch =
        q === "" ||
        j.code.toLowerCase().includes(q) ||
        j.name.toLowerCase().includes(q);
      return matchesRegion && matchesSearch;
    });
  }, [selectedRegion, searchQuery]);

  // Paginated view for table responsiveness
  const totalPages = Math.ceil(filteredJurisdictions.length / pageSize) || 1;
  const paginatedJurisdictions = useMemo(() => {
    const start = (currentPage - 1) * pageSize;
    return filteredJurisdictions.slice(start, start + pageSize);
  }, [filteredJurisdictions, currentPage, pageSize]);

  // Helper to resolve permission for a pair in O(1)
  const getPermission = useCallback(
    (source: Jurisdiction, dest: Jurisdiction): Permission => {
      const key = `${source.code}-${dest.code}`;
      if (overrides[key]) {
        return overrides[key];
      }
      return PRESETS[activePreset].compute(source, dest);
    },
    [overrides, activePreset]
  );

  // Cell toggle: Allowed -> Accredited Only -> Blocked -> Allowed
  const toggleCell = (source: Jurisdiction, dest: Jurisdiction) => {
    const key = `${source.code}-${dest.code}`;
    const current = getPermission(source, dest);
    const next: Permission =
      current === "ALLOWED"
        ? "ACCREDITED_ONLY"
        : current === "ACCREDITED_ONLY"
        ? "BLOCKED"
        : "ALLOWED";

    setOverrides((prev) => ({
      ...prev,
      [key]: next,
    }));
  };

  // Bulk actions
  const applyPreset = (presetKey: PresetKey) => {
    setActivePreset(presetKey);
    setOverrides({});
  };

  const setRowPermission = (source: Jurisdiction, permission: Permission) => {
    setOverrides((prev) => {
      const next = { ...prev };
      for (const dest of filteredJurisdictions) {
        next[`${source.code}-${dest.code}`] = permission;
      }
      return next;
    });
  };

  const setAllVisible = (permission: Permission) => {
    setOverrides((prev) => {
      const next = { ...prev };
      for (const src of filteredJurisdictions) {
        for (const dst of filteredJurisdictions) {
          next[`${src.code}-${dst.code}`] = permission;
        }
      }
      return next;
    });
  };

  // Matrix stats summary
  const stats = useMemo(() => {
    let allowed = 0;
    let accredited = 0;
    let blocked = 0;
    const total = filteredJurisdictions.length * filteredJurisdictions.length;

    for (const src of filteredJurisdictions) {
      for (const dst of filteredJurisdictions) {
        const p = getPermission(src, dst);
        if (p === "ALLOWED") allowed++;
        else if (p === "ACCREDITED_ONLY") accredited++;
        else if (p === "BLOCKED") blocked++;
      }
    }

    return { allowed, accredited, blocked, total };
  }, [filteredJurisdictions, getPermission]);

  // Generate Soroban arguments
  const sorobanArgs = useMemo(() => {
    const blockedList: string[] = [];
    const rulesList: Array<{ from: string; to: string; permission: string }> = [];

    // Collect globally blocked jurisdictions
    for (const j of JURISDICTIONS) {
      if (j.isHighRisk) {
        blockedList.push(j.code);
      }
    }

    // Collect rules between configured jurisdictions
    for (const src of filteredJurisdictions) {
      for (const dst of filteredJurisdictions) {
        const p = getPermission(src, dst);
        if (p === "BLOCKED" && !blockedList.includes(dst.code)) {
          // Rule-level block
        }
        rulesList.push({
          from: src.code,
          to: dst.code,
          permission: p,
        });
      }
    }

    return { blockedList, rulesList };
  }, [filteredJurisdictions, getPermission]);

  // Code generation outputs
  const generatedCode = useMemo(() => {
    const { blockedList, rulesList } = sorobanArgs;

    if (exportFormat === "cli") {
      const blockedJson = JSON.stringify(blockedList);
      const sampleRules = rulesList
        .filter((r) => r.permission !== "ALLOWED")
        .slice(0, 20);
      const rulesJson = JSON.stringify(sampleRules);

      return `# Step 1: Initialize compliance contract with admin key
soroban contract invoke \\
  --id CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU \\
  --source "${contractAdmin}" \\
  --network testnet \\
  -- initialize \\
  --admin "${contractAdmin}"

# Step 2: Configure blocked FATF / OFAC jurisdictions (${blockedList.length} countries)
soroban contract invoke \\
  --id CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU \\
  --source "${contractAdmin}" \\
  --network testnet \\
  -- set_blocked_jurisdictions \\
  --blocked '${blockedJson}'

# Step 3: Set cross-border investor permissibility corridors (${rulesList.length} evaluated)
soroban contract invoke \\
  --id CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU \\
  --source "${contractAdmin}" \\
  --network testnet \\
  -- configure_jurisdiction_rules \\
  --rules '${rulesJson}'`;
    }

    if (exportFormat === "rust") {
      const sampleRules = rulesList
        .filter((r) => r.permission !== "ALLOWED")
        .slice(0, 15);

      return `// Soroban Rust contract initialization snippet
use soroban_sdk::{vec, Env, String, Vec};

pub fn setup_compliance_rules(env: &Env, admin: &Address) {
    // 1. Blocked jurisdictions (${blockedList.length} entries)
    let mut blocked = Vec::<String>::new(env);
${blockedList.map((c) => `    blocked.push_back(String::from_str(env, "${c}"));`).join("\n")}

    // 2. Cross-border permission corridor rules
    // Permission types: Allowed = 0, AccreditedOnly = 1, Blocked = 2
    let mut rules = Vec::<(String, String, u32)>::new(env);
${sampleRules
  .map(
    (r) =>
      `    rules.push_back((String::from_str(env, "${r.from}"), String::from_str(env, "${r.to}"), ${
        r.permission === "ALLOWED" ? 0 : r.permission === "ACCREDITED_ONLY" ? 1 : 2
      }));`
  )
  .join("\n")}

    env.storage().instance().set(&DataKey::BlockedJurisdictions, &blocked);
}`;
    }

    // JSON RPC export
    return JSON.stringify(
      {
        admin: contractAdmin,
        preset: activePreset,
        jurisdictions_evaluated: filteredJurisdictions.length,
        blocked_jurisdictions: blockedList,
        rules: rulesList,
        metadata: {
          generator: "Tessera RWA Compliance Matrix Generator",
          generated_at: new Date().toISOString(),
        },
      },
      null,
      2
    );
  }, [exportFormat, sorobanArgs, contractAdmin, activePreset, filteredJurisdictions]);

  const copyCode = async () => {
    try {
      await navigator.clipboard.writeText(generatedCode);
      setCopied(true);
      setTimeout(() => setCopied(false), 2500);
    } catch {
      // Fallback
    }
  };

  return (
    <div className="my-8 rounded-2xl border border-white/10 bg-base-900/80 p-6 shadow-2xl backdrop-blur-xl">
      {/* Header */}
      <div className="mb-6 flex flex-wrap items-start justify-between gap-4 border-b border-white/10 pb-5">
        <div>
          <div className="inline-flex items-center gap-2 rounded-full border border-brand-400/30 bg-brand-500/10 px-3 py-1 text-xs font-semibold uppercase tracking-wider text-brand-300">
            <span>⚖️ Cross-Border Regulatory Engine</span>
          </div>
          <h2 className="mt-2 text-2xl font-bold tracking-tight text-base-100 sm:text-3xl">
            Interactive RWA Compliance Permissibility Matrix
          </h2>
          <p className="mt-1 max-w-3xl text-sm text-base-300">
            Visually configure cross-border investor permissions between 50+ international
            jurisdictions. Select industry presets or manually adjust corridors, then export
            production Soroban contract arguments.
          </p>
        </div>

        {/* Legend */}
        <div className="flex flex-wrap items-center gap-2 rounded-xl border border-white/10 bg-base-950/60 p-3 text-xs">
          <span className="font-semibold text-base-200">Corridor Legend:</span>
          {(["ALLOWED", "ACCREDITED_ONLY", "BLOCKED"] as Permission[]).map((p) => {
            const conf = PERMISSION_CONFIG[p];
            return (
              <span
                key={p}
                className={`inline-flex items-center gap-1.5 rounded-md border px-2 py-1 font-medium ${conf.bg} ${conf.text} ${conf.border}`}
              >
                <span>{conf.icon}</span>
                <span>{conf.label}</span>
              </span>
            );
          })}
        </div>
      </div>

      {/* Preset Selector */}
      <div className="mb-6 rounded-xl border border-white/10 bg-base-950/40 p-4">
        <label className="text-xs font-bold uppercase tracking-wider text-base-300">
          Preset Regulatory Framework:
        </label>
        <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
          {(Object.keys(PRESETS) as PresetKey[]).map((key) => {
            const preset = PRESETS[key];
            const isSelected = activePreset === key;
            return (
              <button
                key={key}
                type="button"
                onClick={() => applyPreset(key)}
                className={`flex flex-col rounded-lg border p-3 text-left transition-all ${
                  isSelected
                    ? "border-brand-400/80 bg-brand-500/15 text-base-50 shadow-md ring-1 ring-brand-400/40"
                    : "border-white/10 bg-white/[0.02] text-base-300 hover:border-white/20 hover:bg-white/[0.05]"
                }`}
              >
                <span className="text-sm font-semibold">{preset.name}</span>
                <span className="mt-1 line-clamp-2 text-[11px] leading-relaxed text-base-400">
                  {preset.description}
                </span>
              </button>
            );
          })}
        </div>
      </div>

      {/* Stats Cards */}
      <div className="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-4">
        <div className="rounded-xl border border-white/10 bg-base-950/50 p-4 text-center">
          <div className="text-xs text-base-400">Total Corridors</div>
          <div className="mt-1 text-2xl font-bold text-base-100">{stats.total}</div>
          <div className="text-[11px] text-base-400">
            {filteredJurisdictions.length}x{filteredJurisdictions.length} grid
          </div>
        </div>
        <div className="rounded-xl border border-emerald-500/20 bg-emerald-950/20 p-4 text-center">
          <div className="text-xs text-emerald-400">Allowed</div>
          <div className="mt-1 text-2xl font-bold text-emerald-300">{stats.allowed}</div>
          <div className="text-[11px] text-emerald-400/80">
            {stats.total > 0 ? ((stats.allowed / stats.total) * 100).toFixed(1) : 0}% of corridors
          </div>
        </div>
        <div className="rounded-xl border border-amber-500/20 bg-amber-950/20 p-4 text-center">
          <div className="text-xs text-amber-400">Accredited Only</div>
          <div className="mt-1 text-2xl font-bold text-amber-300">{stats.accredited}</div>
          <div className="text-[11px] text-amber-400/80">
            {stats.total > 0 ? ((stats.accredited / stats.total) * 100).toFixed(1) : 0}% of corridors
          </div>
        </div>
        <div className="rounded-xl border border-rose-500/20 bg-rose-950/20 p-4 text-center">
          <div className="text-xs text-rose-400">Blocked</div>
          <div className="mt-1 text-2xl font-bold text-rose-300">{stats.blocked}</div>
          <div className="text-[11px] text-rose-400/80">
            {stats.total > 0 ? ((stats.blocked / stats.total) * 100).toFixed(1) : 0}% of corridors
          </div>
        </div>
      </div>

      {/* Filters and Search Bar */}
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        {/* Region Filter Chips */}
        <div className="flex flex-wrap items-center gap-1.5">
          {REGIONS.map((r) => (
            <button
              key={r}
              type="button"
              onClick={() => {
                setSelectedRegion(r);
                setCurrentPage(1);
              }}
              className={`rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                selectedRegion === r
                  ? "bg-brand-500 text-black font-semibold shadow"
                  : "border border-white/10 bg-white/[0.03] text-base-300 hover:bg-white/[0.08]"
              }`}
            >
              {r}
            </button>
          ))}
        </div>

        {/* Search and Bulk Controls */}
        <div className="flex flex-wrap items-center gap-2">
          <input
            type="text"
            placeholder="Search country or code..."
            value={searchQuery}
            onChange={(e) => {
              setSearchQuery(e.target.value);
              setCurrentPage(1);
            }}
            className="rounded-lg border border-white/10 bg-black/40 px-3 py-1.5 text-xs text-base-100 placeholder-base-400 focus:border-brand-400 focus:outline-none focus:ring-1 focus:ring-brand-400"
          />

          <div className="flex items-center gap-1">
            <button
              type="button"
              onClick={() => setAllVisible("ALLOWED")}
              className="rounded-md border border-emerald-500/30 bg-emerald-500/10 px-2 py-1 text-[11px] font-medium text-emerald-300 hover:bg-emerald-500/20"
              title="Set all visible corridors to Allowed"
            >
              All Allowed
            </button>
            <button
              type="button"
              onClick={() => setAllVisible("ACCREDITED_ONLY")}
              className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-[11px] font-medium text-amber-300 hover:bg-amber-500/20"
              title="Set all visible corridors to Accredited Only"
            >
              All Accredited
            </button>
            <button
              type="button"
              onClick={() => setAllVisible("BLOCKED")}
              className="rounded-md border border-rose-500/30 bg-rose-500/10 px-2 py-1 text-[11px] font-medium text-rose-300 hover:bg-rose-500/20"
              title="Set all visible corridors to Blocked"
            >
              All Blocked
            </button>
            <button
              type="button"
              onClick={() => setOverrides({})}
              className="rounded-md border border-white/10 bg-white/[0.04] px-2 py-1 text-[11px] font-medium text-base-300 hover:bg-white/[0.08]"
              title="Reset manual overrides back to active preset"
            >
              Reset
            </button>
          </div>
        </div>
      </div>

      {/* 2D Interactive Grid */}
      <div className="overflow-hidden rounded-xl border border-white/10 bg-base-950/70 shadow-inner">
        <div className="overflow-x-auto">
          <table
            className="w-full border-collapse text-left text-xs"
            role="grid"
            aria-label="Cross-Border Compliance Permissibility Matrix"
          >
            <thead>
              <tr className="border-b border-white/10 bg-white/[0.03]">
                <th className="sticky left-0 z-20 min-w-[160px] border-r border-white/10 bg-base-900/95 p-3 text-xs font-bold text-base-300 backdrop-blur">
                  <div className="flex items-center justify-between">
                    <span>Source \ Dest</span>
                    <span className="text-[10px] font-normal text-base-400">
                      ({paginatedJurisdictions.length} displayed)
                    </span>
                  </div>
                </th>
                {paginatedJurisdictions.map((dest) => (
                  <th
                    key={dest.code}
                    className="min-w-[56px] p-2 text-center text-xs font-medium text-base-200"
                    title={`${dest.name} (${dest.code})`}
                  >
                    <div className="flex flex-col items-center">
                      <span className="text-base">{dest.flag}</span>
                      <span className="font-mono text-[11px] font-semibold">{dest.code}</span>
                    </div>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {paginatedJurisdictions.map((source) => (
                <tr
                  key={source.code}
                  className="border-b border-white/5 transition-colors hover:bg-white/[0.02]"
                >
                  {/* Row Header (Source Country) */}
                  <th className="sticky left-0 z-10 border-r border-white/10 bg-base-900/95 p-2.5 font-medium text-base-200 backdrop-blur">
                    <div className="flex items-center justify-between gap-2">
                      <div className="flex items-center gap-2 truncate">
                        <span className="text-base">{source.flag}</span>
                        <span className="font-mono font-semibold text-brand-300">{source.code}</span>
                        <span className="hidden truncate text-[11px] text-base-400 xl:inline">
                          {source.name}
                        </span>
                      </div>
                      <div className="flex items-center gap-1">
                        <button
                          type="button"
                          onClick={() => setRowPermission(source, "ALLOWED")}
                          className="h-4 w-4 rounded bg-emerald-500/20 text-[9px] text-emerald-300 hover:bg-emerald-500/40"
                          title={`Set all destination rules for ${source.code} to Allowed`}
                        >
                          ✓
                        </button>
                        <button
                          type="button"
                          onClick={() => setRowPermission(source, "BLOCKED")}
                          className="h-4 w-4 rounded bg-rose-500/20 text-[9px] text-rose-300 hover:bg-rose-500/40"
                          title={`Block all destination rules for ${source.code}`}
                        >
                          ✕
                        </button>
                      </div>
                    </div>
                  </th>

                  {/* Matrix Cells */}
                  {paginatedJurisdictions.map((dest) => {
                    const permission = getPermission(source, dest);
                    const config = PERMISSION_CONFIG[permission];

                    return (
                      <td
                        key={`${source.code}-${dest.code}`}
                        className="p-1 text-center"
                      >
                        <button
                          type="button"
                          onClick={() => toggleCell(source, dest)}
                          className={`group relative flex h-9 w-full items-center justify-center rounded-md border text-xs font-semibold transition-transform active:scale-95 ${config.bg} ${config.text} ${config.border}`}
                          aria-label={`Rule from ${source.name} to ${dest.name}: ${config.label}. Click to cycle.`}
                          title={`${source.name} (${source.code}) → ${dest.name} (${dest.code}): ${config.label} (Click to toggle)`}
                        >
                          <span>{config.icon}</span>
                        </button>
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        {/* Pagination Controls */}
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-white/10 bg-white/[0.02] p-3 text-xs text-base-300">
          <div className="flex items-center gap-2">
            <span>Show per page:</span>
            <select
              value={pageSize}
              onChange={(e) => {
                setPageSize(Number(e.target.value));
                setCurrentPage(1);
              }}
              className="rounded border border-white/10 bg-black/40 px-2 py-1 text-xs text-base-100"
            >
              <option value={8}>8 countries</option>
              <option value={12}>12 countries</option>
              <option value={20}>20 countries</option>
              <option value={filteredJurisdictions.length}>All visible</option>
            </select>
          </div>

          <div className="flex items-center gap-2">
            <span>
              Page {currentPage} of {totalPages} ({filteredJurisdictions.length} countries matching)
            </span>
            <div className="flex items-center gap-1">
              <button
                type="button"
                disabled={currentPage <= 1}
                onClick={() => setCurrentPage((p) => Math.max(p - 1, 1))}
                className="rounded border border-white/10 bg-white/[0.03] px-2 py-1 disabled:opacity-40"
              >
                Previous
              </button>
              <button
                type="button"
                disabled={currentPage >= totalPages}
                onClick={() => setCurrentPage((p) => Math.min(p + 1, totalPages))}
                className="rounded border border-white/10 bg-white/[0.03] px-2 py-1 disabled:opacity-40"
              >
                Next
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Raw Soroban Contract Initialization Arguments Generator */}
      <div className="mt-8 rounded-xl border border-white/10 bg-base-950/60 p-5">
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-white/10 pb-4">
          <div>
            <h3 className="text-base font-semibold text-base-100">
              Raw Soroban Contract Arguments Generator
            </h3>
            <p className="text-xs text-base-400">
              Generate ready-to-run Soroban CLI commands, Rust smart contract vectors, or JSON RPC payloads.
            </p>
          </div>

          <div className="flex items-center gap-2">
            <div className="flex rounded-lg border border-white/10 bg-black/40 p-0.5 text-xs">
              <button
                type="button"
                onClick={() => setExportFormat("cli")}
                className={`rounded-md px-3 py-1 font-medium transition-colors ${
                  exportFormat === "cli"
                    ? "bg-brand-500 text-black font-semibold"
                    : "text-base-300 hover:text-base-100"
                }`}
              >
                Soroban CLI
              </button>
              <button
                type="button"
                onClick={() => setExportFormat("rust")}
                className={`rounded-md px-3 py-1 font-medium transition-colors ${
                  exportFormat === "rust"
                    ? "bg-brand-500 text-black font-semibold"
                    : "text-base-300 hover:text-base-100"
                }`}
              >
                Rust SDK
              </button>
              <button
                type="button"
                onClick={() => setExportFormat("json")}
                className={`rounded-md px-3 py-1 font-medium transition-colors ${
                  exportFormat === "json"
                    ? "bg-brand-500 text-black font-semibold"
                    : "text-base-300 hover:text-base-100"
                }`}
              >
                JSON RPC
              </button>
            </div>

            <button
              type="button"
              onClick={copyCode}
              className="flex items-center gap-1.5 rounded-lg border border-brand-400/40 bg-brand-500/20 px-3 py-1.5 text-xs font-semibold text-brand-300 hover:bg-brand-500/30"
            >
              {copied ? "✓ Copied!" : "📋 Copy Arguments"}
            </button>
          </div>
        </div>

        {/* Contract Admin Input */}
        <div className="my-3 flex flex-wrap items-center gap-2 text-xs">
          <label className="text-base-300">Contract Admin Stellar Key:</label>
          <input
            type="text"
            value={contractAdmin}
            onChange={(e) => setContractAdmin(e.target.value)}
            className="flex-1 rounded-md border border-white/10 bg-black/40 px-3 py-1 font-mono text-xs text-brand-300 focus:border-brand-400 focus:outline-none"
          />
        </div>

        {/* Code Output Box */}
        <div className="relative">
          <pre className="max-h-64 overflow-x-auto rounded-lg border border-white/10 bg-black/70 p-4 font-mono text-xs text-base-200">
            <code>{generatedCode}</code>
          </pre>
        </div>
      </div>
    </div>
  );
}
