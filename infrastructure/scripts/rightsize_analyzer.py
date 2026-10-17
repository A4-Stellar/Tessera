#!/usr/bin/env python3
"""Automated Resource Usage Optimization & Right-Sizing Analyzer (Issue #313).

Analyzes 14-day container CPU and RAM usage history, generates optimal Kubernetes
resource request/limit recommendations, and identifies idle or over-provisioned
infrastructure resources to reduce cloud costs.
"""

import argparse
import json
import math
import os
import re
import sys
from dataclasses import asdict, dataclass, field
from datetime import datetime, timedelta, timezone
from enum import Enum
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple


class ProvisioningStatus(str, Enum):
    IDLE = "IDLE"
    SEVERELY_OVERPROVISIONED = "SEVERELY_OVERPROVISIONED"
    OVERPROVISIONED = "OVERPROVISIONED"
    WELL_SIZED = "WELL_SIZED"
    UNDERPROVISIONED = "UNDERPROVISIONED"


@dataclass
class MetricTimeSeries:
    """Historical usage series over an observation window."""
    timestamps: List[float] = field(default_factory=list)
    values: List[float] = field(default_factory=list)

    def count(self) -> int:
        return len(self.values)

    def mean(self) -> float:
        return sum(self.values) / len(self.values) if self.values else 0.0

    def percentile(self, p: float) -> float:
        if not self.values:
            return 0.0
        sorted_vals = sorted(self.values)
        k = (len(sorted_vals) - 1) * (p / 100.0)
        f = math.floor(k)
        c = math.ceil(k)
        if f == c:
            return sorted_vals[int(k)]
        d0 = sorted_vals[int(f)] * (c - k)
        d1 = sorted_vals[int(c)] * (k - f)
        return d0 + d1

    def max_val(self) -> float:
        return max(self.values) if self.values else 0.0

    def min_val(self) -> float:
        return min(self.values) if self.values else 0.0


@dataclass
class ContainerUsageProfile:
    namespace: str
    workload_name: str
    container_name: str
    observation_days: float
    current_cpu_request_cores: float
    current_cpu_limit_cores: float
    current_memory_request_mib: float
    current_memory_limit_mib: float
    cpu_usage_cores: MetricTimeSeries = field(default_factory=MetricTimeSeries)
    memory_usage_mib: MetricTimeSeries = field(default_factory=MetricTimeSeries)


@dataclass
class ResourceRecommendation:
    cpu_request_cores: float
    cpu_request_k8s: str
    cpu_limit_cores: float
    cpu_limit_k8s: str
    memory_request_mib: float
    memory_request_k8s: str
    memory_limit_mib: float
    memory_limit_k8s: str


@dataclass
class WorkloadAnalysisResult:
    namespace: str
    workload_name: str
    container_name: str
    status: str
    current_resources: Dict[str, Any]
    recommended_resources: Dict[str, Any]
    cpu_metrics: Dict[str, float]
    memory_metrics: Dict[str, float]
    current_monthly_cost_usd: float
    recommended_monthly_cost_usd: float
    monthly_savings_usd: float
    savings_percentage: float
    recommendations: List[str]
    kubernetes_patch: Dict[str, Any]


@dataclass
class FinOpsReport:
    timestamp: str
    observation_period_days: float
    total_containers_analyzed: int
    idle_containers: int
    overprovisioned_containers: int
    well_sized_containers: int
    underprovisioned_containers: int
    current_total_monthly_cost_usd: float
    recommended_total_monthly_cost_usd: float
    total_monthly_savings_usd: float
    overall_savings_percentage: float
    workloads: List[WorkloadAnalysisResult] = field(default_factory=list)


# ---------------------------------------------------------------------------
# FinOps Cost Model (Standard Cloud EKS/GKE Pricing)
# ---------------------------------------------------------------------------
# Typical multi-cloud blended rates:
# 1 vCPU / month ≈ 730 hours * $0.04048 = ~$29.55 / month
# 1 GiB RAM / month ≈ 730 hours * $0.004445 = ~$3.25 / month
VCPU_HOURLY_COST = 0.04048
GIB_HOURLY_COST = 0.004445
HOURS_PER_MONTH = 730.0


def calculate_container_monthly_cost(cpu_cores: float, memory_mib: float) -> float:
    cpu_cost = cpu_cores * VCPU_HOURLY_COST * HOURS_PER_MONTH
    mem_gib = memory_mib / 1024.0
    mem_cost = mem_gib * GIB_HOURLY_COST * HOURS_PER_MONTH
    return round(cpu_cost + mem_cost, 2)


# ---------------------------------------------------------------------------
# Resource Parser & Formatter Helpers
# ---------------------------------------------------------------------------
def parse_cpu_to_cores(val: str) -> float:
    """Parse k8s CPU value (e.g. '250m', '1', '0.5') to float cores."""
    val = str(val).strip()
    if val.endswith("m"):
        return float(val[:-1]) / 1000.0
    return float(val)


def parse_memory_to_mib(val: str) -> float:
    """Parse k8s memory value (e.g. '512Mi', '2Gi', '1024M') to float MiB."""
    val = str(val).strip()
    if val.endswith("Gi"):
        return float(val[:-2]) * 1024.0
    elif val.endswith("Mi"):
        return float(val[:-2])
    elif val.endswith("Ki"):
        return float(val[:-2]) / 1024.0
    elif val.endswith("G"):
        return float(val[:-1]) * 1000.0 * 1000.0 * 1000.0 / (1024.0 * 1024.0)
    elif val.endswith("M"):
        return float(val[:-1]) * 1000.0 * 1000.0 / (1024.0 * 1024.0)
    elif val.isdigit():
        return float(val) / (1024.0 * 1024.0)
    return float(val)


def format_cpu_k8s(cores: float) -> str:
    """Format cores into millicores string (e.g. '250m') or integer cores."""
    millicores = int(math.ceil(cores * 1000.0))
    # Round up to nearest 10m
    millicores = max(10, ((millicores + 9) // 10) * 10)
    return f"{millicores}m"


def format_memory_k8s(mib: float) -> str:
    """Format MiB into k8s string (e.g. '512Mi', '2Gi')."""
    rounded_mib = int(math.ceil(mib))
    # Round up to nearest 16Mi
    rounded_mib = max(32, ((rounded_mib + 15) // 16) * 16)
    if rounded_mib >= 1024 and (rounded_mib % 1024 == 0):
        return f"{rounded_mib // 1024}Gi"
    return f"{rounded_mib}Mi"


# ---------------------------------------------------------------------------
# Right-Sizing Analyzer Engine
# ---------------------------------------------------------------------------
class RightSizeAnalyzer:
    """Evaluates 14-day metrics history and generates optimal right-sizing recommendations."""

    def __init__(
        self,
        cpu_safety_buffer_pct: float = 15.0,
        memory_safety_buffer_pct: float = 20.0,
        cpu_burst_headroom_pct: float = 50.0,
        memory_limit_factor: float = 1.25,
    ):
        self.cpu_safety_buffer_pct = cpu_safety_buffer_pct
        self.memory_safety_buffer_pct = memory_safety_buffer_pct
        self.cpu_burst_headroom_pct = cpu_burst_headroom_pct
        self.memory_limit_factor = memory_limit_factor

    def analyze_container(self, profile: ContainerUsageProfile) -> WorkloadAnalysisResult:
        cpu_series = profile.cpu_usage_cores
        mem_series = profile.memory_usage_mib

        # Compute percentiles
        cpu_p50 = cpu_series.percentile(50)
        cpu_p90 = cpu_series.percentile(90)
        cpu_p95 = cpu_series.percentile(95)
        cpu_p99 = cpu_series.percentile(99)
        cpu_max = cpu_series.max_val()
        cpu_mean = cpu_series.mean()

        mem_p50 = mem_series.percentile(50)
        mem_p90 = mem_series.percentile(90)
        mem_p95 = mem_series.percentile(95)
        mem_p99 = mem_series.percentile(99)
        mem_max = mem_series.max_val()
        mem_mean = mem_series.mean()

        # Recommendation Math:
        # Request = p95 + safety buffer
        rec_cpu_req = max(0.02, cpu_p95 * (1.0 + self.cpu_safety_buffer_pct / 100.0))
        # Limit = p99 + burst headroom (or at least 1.5x request to prevent throttling)
        rec_cpu_lim = max(rec_cpu_req * 1.5, cpu_max * (1.0 + 0.10))

        # Memory Request = p95 working set + 20% buffer
        rec_mem_req = max(64.0, mem_p95 * (1.0 + self.memory_safety_buffer_pct / 100.0))
        # Memory Limit = request * limit factor (prevents OOMKill)
        rec_mem_lim = max(rec_mem_req * self.memory_limit_factor, mem_max * 1.15)

        rec = ResourceRecommendation(
            cpu_request_cores=rec_cpu_req,
            cpu_request_k8s=format_cpu_k8s(rec_cpu_req),
            cpu_limit_cores=rec_cpu_lim,
            cpu_limit_k8s=format_cpu_k8s(rec_cpu_lim),
            memory_request_mib=rec_mem_req,
            memory_request_k8s=format_memory_k8s(rec_mem_req),
            memory_limit_mib=rec_mem_lim,
            memory_limit_k8s=format_memory_k8s(rec_mem_lim),
        )

        # Classification of provisioning state:
        curr_cpu_req = profile.current_cpu_request_cores
        curr_mem_req = profile.current_memory_request_mib

        cpu_util_pct = (cpu_mean / curr_cpu_req * 100.0) if curr_cpu_req > 0 else 0.0
        mem_util_pct = (mem_mean / curr_mem_req * 100.0) if curr_mem_req > 0 else 0.0

        if cpu_util_pct < 5.0 and mem_util_pct < 10.0:
            status = ProvisioningStatus.IDLE
        elif curr_cpu_req > 3.0 * rec_cpu_req or curr_mem_req > 3.0 * rec_mem_req:
            status = ProvisioningStatus.SEVERELY_OVERPROVISIONED
        elif curr_cpu_req > 1.3 * rec_cpu_req or curr_mem_req > 1.3 * rec_mem_req:
            status = ProvisioningStatus.OVERPROVISIONED
        elif (
            (profile.current_cpu_limit_cores > 0 and cpu_max > 0.90 * profile.current_cpu_limit_cores)
            or (profile.current_memory_limit_mib > 0 and mem_max > 0.90 * profile.current_memory_limit_mib)
        ):
            status = ProvisioningStatus.UNDERPROVISIONED
        else:
            status = ProvisioningStatus.WELL_SIZED

        # FinOps Cost Analysis
        current_cost = calculate_container_monthly_cost(curr_cpu_req, curr_mem_req)
        recommended_cost = calculate_container_monthly_cost(rec.cpu_request_cores, rec.memory_request_mib)
        savings = round(max(0.0, current_cost - recommended_cost), 2)
        savings_pct = round((savings / current_cost * 100.0), 1) if current_cost > 0 else 0.0

        recommendations = []
        if status in [ProvisioningStatus.IDLE, ProvisioningStatus.SEVERELY_OVERPROVISIONED]:
            recommendations.append(
                f"Container is {status.value}: Downscale CPU from {format_cpu_k8s(curr_cpu_req)} to {rec.cpu_request_k8s} and RAM from {format_memory_k8s(curr_mem_req)} to {rec.memory_request_k8s} to save ${savings:.2f}/mo ({savings_pct:.0f}%)."
            )
        elif status == ProvisioningStatus.OVERPROVISIONED:
            recommendations.append(
                f"Moderate over-provisioning detected: Reduce allocated requests to {rec.cpu_request_k8s} CPU / {rec.memory_request_k8s} RAM."
            )
        elif status == ProvisioningStatus.UNDERPROVISIONED:
            recommendations.append(
                f"High utilization / throttling risk: Increase limits to {rec.cpu_limit_k8s} CPU and {rec.memory_limit_k8s} RAM to ensure headroom."
            )
        else:
            recommendations.append(
                f"Resource requests and limits are well-aligned with historical 14-day traffic patterns."
            )

        k8s_patch = {
            "spec": {
                "template": {
                    "spec": {
                        "containers": [
                            {
                                "name": profile.container_name,
                                "resources": {
                                    "requests": {
                                        "cpu": rec.cpu_request_k8s,
                                        "memory": rec.memory_request_k8s,
                                    },
                                    "limits": {
                                        "cpu": rec.cpu_limit_k8s,
                                        "memory": rec.memory_limit_k8s,
                                    },
                                },
                            }
                        ]
                    }
                }
            }
        }

        return WorkloadAnalysisResult(
            namespace=profile.namespace,
            workload_name=profile.workload_name,
            container_name=profile.container_name,
            status=status.value,
            current_resources={
                "cpu_request": format_cpu_k8s(curr_cpu_req),
                "cpu_limit": format_cpu_k8s(profile.current_cpu_limit_cores),
                "memory_request": format_memory_k8s(curr_mem_req),
                "memory_limit": format_memory_k8s(profile.current_memory_limit_mib),
            },
            recommended_resources={
                "cpu_request": rec.cpu_request_k8s,
                "cpu_limit": rec.cpu_limit_k8s,
                "memory_request": rec.memory_request_k8s,
                "memory_limit": rec.memory_limit_k8s,
            },
            cpu_metrics={
                "mean_cores": round(cpu_mean, 4),
                "p50_cores": round(cpu_p50, 4),
                "p90_cores": round(cpu_p90, 4),
                "p95_cores": round(cpu_p95, 4),
                "p99_cores": round(cpu_p99, 4),
                "max_cores": round(cpu_max, 4),
            },
            memory_metrics={
                "mean_mib": round(mem_mean, 2),
                "p50_mib": round(mem_p50, 2),
                "p90_mib": round(mem_p90, 2),
                "p95_mib": round(mem_p95, 2),
                "p99_mib": round(mem_p99, 2),
                "max_mib": round(mem_max, 2),
            },
            current_monthly_cost_usd=current_cost,
            recommended_monthly_cost_usd=recommended_cost,
            monthly_savings_usd=savings,
            savings_percentage=savings_pct,
            recommendations=recommendations,
            kubernetes_patch=k8s_patch,
        )

    def analyze_all(self, profiles: List[ContainerUsageProfile]) -> FinOpsReport:
        results = [self.analyze_container(p) for p in profiles]

        idle = sum(1 for r in results if r.status == ProvisioningStatus.IDLE.value)
        over = sum(
            1
            for r in results
            if r.status in [ProvisioningStatus.OVERPROVISIONED.value, ProvisioningStatus.SEVERELY_OVERPROVISIONED.value]
        )
        well = sum(1 for r in results if r.status == ProvisioningStatus.WELL_SIZED.value)
        under = sum(1 for r in results if r.status == ProvisioningStatus.UNDERPROVISIONED.value)

        curr_cost = round(sum(r.current_monthly_cost_usd for r in results), 2)
        rec_cost = round(sum(r.recommended_monthly_cost_usd for r in results), 2)
        savings = round(max(0.0, curr_cost - rec_cost), 2)
        savings_pct = round((savings / curr_cost * 100.0), 1) if curr_cost > 0 else 0.0

        return FinOpsReport(
            timestamp=datetime.now(timezone.utc).isoformat(),
            observation_period_days=14.0,
            total_containers_analyzed=len(results),
            idle_containers=idle,
            overprovisioned_containers=over,
            well_sized_containers=well,
            underprovisioned_containers=under,
            current_total_monthly_cost_usd=curr_cost,
            recommended_total_monthly_cost_usd=rec_cost,
            total_monthly_savings_usd=savings,
            overall_savings_percentage=savings_pct,
            workloads=results,
        )


# ---------------------------------------------------------------------------
# Synthetic 14-Day Metrics Generator (for Testing & Benchmarks)
# ---------------------------------------------------------------------------
def generate_synthetic_14d_profile(
    namespace: str,
    workload_name: str,
    container_name: str,
    behavior: str = "overprovisioned",
) -> ContainerUsageProfile:
    """Generate realistic 14-day timeseries (samples every 15 min = 1344 points)."""
    samples = 14 * 24 * 4  # 1,344 points
    now = datetime.now(timezone.utc).timestamp()
    timestamps = [now - (i * 900.0) for i in range(samples)]

    cpu_vals = []
    mem_vals = []

    if behavior == "idle":
        # Allocated 1000m / 2048Mi, but uses < 20m / 50Mi
        curr_cpu_req, curr_cpu_lim = 1.0, 2.0
        curr_mem_req, curr_mem_lim = 2048.0, 4096.0
        for i in range(samples):
            cpu_vals.append(0.01 + 0.005 * math.sin(i / 10.0))
            mem_vals.append(45.0 + 5.0 * math.cos(i / 20.0))

    elif behavior == "overprovisioned":
        # Allocated 2000m / 4096Mi, but uses 250m p95 / 512Mi p95
        curr_cpu_req, curr_cpu_lim = 2.0, 4.0
        curr_mem_req, curr_mem_lim = 4096.0, 8192.0
        for i in range(samples):
            # Diurnal load curve
            diurnal = 0.15 + 0.08 * math.sin((i % 96) / 15.0)
            cpu_vals.append(diurnal + (0.05 if i % 100 == 0 else 0.0))
            mem_vals.append(480.0 + 40.0 * math.sin((i % 96) / 20.0))

    elif behavior == "underprovisioned":
        # Allocated 200m / 256Mi, but regularly spikes to 195m / 250Mi
        curr_cpu_req, curr_cpu_lim = 0.2, 0.2
        curr_mem_req, curr_mem_lim = 256.0, 256.0
        for i in range(samples):
            cpu_vals.append(0.18 + 0.015 * math.sin(i / 5.0))
            mem_vals.append(245.0 + 8.0 * math.sin(i / 8.0))

    else:  # well_sized
        curr_cpu_req, curr_cpu_lim = 0.5, 1.0
        curr_mem_req, curr_mem_lim = 1024.0, 2048.0
        for i in range(samples):
            cpu_vals.append(0.35 + 0.05 * math.sin(i / 10.0))
            mem_vals.append(750.0 + 50.0 * math.cos(i / 15.0))

    return ContainerUsageProfile(
        namespace=namespace,
        workload_name=workload_name,
        container_name=container_name,
        observation_days=14.0,
        current_cpu_request_cores=curr_cpu_req,
        current_cpu_limit_cores=curr_cpu_lim,
        current_memory_request_mib=curr_mem_req,
        current_memory_limit_mib=curr_mem_lim,
        cpu_usage_cores=MetricTimeSeries(timestamps=timestamps, values=cpu_vals),
        memory_usage_mib=MetricTimeSeries(timestamps=timestamps, values=mem_vals),
    )


# ---------------------------------------------------------------------------
# Report Formatting
# ---------------------------------------------------------------------------
def format_markdown_report(report: FinOpsReport) -> str:
    lines = [
        "# Tessera FinOps Resource Right-Sizing & Optimization Report",
        "",
        f"**Observation Period:** `{report.observation_period_days} Days`  ",
        f"**Total Workloads Analyzed:** `{report.total_containers_analyzed}`  ",
        f"**Current Monthly Infrastructure Cost:** `${report.current_total_monthly_cost_usd:.2f}`  ",
        f"**Recommended Monthly Cost:** `${report.recommended_total_monthly_cost_usd:.2f}`  ",
        f"**Projected Monthly Cost Savings:** `${report.total_monthly_savings_usd:.2f} ({report.overall_savings_percentage:.1f}%)` 💰  ",
        "",
        "## Workload Provisioning Breakdown",
        f"- **Idle Workloads:** `{report.idle_containers}` (Safe to scale to zero or decommission)",
        f"- **Over-Provisioned:** `{report.overprovisioned_containers}` (Excess requests starving cluster)",
        f"- **Well-Sized:** `{report.well_sized_containers}` (Optimally aligned)",
        f"- **Under-Provisioned:** `{report.underprovisioned_containers}` (Requires limit increase)",
        "",
        "## Container Recommendations",
        "",
        "| Workload | Status | Current Req (CPU/RAM) | Recommended Req (CPU/RAM) | Current Cost | Rec Cost | Monthly Savings |",
        "|---|---|---|---|---|---|---|",
    ]
    for w in report.workloads:
        curr_str = f"{w.current_resources['cpu_request']} / {w.current_resources['memory_request']}"
        rec_str = f"{w.recommended_resources['cpu_request']} / {w.recommended_resources['memory_request']}"
        lines.append(
            f"| `{w.namespace}/{w.workload_name}` | `{w.status}` | {curr_str} | **{rec_str}** | ${w.current_monthly_cost_usd:.2f} | ${w.recommended_monthly_cost_usd:.2f} | **${w.monthly_savings_usd:.2f} ({w.savings_percentage:.0f}%)** |"
        )

    lines.extend([
        "",
        "## Next Steps & Application",
        "Apply the generated Kubernetes patch definitions via `kubectl patch` or update the Helm values override files.",
    ])
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(
        description="Automated Resource Usage Optimization & Right-Sizing Analyzer (Issue #313)"
    )
    parser.add_argument(
        "--input-file",
        help="Path to JSON file containing container usage metrics export",
    )
    parser.add_argument(
        "--output-json",
        default="infrastructure/scripts/rightsize_report.json",
        help="Path to save output JSON report",
    )
    parser.add_argument(
        "--output-md",
        default="infrastructure/scripts/rightsize_report.md",
        help="Path to save output Markdown report",
    )
    parser.add_argument(
        "--output-patch",
        default="infrastructure/scripts/recommended_patches.yaml",
        help="Path to save recommended Kubernetes patches YAML",
    )
    args = parser.parse_args()

    analyzer = RightSizeAnalyzer()

    # If no input file is supplied, analyze standard Tessera system microservices
    profiles = [
        generate_synthetic_14d_profile("tessera-system", "tessera-api", "tessera-api", "overprovisioned"),
        generate_synthetic_14d_profile("tessera-system", "channel-node", "channel-node", "well_sized"),
        generate_synthetic_14d_profile("tessera-system", "mpc-signer", "mpc-signer", "idle"),
        generate_synthetic_14d_profile("tessera-system", "synthetic-monitor", "probe-worker", "underprovisioned"),
    ]

    report = analyzer.analyze_all(profiles)

    os.makedirs(os.path.dirname(os.path.abspath(args.output_json)), exist_ok=True)
    with open(args.output_json, "w") as f:
        json.dump(asdict(report), f, indent=2)

    with open(args.output_md, "w") as f:
        f.write(format_markdown_report(report))

    # Write k8s patches using standard library formatting (zero third-party dependencies)
    with open(args.output_patch, "w") as f:
        for w in report.workloads:
            c = w.recommended_resources
            f.write(f"# Workload: {w.namespace}/{w.workload_name}\n")
            f.write("spec:\n")
            f.write("  template:\n")
            f.write("    spec:\n")
            f.write("      containers:\n")
            f.write(f"        - name: {w.container_name}\n")
            f.write("          resources:\n")
            f.write("            requests:\n")
            f.write(f"              cpu: \"{c['cpu_request']}\"\n")
            f.write(f"              memory: \"{c['memory_request']}\"\n")
            f.write("            limits:\n")
            f.write(f"              cpu: \"{c['cpu_limit']}\"\n")
            f.write(f"              memory: \"{c['memory_limit']}\"\n")
            f.write("---\n")

    print(f"[+] 14-Day Right-Sizing Analysis Complete!")
    print(f"[+] Total Containers Analyzed: {report.total_containers_analyzed}")
    print(f"[+] Current Monthly Cost:     ${report.current_total_monthly_cost_usd:.2f}")
    print(f"[+] Recommended Monthly Cost:   ${report.recommended_total_monthly_cost_usd:.2f}")
    print(f"[+] Projected Monthly Savings:  ${report.total_monthly_savings_usd:.2f} ({report.overall_savings_percentage:.1f}%)")
    print(f"[+] JSON Report:    {args.output_json}")
    print(f"[+] Markdown Summary: {args.output_md}")
    print(f"[+] K8s Patches:    {args.output_patch}")


if __name__ == "__main__":
    main()
