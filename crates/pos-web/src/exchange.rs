use std::collections::BTreeMap;

use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::window;

const PRICE_FEED_URL: &str = "https://price-feed.dev.fedibtc.com/latest";
const RATES_CACHE_KEY: &str = "fm_pos_rates_cache";
const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000; // 1 day

#[derive(Deserialize)]
struct PriceFeedResponse {
    prices: BTreeMap<String, PriceEntry>,
}

#[derive(Deserialize)]
struct PriceEntry {
    rate: f64,
}

/// Rates map: currency_code -> btc_per_unit (e.g. USD -> 0.0000125)
pub type RateMap = BTreeMap<String, f64>;

/// Fetch exchange rates from Fedi price feed
pub async fn fetch_rates() -> Result<RateMap, String> {
    let window = window().ok_or("No window")?;
    let resp_value = JsFuture::from(window.fetch_with_str(PRICE_FEED_URL))
        .await
        .map_err(|e| format!("Fetch error: {e:?}"))?;

    let resp: web_sys::Response = resp_value.dyn_into().map_err(|_| "Not a Response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let json = JsFuture::from(resp.json().map_err(|_| "json() failed")?)
        .await
        .map_err(|e| format!("JSON parse error: {e:?}"))?;

    let data: PriceFeedResponse =
        serde_wasm_bindgen::from_value(json).map_err(|e| format!("Deserialize error: {e}"))?;

    let btc_usd = data
        .prices
        .get("BTC/USD")
        .map(|p| p.rate)
        .ok_or("BTC/USD rate missing")?;

    let mut map: RateMap = BTreeMap::new();
    map.insert("sat".to_string(), 1.0 / 100_000_000.0);
    map.insert("USD".to_string(), 1.0 / btc_usd);

    for (key, entry) in &data.prices {
        if key == "BTC/USD" {
            continue;
        }
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() == 2 && parts[1] == "USD" && parts[0] != "USD" && parts[0] != "sat" {
            let btc_per_unit = entry.rate / btc_usd;
            map.insert(parts[0].to_string(), btc_per_unit);
        }
    }

    // Cache to localStorage
    cache_rates(&map);

    Ok(map)
}

/// Load cached rates from localStorage
pub fn load_cached_rates() -> Option<RateMap> {
    let window = window()?;
    let storage = window.local_storage().ok()??;
    let val = storage.get_item(RATES_CACHE_KEY).ok()??;

    #[derive(Deserialize)]
    struct CacheEntry {
        rates: RateMap,
        timestamp: u64,
    }

    let entry: CacheEntry = serde_json::from_str(&val).ok()?;
    let now = js_sys::Date::now() as u64;
    if now - entry.timestamp > CACHE_TTL_MS {
        return None;
    }
    Some(entry.rates)
}

fn cache_rates(rates: &RateMap) {
    let Some(window) = window() else { return };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };

    #[derive(serde::Serialize)]
    struct CacheEntry<'a> {
        rates: &'a RateMap,
        timestamp: u64,
    }

    let entry = CacheEntry {
        rates,
        timestamp: js_sys::Date::now() as u64,
    };

    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = storage.set_item(RATES_CACHE_KEY, &json);
    }
}

/// Convert fiat amount to msats given a rate map
pub fn fiat_to_msats(amount: f64, currency: &str, rates: &RateMap) -> Option<u64> {
    if currency == "sat" {
        return Some((amount * 1000.0) as u64);
    }
    let btc_per_unit = rates.get(currency)?;
    let btc = amount * btc_per_unit;
    let msats = (btc * 100_000_000_000.0) as u64;
    Some(msats)
}

/// Convert msats to fiat given a rate map
pub fn msats_to_fiat(msats: u64, currency: &str, rates: &RateMap) -> Option<f64> {
    if currency == "sat" {
        return Some(msats as f64 / 1000.0);
    }
    let btc_per_unit = rates.get(currency)?;
    if *btc_per_unit == 0.0 {
        return None;
    }
    let btc = msats as f64 / 100_000_000_000.0;
    Some(btc / btc_per_unit)
}
