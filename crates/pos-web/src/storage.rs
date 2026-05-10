use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const WALLET_KEY: &str = "fm_pos_wallet";
const CONFIG_KEY: &str = "fm_pos_config";
const TRANSACTIONS_KEY: &str = "fm_pos_transactions";

/// Wallet: denomination_msats -> Vec<serialized OOBNotes string>
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Wallet {
    /// denomination in msats -> list of single-note OOBNotes strings
    pub notes: BTreeMap<u64, Vec<String>>,
}

impl Wallet {
    pub fn load() -> Self {
        get_json(WALLET_KEY).unwrap_or_default()
    }

    pub fn save(&self) {
        set_json(WALLET_KEY, self);
    }

    pub fn total_msats(&self) -> u64 {
        self.notes
            .iter()
            .map(|(denom, notes)| denom * notes.len() as u64)
            .sum()
    }

    /// Get a breakdown: denomination -> count
    pub fn denomination_counts(&self) -> BTreeMap<u64, usize> {
        self.notes
            .iter()
            .map(|(d, n)| (*d, n.len()))
            .filter(|(_, c)| *c > 0)
            .collect()
    }

    /// Smallest denomination currently in the wallet
    pub fn smallest_denomination(&self) -> Option<u64> {
        self.notes.iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(d, _)| *d)
            .next()
    }

    /// Check if any of the given notes are already in the wallet
    pub fn contains_any(&self, split: &BTreeMap<u64, Vec<String>>) -> bool {
        for (denom, new_notes) in split {
            if let Some(existing) = self.notes.get(denom) {
                for note in new_notes {
                    if existing.contains(note) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Add notes (already split by denomination)
    pub fn deposit(&mut self, split: BTreeMap<u64, Vec<String>>) {
        for (denom, notes) in split {
            self.notes.entry(denom).or_default().extend(notes);
        }
        self.save();
    }

    /// Withdraw exact amount in msats, returning the note strings.
    ///
    /// Uses binary decomposition: since denominations are powers of 2,
    /// we determine how many of each denomination we need from the
    /// binary representation of the target amount. If a needed denomination
    /// isn't available, we use multiple smaller ones (splitting down).
    /// If smaller ones aren't available either, we take one larger note
    /// and make change internally (splitting up then down).
    pub fn withdraw_exact(&mut self, amount_msats: u64) -> Option<Vec<String>> {
        // Build availability map: denom -> count available
        let mut available: BTreeMap<u64, usize> = BTreeMap::new();
        for (denom, notes) in &self.notes {
            if !notes.is_empty() {
                available.insert(*denom, notes.len());
            }
        }

        tracing::info!(
            amount_msats,
            available = ?available,
            "withdraw_exact: starting coin selection"
        );

        // Plan: how many of each denomination to withdraw
        let plan = match self.plan_withdrawal(amount_msats, &available) {
            Some(p) => p,
            None => {
                tracing::warn!(amount_msats, "withdraw_exact: no valid plan found");
                return None;
            }
        };

        let plan_total: u64 = plan.iter().map(|(d, c)| d * (*c as u64)).sum();
        tracing::info!(
            plan = ?plan,
            plan_total,
            delta = plan_total as i64 - amount_msats as i64,
            "withdraw_exact: plan selected"
        );

        // Execute the plan
        let mut selected = Vec::new();
        for (denom, count) in &plan {
            let notes = self.notes.get_mut(denom)?;
            for _ in 0..*count {
                selected.push(notes.pop()?);
            }
        }

        self.save();
        Some(selected)
    }

    /// Plan how many of each denomination to withdraw for a given amount.
    ///
    /// Go largest to smallest, take notes while remainder >= denom.
    /// If we can't hit exact amount, accept if delta <= 1 sat (under-change).
    /// Otherwise add one more of the smallest available tier, drop everything
    /// below it, accept if delta <= 1 sat (over-change). Fail otherwise.
    fn plan_withdrawal(
        &self,
        amount: u64,
        available: &BTreeMap<u64, usize>,
    ) -> Option<BTreeMap<u64, usize>> {
        const TOLERANCE: u64 = 1000; // 1 sat in msats

        let mut remaining = amount;
        let mut plan: BTreeMap<u64, usize> = BTreeMap::new();
        let mut avail = available.clone();

        // Pass 1: largest to smallest, take while remainder >= denom
        let denoms: Vec<u64> = avail.keys().rev().cloned().collect();
        for denom in &denoms {
            if remaining == 0 { break; }
            let have = avail.get(&denom).copied().unwrap_or(0);
            let use_n = (remaining / denom) as usize;
            let use_n = use_n.min(have);
            if use_n > 0 {
                tracing::debug!(denom, use_n, remaining, "plan: greedy take");
                *plan.entry(*denom).or_insert(0) += use_n;
                *avail.get_mut(denom).expect("have") -= use_n;
                remaining -= denom * use_n as u64;
            }
        }

        // Exact or close enough (gave slightly too little change, delta <= 1 sat)
        if remaining <= TOLERANCE {
            tracing::debug!(remaining, "plan: greedy sufficient");
            return Some(plan);
        }

        tracing::debug!(remaining, plan = ?plan, "plan: greedy insufficient, trying bump");

        // Pass 2: go back up, find smallest available tier, add one,
        // drop everything below it
        let smallest_available: Vec<u64> = avail.iter()
            .filter(|(_, count)| **count > 0)
            .map(|(d, _)| *d)
            .collect(); // already sorted ascending (BTreeMap)

        for bump_denom in &smallest_available {
            if *bump_denom <= remaining {
                continue; // too small, greedy would have taken it
            }
            // Add one of this denomination
            let mut new_plan = plan.clone();
            *new_plan.entry(*bump_denom).or_insert(0) += 1;

            // Remove all denominations below it from the plan
            let remove: Vec<u64> = new_plan.keys().filter(|d| **d < *bump_denom).cloned().collect();
            for d in &remove {
                new_plan.remove(d);
            }

            let total: u64 = new_plan.iter().map(|(d, c)| d * *c as u64).sum();
            let delta = total.saturating_sub(amount);

            tracing::debug!(bump_denom, delta, new_plan = ?new_plan, "plan: bump candidate");

            // Over-changed by <= 1 sat — acceptable
            if delta <= TOLERANCE {
                tracing::debug!(bump_denom, delta, "plan: bump accepted");
                return Some(new_plan);
            }
        }

        tracing::warn!(amount, remaining, "plan: no valid plan found");
        None
    }

    /// Withdraw up to amount, picking from smallest denominations first for change-giving.
    /// Returns (notes, total_withdrawn_msats). May overshoot slightly if exact not possible.
    pub fn withdraw_at_least(&mut self, amount_msats: u64) -> Option<Vec<String>> {
        // Try exact first
        let mut remaining = amount_msats;
        let mut selected = Vec::new();

        let denoms: Vec<u64> = self.notes.keys().rev().cloned().collect();

        for denom in denoms {
            if remaining == 0 {
                break;
            }
            if let Some(notes) = self.notes.get_mut(&denom) {
                while remaining >= denom && !notes.is_empty() {
                    if let Some(note) = notes.pop() {
                        selected.push(note);
                        remaining -= denom;
                    }
                }
            }
        }

        if remaining > 0 {
            // Need one more note of the smallest available denomination >= remaining
            let denoms: Vec<u64> = self.notes.keys().cloned().collect();
            let mut found = false;
            for denom in denoms {
                if denom >= remaining {
                    if let Some(notes) = self.notes.get_mut(&denom) {
                        if let Some(note) = notes.pop() {
                            selected.push(note);
                            found = true;
                            break;
                        }
                    }
                }
            }
            if !found {
                // Put notes back
                for s in &selected {
                    if let Ok(parsed) = crate::ecash::parse_notes(s) {
                        let msats = parsed.total_msats();
                        self.notes.entry(msats).or_default().push(s.clone());
                    }
                }
                return None;
            }
        }

        self.save();
        Some(selected)
    }

    /// Check for denomination gaps in the lowest 80% of present denominations
    pub fn has_change_gaps(&self) -> bool {
        let counts = self.denomination_counts();
        if counts.len() < 2 {
            return false;
        }

        let denoms: Vec<u64> = counts.keys().cloned().collect();
        let cutoff = (denoms.len() as f64 * 0.8).ceil() as usize;
        let low_denoms = &denoms[..cutoff.min(denoms.len())];

        // Check if any denomination in the low range has 0 notes
        // Also check for missing denominations between min and max of low range
        for d in low_denoms {
            if counts.get(d).copied().unwrap_or(0) == 0 {
                return true;
            }
        }

        // Check for standard fedimint denomination gaps
        // Standard denoms are powers: 1, 2, 4, 8, 16, ... (msat)
        // Actually in fedimint they're 1, 10, 100, 1000, 10000, ... (msat)
        if low_denoms.len() >= 2 {
            let all_known: Vec<u64> = denoms.clone();
            let max_low = low_denoms.last().copied().unwrap_or(0);
            // Check if we have any denom that's present in wallet history but currently at 0
            for d in &all_known {
                if *d <= max_low && counts.get(d).copied().unwrap_or(0) == 0 {
                    return true;
                }
            }
        }

        false
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PosConfig {
    pub federation_id: String,
    pub pin_hash: String,
    pub default_currency: String,
    /// Cached exchange rates: currency -> btc_per_unit
    pub cached_rates: BTreeMap<String, f64>,
    pub rates_timestamp: u64,
    /// Manually set exchange rate (sats per fiat unit), if any
    pub manual_rate: Option<(String, f64)>,
}

impl PosConfig {
    pub fn load() -> Option<Self> {
        get_json(CONFIG_KEY)
    }

    pub fn save(&self) {
        set_json(CONFIG_KEY, self);
    }
}

/// Simple PIN hashing (not cryptographic, just for local protection)
pub fn hash_pin(pin: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    pin.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub timestamp: u64,
    pub amount_msats: u64,
    pub currency: String,
    pub fiat_amount: f64,
    pub change_msats: u64,
    /// Amount actually paid by customer (for sales)
    #[serde(default)]
    pub paid_msats: u64,
    pub tx_type: TxType,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TxType {
    Sale,
    Deposit,
    Withdrawal,
}

impl std::fmt::Display for TxType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxType::Sale => write!(f, "Sale"),
            TxType::Deposit => write!(f, "Deposit"),
            TxType::Withdrawal => write!(f, "Withdrawal"),
        }
    }
}

pub fn load_transactions() -> Vec<Transaction> {
    get_json(TRANSACTIONS_KEY).unwrap_or_default()
}

pub fn save_transactions(txs: &[Transaction]) {
    set_json(TRANSACTIONS_KEY, &txs.to_vec());
}

pub fn add_transaction(tx: Transaction) {
    let mut txs = load_transactions();
    txs.push(tx);
    save_transactions(&txs);
}

fn get_json<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let val = storage.get_item(key).ok()??;
    serde_json::from_str(&val).ok()
}

fn set_json<T: serde::Serialize>(key: &str, value: &T) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    if let Ok(json) = serde_json::to_string(value) {
        let _ = storage.set_item(key, &json);
    }
}
