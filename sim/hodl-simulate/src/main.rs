//! Drive `hodl_core::consensus::mint_fn` + the retargeting algorithm
//! from `LedgerState::end_of_block` over a synthetic L1 event stream
//! and emit a per-block trace.
//!
//! Reads scenario JSON from stdin, writes trace JSON to stdout. The
//! Python orchestrator in `sim/run.py` calls this binary once per
//! scenario.
//!
//! The retargeting math here is a re-implementation rather than a
//! direct call into `LedgerState::end_of_block`, because we want the
//! parameters (M_w, M*, max_factor) to be configurable per scenario
//! — the production constants are compile-time. A test in this crate
//! pins the re-implementation against `LedgerState::end_of_block`
//! using the production constants, so any future drift between the
//! two will fail CI.

use anyhow::{Context, Result};
use hodl_core::consensus::mint_fn;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

#[derive(Deserialize)]
struct Scenario {
    params: Params,
    horizon_l1_blocks: u32,
    events: Vec<Event>,
}

#[derive(Deserialize, Serialize, Clone)]
struct Params {
    initial_r: f64,
    target_atoms_per_block: u64,
    retarget_window_atoms: u64,
    retarget_max_factor: f64,
}

#[derive(Deserialize, Clone)]
struct Event {
    l1_height: u32,
    value_sat: u64,
    lock_blocks: u32,
}

#[derive(Serialize)]
struct Trace {
    params: Params,
    l1_height: Vec<u32>,
    minted_at_height: Vec<u64>,
    cumulative_atoms: Vec<u64>,
    current_r: Vec<f64>,
    window_atoms: Vec<u64>,
    retargets: Vec<RetargetEvent>,
}

#[derive(Serialize)]
struct RetargetEvent {
    l1_height: u32,
    old_r: f64,
    new_r: f64,
    ratio: f64,
    delta_blocks: u32,
    window_atoms_at_retarget: u64,
}

fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("read scenario from stdin")?;
    let scenario: Scenario = serde_json::from_str(&input).context("parse scenario JSON")?;
    let trace = simulate(&scenario);
    let out = serde_json::to_string(&trace).context("serialize trace")?;
    std::io::stdout()
        .write_all(out.as_bytes())
        .context("write trace to stdout")?;
    Ok(())
}

fn simulate(s: &Scenario) -> Trace {
    let mut r = s.params.initial_r;
    let mut window_atoms: u64 = 0;
    let mut window_start: Option<u32> = None;
    let mut cumulative: u64 = 0;
    let h_max = s.horizon_l1_blocks;

    // Pre-bucket events by L1 height. Cheap given we iterate the
    // whole horizon anyway; avoids quadratic event scans.
    let mut events = s.events.clone();
    events.sort_by_key(|e| e.l1_height);
    let mut event_cursor: usize = 0;

    let cap = h_max as usize + 1;
    let mut l1_heights = Vec::with_capacity(cap);
    let mut mints_per_block = Vec::with_capacity(cap);
    let mut cum = Vec::with_capacity(cap);
    let mut r_trace = Vec::with_capacity(cap);
    let mut window_trace = Vec::with_capacity(cap);
    let mut retargets = Vec::new();

    for h in 0..=h_max {
        // Apply mints in this L1 block, in input order. Each uses the
        // *pre-retarget* r — retargeting at this block's end-of-block
        // affects only future blocks, matching production.
        let mut block_mints: u64 = 0;
        while event_cursor < events.len() && events[event_cursor].l1_height == h {
            let ev = &events[event_cursor];
            let m = mint_fn(ev.value_sat, ev.lock_blocks, r);
            block_mints = block_mints.saturating_add(m);
            event_cursor += 1;
        }
        window_atoms = window_atoms.saturating_add(block_mints);
        cumulative = cumulative.saturating_add(block_mints);

        // Mirror LedgerState::end_of_block exactly. The early-return
        // structure matches the production code's bullet ordering.
        if h > 0 && window_atoms > 0 {
            if window_start.is_none() {
                window_start = Some(h);
            }
            if window_atoms >= s.params.retarget_window_atoms {
                let start = window_start.expect("set just above");
                let delta = h.saturating_sub(start);
                if delta > 0 {
                    let m_obs = window_atoms as f64 / delta as f64;
                    let m_star = s.params.target_atoms_per_block as f64;
                    let raw = m_star / m_obs;
                    let lo = 1.0 / s.params.retarget_max_factor;
                    let hi = s.params.retarget_max_factor;
                    let ratio = raw.max(lo).min(hi);
                    let old_r = r;
                    r *= ratio;
                    retargets.push(RetargetEvent {
                        l1_height: h,
                        old_r,
                        new_r: r,
                        ratio,
                        delta_blocks: delta,
                        window_atoms_at_retarget: window_atoms,
                    });
                    window_atoms = 0;
                    window_start = None;
                }
            }
        }

        l1_heights.push(h);
        mints_per_block.push(block_mints);
        cum.push(cumulative);
        r_trace.push(r);
        window_trace.push(window_atoms);
    }

    Trace {
        params: s.params.clone(),
        l1_height: l1_heights,
        minted_at_height: mints_per_block,
        cumulative_atoms: cum,
        current_r: r_trace,
        window_atoms: window_trace,
        retargets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodl_core::consensus::{
        INITIAL_R, RETARGET_MAX_FACTOR, RETARGET_MINT_WINDOW_ATOMS, TARGET_ATOMS_PER_BLOCK,
    };
    use hodl_core::state::LedgerState;

    /// The simulator's retargeting math must agree bit-for-bit with
    /// `LedgerState::end_of_block` when fed the production constants.
    /// We bypass `apply_mint` (which would require a witness/proof)
    /// by mutating `LedgerState`'s public fields directly — the
    /// `end_of_block` path only inspects `current_window_atoms` and
    /// `current_window_start_l1_height`, so the result is faithful.
    #[test]
    fn matches_ledger_state_end_of_block() {
        let events: Vec<Event> = (1..=250)
            .map(|h| Event {
                l1_height: h,
                value_sat: 100_000_000,
                lock_blocks: 1500,
            })
            .collect();
        let scenario = Scenario {
            params: Params {
                initial_r: INITIAL_R,
                target_atoms_per_block: TARGET_ATOMS_PER_BLOCK,
                retarget_window_atoms: RETARGET_MINT_WINDOW_ATOMS,
                retarget_max_factor: RETARGET_MAX_FACTOR,
            },
            horizon_l1_blocks: 300,
            events: events.clone(),
        };
        let trace = simulate(&scenario);

        // Drive LedgerState through the same events. Bypass apply_mint
        // (which needs a real witness) and just mutate the window
        // counters; only end_of_block reads them, so the result is
        // faithful.
        let mut state = LedgerState::default();
        for h in 0..=scenario.horizon_l1_blocks {
            let mut block_mints: u64 = 0;
            for ev in &events {
                if ev.l1_height == h {
                    block_mints =
                        block_mints.saturating_add(mint_fn(ev.value_sat, ev.lock_blocks, state.current_r));
                }
            }
            state.current_window_atoms = state.current_window_atoms.saturating_add(block_mints);
            state.total_minted_atoms = state.total_minted_atoms.saturating_add(block_mints);
            if h > 0 {
                state.end_of_block(h, h);
            }
        }

        let sim_final_r = *trace.current_r.last().unwrap();
        let sim_final_cum = *trace.cumulative_atoms.last().unwrap();
        assert!(
            (state.current_r - sim_final_r).abs() < 1e-12,
            "current_r drift: ledger={}, sim={}",
            state.current_r,
            sim_final_r,
        );
        assert_eq!(
            state.total_minted_atoms, sim_final_cum,
            "cumulative drift",
        );
    }

    /// Sanity-check the retargeting direction: a window completed
    /// faster than target ⇒ observed > target ⇒ r shrinks.
    #[test]
    fn fast_window_shrinks_r() {
        // Pack 100M atoms-worth of mints into the first 10 blocks.
        let events: Vec<Event> = (1..=10)
            .map(|h| Event {
                l1_height: h,
                value_sat: 1_000_000_000, // 10 BTC each
                lock_blocks: 5000,
            })
            .collect();
        let scenario = Scenario {
            params: Params {
                initial_r: 0.001,
                target_atoms_per_block: 1_000_000,
                retarget_window_atoms: 100_000_000,
                retarget_max_factor: 2.0,
            },
            horizon_l1_blocks: 50,
            events,
        };
        let trace = simulate(&scenario);
        assert!(!trace.retargets.is_empty(), "expected at least one retarget");
        let first = &trace.retargets[0];
        assert!(first.new_r < first.old_r,
            "fast-issued window must shrink r: old={}, new={}",
            first.old_r, first.new_r);
    }
}
