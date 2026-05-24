"""Retargeting-simulator scenario catalog.

Each generator returns ``(name, scenario_dict)`` where ``scenario_dict``
is the JSON payload accepted on stdin by the ``hodl-simulate`` binary:

    {
      "params": {
        "initial_r": float,
        "target_atoms_per_block": int,
        "retarget_window_atoms": int,
        "retarget_max_factor": float,
      },
      "horizon_l1_blocks": int,
      "events": [
        {"l1_height": int, "value_sat": int, "lock_blocks": int},
        ...
      ],
    }

Add new scenarios by writing a generator and appending it to the
``SCENARIOS`` list at the bottom.
"""

import math
import random

# Demo-scale params — match crates/hodl-core/src/consensus.rs at HEAD.
# (INITIAL_R = 1/1000, TARGET_ATOMS_PER_BLOCK = 1M, RETARGET_MINT_WINDOW
# = 100M, RETARGET_MAX_FACTOR = 2.)
DEMO_PARAMS = {
    "initial_r": 1.0 / 1000.0,
    "target_atoms_per_block": 1_000_000,
    "retarget_window_atoms": 100_000_000,
    "retarget_max_factor": 2.0,
}

# Planned-mainnet params from the same file.
MAINNET_PARAMS = {
    "initial_r": 1.0 / 26_280.0,
    "target_atoms_per_block": 50_000_000,
    "retarget_window_atoms": 216_000_000_000,
    "retarget_max_factor": 2.0,
}


def mint_fn(value_sat: int, lock_blocks: int, r: float) -> int:
    """Python mirror of ``hodl_core::consensus::mint_fn``. Used by
    scenarios that need to size mints to land near a target output.
    The simulator itself calls the Rust implementation — this is for
    sizing arithmetic only."""
    if lock_blocks == 0 or value_sat == 0:
        return 0
    rt = r * lock_blocks
    ratio = 1.0 - (1.0 + rt) * math.exp(-rt)
    ratio = max(0.0, min(1.0 - 2.22e-16, ratio))
    return int(value_sat * ratio)


def _make(name, params, horizon, events):
    return name, {
        "params": params,
        "horizon_l1_blocks": horizon,
        "events": events,
    }


# ---------- scenarios ----------

def steady_match_m_star(horizon=2000):
    """One mint per L1 block, sized so atoms minted ≈ M* per block.
    With demo r=1/1000 and T=1000, mint_fn ratio ≈ 0.264, so V ≈ 3.8M
    sat per block gives ≈ 1M atoms — the steady-state target.
    Expect: ``r`` drifts only slightly, supply ≈ projection."""
    p = DEMO_PARAMS
    events = [
        {"l1_height": h, "value_sat": 3_800_000, "lock_blocks": 1000}
        for h in range(1, horizon + 1)
    ]
    return _make("steady_match", p, horizon, events)


def under_target(horizon=2000):
    """Constant cadence, each mint half steady-state. Observed rate is
    below M*, so the algorithm should *grow* ``r`` to compensate
    (clamped at +2× per window)."""
    p = DEMO_PARAMS
    events = [
        {"l1_height": h, "value_sat": 1_900_000, "lock_blocks": 1000}
        for h in range(1, horizon + 1)
    ]
    return _make("under_target", p, horizon, events)


def over_target(horizon=2000):
    """Constant cadence, each mint double steady-state. Observed rate
    above M*, so ``r`` shrinks (clamped at −2× per window)."""
    p = DEMO_PARAMS
    events = [
        {"l1_height": h, "value_sat": 7_600_000, "lock_blocks": 1000}
        for h in range(1, horizon + 1)
    ]
    return _make("over_target", p, horizon, events)


def quiet_then_burst(horizon=3000):
    """500 quiet blocks, one big mint, then quiet again. Tests the
    edge case where a single mint pushes ``window_atoms`` past M_w in
    a single block: Δ_actual=0 → retarget defers to the next block,
    where Δ_actual=1 makes ``m_obs`` huge → ratio clamped to 1/2.
    After that the algorithm is idle and ``r`` is preserved at the
    halved value for the rest of the horizon."""
    p = DEMO_PARAMS
    events = [{"l1_height": 500, "value_sat": 500_000_000, "lock_blocks": 5000}]
    return _make("quiet_then_burst", p, horizon, events)


def periodic_bursts(horizon=3000):
    """Big mint every 100 blocks (~1 natural window worth of issuance
    each). ``r`` should ratchet through many retargets — watch whether
    it converges or oscillates."""
    p = DEMO_PARAMS
    events = [
        {"l1_height": h, "value_sat": 500_000_000, "lock_blocks": 1000}
        for h in range(100, horizon + 1, 100)
    ]
    return _make("periodic_bursts", p, horizon, events)


def whale_long_lock(horizon=5000):
    """A single very large, very long-locked mint at block 1, then
    silence. The mint_fn ratio approaches 1 for large rT, so atoms
    minted ≈ value_sat → blows past M_w by orders of magnitude in
    one block. Maximum-shrink retarget, then ``r`` is frozen for the
    remainder of the horizon."""
    p = DEMO_PARAMS
    events = [{"l1_height": 1, "value_sat": 10_000_000_000, "lock_blocks": 30_000}]
    return _make("whale_long_lock", p, horizon, events)


def high_volatility(horizon=5000, seed=42):
    """Randomised gap-then-cluster pattern with a fixed seed for
    reproducibility. Gaps drawn from [10, 200] blocks; bursts are 1–5
    mints of varying size and T. Stress-test for "weird patterns that
    blow up cumulative supply"."""
    rng = random.Random(seed)
    p = DEMO_PARAMS
    events = []
    h = 0
    while h < horizon:
        h += rng.randint(10, 200)
        if h >= horizon:
            break
        burst_n = rng.randint(1, 5)
        burst_v = rng.choice([50_000_000, 100_000_000, 200_000_000, 500_000_000])
        burst_t = rng.choice([500, 1000, 2000, 5000])
        for i in range(burst_n):
            if h + i >= horizon:
                break
            events.append({
                "l1_height": h + i,
                "value_sat": burst_v,
                "lock_blocks": burst_t,
            })
        h += burst_n
    return _make("high_volatility", p, horizon, events)


def adversarial_just_below_window(horizon=2000):
    """Bursts engineered to bring ``window_atoms`` just under M_w, then
    a small follow-up mint that pushes us over with a tiny Δ_actual.
    Drives the retarget toward its 1/2 floor every time. Hypothesis:
    if a depositor can time mints this way they can ratchet ``r`` to
    near-zero, suppressing future yield for everyone else."""
    p = DEMO_PARAMS
    events = []
    # First burst (block 1): 375M sat × T=1000 ≈ 99M atoms — just
    # under M_w. Then a tiny mint at block 3 pushes over the line
    # with Δ_actual = 2.
    events.append({"l1_height": 1, "value_sat": 375_000_000, "lock_blocks": 1000})
    events.append({"l1_height": 3, "value_sat": 5_000_000, "lock_blocks": 1000})
    # Repeat the pattern every 500 blocks (giving ``r`` time to drift
    # back via being idle — except it doesn't, because retargets only
    # fire on window completion, so each cycle just compounds).
    for h0 in range(500, horizon, 500):
        events.append({"l1_height": h0, "value_sat": 375_000_000, "lock_blocks": 1000})
        events.append({"l1_height": h0 + 2, "value_sat": 5_000_000, "lock_blocks": 1000})
    return _make("adversarial_just_below_window", p, horizon, events)


def mainnet_sparse(horizon=30_000):
    """Mainnet-scale parameters. One very large mint per natural-window
    cadence (4320 blocks ≈ 1 month at 10-min blocks). Demonstrates the
    same dynamics on the planned production constants."""
    p = MAINNET_PARAMS
    events = []
    for h in range(4320, horizon, 4320):
        # mint_fn(1e12 sat, T=26280, r=1/26280) ≈ 1e12 × 0.264 = 264e9
        # atoms — slightly above one M_w (216e9) so each mint cleanly
        # completes a window.
        events.append({
            "l1_height": h,
            "value_sat": 1_000_000_000_000,
            "lock_blocks": 26_280,
        })
    return _make("mainnet_sparse", p, horizon, events)


SCENARIOS = [
    steady_match_m_star(),
    under_target(),
    over_target(),
    quiet_then_burst(),
    periodic_bursts(),
    whale_long_lock(),
    high_volatility(),
    adversarial_just_below_window(),
    mainnet_sparse(),
]
