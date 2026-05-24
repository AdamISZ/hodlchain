"""Run the hodlchain retargeting simulator across all scenarios and
emit PNG plots.

Usage:
    python sim/run.py                      # all scenarios
    python sim/run.py quiet_then_burst     # only the named one(s)

Output goes to ``sim/out/<scenario>.png`` plus ``sim/out/summary.png``.
The Rust binary in ``sim/hodl-simulate/`` is auto-built (release) on
first invocation.
"""

import json
import subprocess
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

import scenarios

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent
OUT_DIR = HERE / "out"


def build_simulator() -> Path:
    print("[..] building hodl-simulate (release)...")
    subprocess.run(
        ["cargo", "build", "-p", "hodl-simulate", "--release", "--quiet"],
        cwd=REPO_ROOT,
        check=True,
    )
    bin_path = REPO_ROOT / "target" / "release" / "hodl-simulate"
    if not bin_path.exists():
        raise FileNotFoundError(f"binary not found at {bin_path}")
    return bin_path


def run_one(binary: Path, scenario: dict) -> dict:
    res = subprocess.run(
        [str(binary)],
        input=json.dumps(scenario),
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(res.stdout)


def plot_scenario(name: str, scenario: dict, trace: dict, out_path: Path) -> None:
    params = scenario["params"]
    m_star = params["target_atoms_per_block"]
    m_w = params["retarget_window_atoms"]
    initial_r = params["initial_r"]

    h = np.array(trace["l1_height"])
    mints = np.array(trace["minted_at_height"], dtype=float)
    cum = np.array(trace["cumulative_atoms"], dtype=float)
    r = np.array(trace["current_r"], dtype=float)
    win = np.array(trace["window_atoms"], dtype=float)
    retargets = trace["retargets"]

    fig, axes = plt.subplots(4, 1, figsize=(12, 10), sharex=True, constrained_layout=True)
    fig.suptitle(f"hodlchain retargeting simulation: {name}", fontsize=13)

    # Panel 1: mints per L1 block (stem-like; use bar of width 1)
    ax = axes[0]
    ax.bar(h, mints / 1e6, width=1.0, color="steelblue", alpha=0.8)
    ax.set_ylabel("atoms minted\n(millions / L1 block)")
    ax.grid(True, axis="y", alpha=0.3)

    # Panel 2: cumulative L2 supply vs. M*·H baseline
    ax = axes[1]
    ax.plot(h, cum / 1e6, color="darkblue", lw=1.5, label="actual L2 supply")
    projected = m_star * h
    ax.plot(h, projected / 1e6, color="gray", lw=1, ls="--", label="projection (M*·H)")
    ax.set_ylabel("cumulative atoms\n(millions)")
    ax.legend(loc="upper left", fontsize=8)
    ax.grid(True, alpha=0.3)

    # Panel 3: current r
    ax = axes[2]
    ax.plot(h, r, color="darkred", lw=1.5)
    ax.axhline(initial_r, color="gray", ls=":", lw=1, label=f"initial r = {initial_r:.2e}")
    ax.set_ylabel("current r\n(per L1 block)")
    ax.legend(loc="upper right", fontsize=8)
    ax.grid(True, alpha=0.3)

    # Panel 4: window atoms with M_w threshold
    ax = axes[3]
    ax.plot(h, win / 1e6, color="darkgreen", lw=1)
    ax.axhline(m_w / 1e6, color="orange", ls="--", lw=1, label=f"M_w = {m_w/1e6:.0f}M")
    ax.set_ylabel("window atoms\n(millions)")
    ax.set_xlabel("L1 block height")
    ax.legend(loc="upper right", fontsize=8)
    ax.grid(True, alpha=0.3)

    # Mark retargets on every panel: red when r shrank, green when grew.
    for rt in retargets:
        color = "red" if rt["new_r"] < rt["old_r"] else "green"
        for ax in axes:
            ax.axvline(rt["l1_height"], color=color, alpha=0.25, lw=0.7)

    final_cum = cum[-1]
    final_proj = m_star * h[-1]
    deviation_pct = 100.0 * (final_cum - final_proj) / max(final_proj, 1.0)
    axes[0].set_title(
        f"final supply: {final_cum/1e6:.1f}M atoms  "
        f"vs.  projection: {final_proj/1e6:.1f}M  "
        f"({deviation_pct:+.1f}%)  |  retargets: {len(retargets)}",
        fontsize=10,
    )

    fig.savefig(out_path, dpi=120)
    plt.close(fig)
    print(f"[ok] wrote {out_path.relative_to(REPO_ROOT)}")


def plot_summary(results: list, out_path: Path) -> None:
    """Cross-scenario bar chart: actual / projected at horizon, log scale."""
    names = []
    ratios = []
    actuals = []
    projections = []
    for name, scenario, trace in results:
        h_max = scenario["horizon_l1_blocks"]
        m_star = scenario["params"]["target_atoms_per_block"]
        final_cum = trace["cumulative_atoms"][-1]
        proj = m_star * h_max
        if proj == 0:
            continue
        names.append(name)
        ratios.append(final_cum / proj)
        actuals.append(final_cum)
        projections.append(proj)

    order = np.argsort(ratios)
    names_s = [names[i] for i in order]
    ratios_s = [ratios[i] for i in order]

    def color(r):
        if r < 0.5:
            return "crimson"
        if r < 0.9:
            return "darkorange"
        if r < 1.1:
            return "seagreen"
        return "steelblue"

    fig, ax = plt.subplots(figsize=(10, 0.5 * max(len(names_s), 4) + 2), constrained_layout=True)
    bars = ax.barh(names_s, ratios_s, color=[color(r) for r in ratios_s])
    ax.axvline(1.0, color="gray", ls="--", lw=1, label="projected = M*·H")
    ax.set_xscale("log")
    ax.set_xlabel("final actual supply / projected supply  (log)")
    ax.set_title("Cumulative L2 supply vs. projection at scenario horizon")
    ax.grid(True, axis="x", alpha=0.3)
    ax.legend(loc="lower right")
    for bar, r in zip(bars, ratios_s):
        ax.text(r, bar.get_y() + bar.get_height() / 2, f"  ×{r:.2g}", va="center", fontsize=8)
    fig.savefig(out_path, dpi=120)
    plt.close(fig)
    print(f"[ok] wrote {out_path.relative_to(REPO_ROOT)}")


def main() -> None:
    OUT_DIR.mkdir(exist_ok=True)
    binary = build_simulator()
    wanted = set(sys.argv[1:])
    results = []
    for name, scenario in scenarios.SCENARIOS:
        if wanted and name not in wanted:
            continue
        print(f"[..] running {name} (horizon={scenario['horizon_l1_blocks']}, "
              f"events={len(scenario['events'])})...")
        trace = run_one(binary, scenario)
        plot_scenario(name, scenario, trace, OUT_DIR / f"{name}.png")
        results.append((name, scenario, trace))
    if not wanted and len(results) > 1:
        plot_summary(results, OUT_DIR / "summary.png")


if __name__ == "__main__":
    main()
