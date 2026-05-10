use std::collections::BTreeMap;

use leptos::html::Video;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::ecash;
use crate::exchange::{self, RateMap};
use crate::qr::{self, AnimatedQrCollector, ProcessResult};
use crate::scanner;
use crate::storage::{self, PosConfig, Transaction, TxType, Wallet};

#[derive(Clone, Debug, PartialEq)]
enum AppView {
    Setup(SetupStep),
    Pos,
    AwaitPayment(u64), // requested msats
    PaymentSuccess { received_sats: u64, change_sats: u64 },
    GiveChange { ecash_str: String, received_sats: u64, change_sats: u64 },
    PinEntry,
    Admin,
    AdminWithdraw(String),
}

#[derive(Clone, Debug, PartialEq)]
enum SetupStep {
    Federation,
    Pin,
    InitialDeposit,
    ScanDeposit,
}

const QR_CHUNK_SIZE: usize = 300;

#[component]
pub fn App() -> impl IntoView {
    let view = RwSignal::new(initial_view());
    let wallet = RwSignal::new(Wallet::load());
    let config = RwSignal::new(PosConfig::load());
    let rates = RwSignal::new(load_initial_rates());
    let error_msg = RwSignal::new(None::<String>);

    // Fetch rates on mount
    spawn_local(async move {
        if let Ok(new_rates) = exchange::fetch_rates().await {
            rates.set(new_rates);
        }
    });

    view! {
        <div class="pos-container">
            {move || {
                let current_view = view.get();
                match current_view {
                    AppView::Setup(step) => view! {
                        <SetupFlow step=step view=view config=config wallet=wallet />
                    }.into_any(),
                    AppView::Pos => view! {
                        <PosView view=view wallet=wallet config=config rates=rates error_msg=error_msg />
                    }.into_any(),
                    AppView::AwaitPayment(msats) => view! {
                        <AwaitPaymentView requested_msats=msats view=view wallet=wallet config=config />
                    }.into_any(),
                    AppView::PaymentSuccess { received_sats, change_sats } => view! {
                        <PaymentSuccessView received_sats=received_sats change_sats=change_sats view=view />
                    }.into_any(),
                    AppView::GiveChange { ref ecash_str, received_sats, change_sats } => {
                        let s = ecash_str.clone();
                        view! { <GiveChangeView ecash_str=s received_sats=received_sats change_sats=change_sats view=view /> }.into_any()
                    },
                    AppView::PinEntry => view! {
                        <PinEntryView view=view config=config />
                    }.into_any(),
                    AppView::Admin => view! {
                        <AdminView view=view wallet=wallet config=config rates=rates />
                    }.into_any(),
                    AppView::AdminWithdraw(ref ecash_str) => {
                        let s = ecash_str.clone();
                        view! { <AnimatedQrView title="Scan to withdraw".to_string() ecash_str=s view=view /> }.into_any()
                    },
                }
            }}
            {move || error_msg.get().map(|msg| view! {
                <div class="warning-banner" style="background: #fee2e2; border-color: #ef4444; color: #991b1b;">
                    {msg}
                    <button style="margin-left: 0.5rem; font-weight: bold;" on:click=move |_| error_msg.set(None)>"x"</button>
                </div>
            })}
        </div>
    }
}

fn initial_view() -> AppView {
    if PosConfig::load().is_some() {
        AppView::Pos
    } else {
        AppView::Setup(SetupStep::Federation)
    }
}

fn load_initial_rates() -> RateMap {
    exchange::load_cached_rates().unwrap_or_else(|| {
        let mut m = BTreeMap::new();
        m.insert("sat".to_string(), 1.0 / 100_000_000.0);
        m
    })
}

// ─── Setup Flow ───────────────────────────────────────────────

#[component]
fn SetupFlow(
    step: SetupStep,
    view: RwSignal<AppView>,
    config: RwSignal<Option<PosConfig>>,
    wallet: RwSignal<Wallet>,
) -> impl IntoView {
    match step {
        SetupStep::Federation => view! { <SetupFederation view=view config=config /> }.into_any(),
        SetupStep::Pin => view! { <SetupPin view=view config=config /> }.into_any(),
        SetupStep::InitialDeposit => {
            view! { <SetupDeposit view=view wallet=wallet /> }.into_any()
        }
        SetupStep::ScanDeposit => {
            view! { <ScanDepositView view=view config=config wallet=wallet /> }.into_any()
        }
    }
}

#[component]
fn SetupFederation(view: RwSignal<AppView>, config: RwSignal<Option<PosConfig>>) -> impl IntoView {
    let invite_input = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let scanning = RwSignal::new(false);
    let video_ref = NodeRef::<Video>::new();

    let on_submit = move || {
        let input = invite_input.get();
        match ecash::parse_invite_code(&input) {
            Ok(fed_id) => {
                config.set(Some(PosConfig {
                    federation_id: hex::encode(fed_id),
                    pin_hash: String::new(),
                    default_currency: "USD".to_string(),
                    cached_rates: BTreeMap::new(),
                    rates_timestamp: 0,
                    manual_rate: None,
                }));
                view.set(AppView::Setup(SetupStep::Pin));
            }
            Err(e) => error.set(Some(e)),
        }
    };

    let start_scan = move |_| {
        scanning.set(true);
        let video_ref_clone = video_ref;
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(200).await;
            if let Some(video) = video_ref_clone.get() {
                match scanner::start_camera(&video).await {
                    Ok(stream) => {
                        loop {
                            gloo_timers::future::TimeoutFuture::new(300).await;
                            if !scanning.get_untracked() {
                                scanner::stop_camera(&stream);
                                break;
                            }
                            if let Some(data) = scanner::scan_frame(&video).await {
                                scanner::stop_camera(&stream);
                                scanning.set(false);
                                invite_input.set(data.clone());
                                if let Ok(fed_id) = ecash::parse_invite_code(&data) {
                                    config.set(Some(PosConfig {
                                        federation_id: hex::encode(fed_id),
                                        pin_hash: String::new(),
                                        default_currency: "USD".to_string(),
                                        cached_rates: BTreeMap::new(),
                                        rates_timestamp: 0,
                                        manual_rate: None,
                                    }));
                                    view.set(AppView::Setup(SetupStep::Pin));
                                }
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(e));
                        scanning.set(false);
                    }
                }
            }
        });
    };

    view! {
        <div class="setup-card">
            <h2>"Setup: Federation"</h2>
            <p style="color: #6b7280; margin-bottom: 1rem; font-size: 0.875rem;">
                "Scan or paste a federation invite code to get started."
            </p>

            {move || if scanning.get() {
                view! {
                    <div class="scanner-container">
                        <video node_ref=video_ref autoplay=true playsinline=true
                            style="width: 100%; max-height: 70vh; object-fit: cover;"></video>
                        <div class="scanner-overlay">
                            <button class="btn btn-gray" on:click=move |_| scanning.set(false)>
                                "Cancel"
                            </button>
                        </div>
                    </div>
                }.into_any()
            } else {
                view! {
                    <div>
                        <textarea
                            rows=3
                            placeholder="Paste invite code here..."
                            style="margin-bottom: 0.75rem; font-size: 0.75rem;"
                            prop:value=move || invite_input.get()
                            on:input=move |ev| {
                                invite_input.set(event_target_value(&ev));
                            }
                        ></textarea>
                        <div style="display: flex; gap: 0.5rem;">
                            <button class="btn btn-blue" style="flex: 1;" on:click=move |_| on_submit()>
                                "Confirm"
                            </button>
                            <button class="btn btn-gray" style="flex: 1;" on:click=start_scan>
                                "Scan QR"
                            </button>
                        </div>
                    </div>
                }.into_any()
            }}

            {move || error.get().map(|e| view! {
                <div class="warning-banner" style="margin-top: 0.75rem;">{e}</div>
            })}
        </div>
    }
}

#[component]
fn SetupPin(view: RwSignal<AppView>, config: RwSignal<Option<PosConfig>>) -> impl IntoView {
    let pin = RwSignal::new(String::new());
    let confirm_pin = RwSignal::new(String::new());
    let stage = RwSignal::new(0u8); // 0 = enter, 1 = confirm
    let error = RwSignal::new(None::<String>);

    let on_digit = move |d: char| {
        if stage.get() == 0 {
            pin.update(|p| { if p.len() < 6 { p.push(d); } });
        } else {
            confirm_pin.update(|p| { if p.len() < 6 { p.push(d); } });
        }
    };

    let on_backspace = move || {
        if stage.get() == 0 {
            pin.update(|p| { p.pop(); });
        } else {
            confirm_pin.update(|p| { p.pop(); });
        }
    };

    let on_confirm = move || {
        if stage.get() == 0 {
            if pin.get().len() < 4 {
                error.set(Some("PIN must be at least 4 digits".to_string()));
                return;
            }
            stage.set(1);
            error.set(None);
        } else {
            if pin.get() != confirm_pin.get() {
                error.set(Some("PINs don't match".to_string()));
                confirm_pin.set(String::new());
                return;
            }
            config.update(|c| {
                if let Some(cfg) = c {
                    cfg.pin_hash = storage::hash_pin(&pin.get());
                    cfg.save();
                }
            });
            view.set(AppView::Setup(SetupStep::InitialDeposit));
        }
    };

    view! {
        <div class="setup-card">
            <h2>{move || if stage.get() == 0 { "Set Admin PIN" } else { "Confirm PIN" }}</h2>
            <div class="pin-input">
                {move || {
                    let current = if stage.get() == 0 { pin.get() } else { confirm_pin.get() };
                    (0..6).map(|i| {
                        let filled = i < current.len();
                        view! { <div class={if filled { "pin-dot filled" } else { "pin-dot" }}></div> }
                    }).collect::<Vec<_>>()
                }}
            </div>
            <PinNumpad on_digit=on_digit on_backspace=on_backspace on_confirm=on_confirm />
            {move || error.get().map(|e| view! {
                <div class="warning-banner" style="margin-top: 0.75rem;">{e}</div>
            })}
        </div>
    }
}

#[component]
fn SetupDeposit(view: RwSignal<AppView>, wallet: RwSignal<Wallet>) -> impl IntoView {
    view! {
        <div class="setup-card">
            <h2>"Deposit Change"</h2>
            <p style="color: #6b7280; margin-bottom: 1rem; font-size: 0.875rem;">
                "Deposit ecash for making change. You can always do this later from admin."
            </p>
            <button class="btn btn-blue" style="margin-bottom: 0.5rem;"
                on:click=move |_| view.set(AppView::Setup(SetupStep::ScanDeposit))>
                "Deposit 10,000 sats"
            </button>
            <button class="btn btn-blue" style="margin-bottom: 0.5rem;"
                on:click=move |_| view.set(AppView::Setup(SetupStep::ScanDeposit))>
                "Deposit 50,000 sats"
            </button>
            <button class="btn btn-blue" style="margin-bottom: 0.5rem;"
                on:click=move |_| view.set(AppView::Setup(SetupStep::ScanDeposit))>
                "Deposit 100,000 sats"
            </button>
            <button class="btn btn-gray" style="margin-bottom: 0.5rem;"
                on:click=move |_| view.set(AppView::Setup(SetupStep::ScanDeposit))>
                "Custom amount"
            </button>
            <button class="btn btn-gray" on:click=move |_| view.set(AppView::Pos)>
                "Skip for now"
            </button>
        </div>
    }
}

#[component]
fn ScanDepositView(
    view: RwSignal<AppView>,
    config: RwSignal<Option<PosConfig>>,
    wallet: RwSignal<Wallet>,
) -> impl IntoView {
    let scanning = RwSignal::new(false);
    let paste_input = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let progress = RwSignal::new(0.0f64);
    let video_ref = NodeRef::<Video>::new();
    let collector = RwSignal::new(AnimatedQrCollector::new());

    let process_ecash = move |data: String| {
        let cfg = config.get();
        let Some(cfg) = cfg else {
            error.set(Some("No config".to_string()));
            return;
        };
        let expected_prefix = match get_federation_prefix(&cfg) {
            Ok(p) => p,
            Err(e) => { error.set(Some(e)); return; }
        };

        match ecash::parse_notes(&data) {
            Ok(notes) => {
                if !ecash::check_federation(&notes, &expected_prefix) {
                    error.set(Some("Wrong federation!".to_string()));
                    return;
                }
                let total = notes.total_msats();
                let split = ecash::split_notes_by_denomination(&notes);
                if wallet.get().contains_any(&split) {
                    error.set(Some("These notes are already in the wallet!".to_string()));
                    return;
                }
                wallet.update(|w| w.deposit(split));
                storage::add_transaction(Transaction {
                    timestamp: js_sys::Date::now() as u64,
                    amount_msats: total,
                    currency: "sat".to_string(),
                    fiat_amount: total as f64 / 1000.0,
                    change_msats: 0,
                    tx_type: TxType::Deposit,
                });
                view.set(AppView::Pos);
            }
            Err(e) => error.set(Some(e)),
        }
    };

    let process_ecash_scan = process_ecash.clone();

    let start_scan = move |_| {
        scanning.set(true);
        collector.update(|c| c.reset());
        let video_ref_clone = video_ref;
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(200).await;
            if let Some(video) = video_ref_clone.get() {
                match scanner::start_camera(&video).await {
                    Ok(stream) => {
                        loop {
                            gloo_timers::future::TimeoutFuture::new(200).await;
                            if !scanning.get_untracked() {
                                scanner::stop_camera(&stream);
                                break;
                            }
                            if let Some(raw) = scanner::scan_frame(&video).await {
                                // Try direct ecash parse first (static QR)
                                if ecash::parse_notes(&raw).is_ok() {
                                    scanner::stop_camera(&stream);
                                    scanning.set(false);
                                    process_ecash_scan(raw);
                                    break;
                                }
                                // Try as qrloop animated frame
                                let result = collector.try_update(|c| c.process_scan(&raw));
                                match result {
                                    Some(ProcessResult::Complete(data)) => {
                                        scanner::stop_camera(&stream);
                                        scanning.set(false);
                                        process_ecash_scan(data);
                                        break;
                                    }
                                    Some(ProcessResult::Progress(p)) => {
                                        progress.set(p);
                                    }
                                    _ => {} // NotAFrame or None — ignore
                                }
                            }
                        }
                    }
                    Err(e) => { error.set(Some(e)); scanning.set(false); }
                }
            }
        });
    };

    let on_paste_submit = move |_| {
        let data = paste_input.get();
        if !data.is_empty() { process_ecash(data); }
    };

    view! {
        <div class="setup-card">
            <h2>"Scan Ecash"</h2>
            {move || if scanning.get() {
                view! {
                    <div class="scanner-container">
                        <video node_ref=video_ref autoplay=true playsinline=true
                            style="width: 100%; max-height: 70vh; object-fit: cover;"></video>
                        <div style="position: absolute; top: 1rem; left: 1rem; right: 1rem;">
                            <div class="progress-bar">
                                <div class="progress-bar-fill" style=move || format!("width: {}%", progress.get() * 100.0)></div>
                            </div>
                        </div>
                        <div class="scanner-overlay">
                            <button class="btn btn-gray" on:click=move |_| scanning.set(false)>"Cancel"</button>
                        </div>
                    </div>
                }.into_any()
            } else {
                view! {
                    <div>
                        <textarea rows=3 placeholder="Paste ecash notes here..."
                            style="margin-bottom: 0.75rem; font-size: 0.75rem;"
                            prop:value=move || paste_input.get()
                            on:input=move |ev| paste_input.set(event_target_value(&ev))
                        ></textarea>
                        <div style="display: flex; gap: 0.5rem;">
                            <button class="btn btn-blue" style="flex: 1;" on:click=on_paste_submit>"Submit"</button>
                            <button class="btn btn-gray" style="flex: 1;" on:click=start_scan>"Scan QR"</button>
                        </div>
                        <button class="btn btn-gray" style="margin-top: 0.5rem;" on:click=move |_| view.set(AppView::Pos)>"Skip"</button>
                    </div>
                }.into_any()
            }}
            {move || error.get().map(|e| view! {
                <div class="warning-banner" style="margin-top: 0.75rem;">{e}</div>
            })}
        </div>
    }
}

// ─── PoS View ───────────────────────────────────────────────────

#[component]
fn PosView(
    view: RwSignal<AppView>,
    wallet: RwSignal<Wallet>,
    config: RwSignal<Option<PosConfig>>,
    rates: RwSignal<RateMap>,
    error_msg: RwSignal<Option<String>>,
) -> impl IntoView {
    let amount_str = RwSignal::new(String::new());
    let currency = RwSignal::new(
        config.get().map(|c| c.default_currency.clone()).unwrap_or("sat".to_string())
    );

    let sats_equiv = move || {
        let amt: f64 = amount_str.get().parse().unwrap_or(0.0);
        exchange::fiat_to_msats(amt, &currency.get(), &rates.get()).unwrap_or(0) / 1000
    };

    let handle_numpad = move |val: &str| {
        amount_str.update(|s| {
            match val {
                "." => { if !s.contains('.') && !s.is_empty() { s.push('.'); } }
                "\u{232b}" => { s.pop(); }
                d => {
                    if s.len() >= 10 { return; }
                    if s == "0" && d != "." { return; }
                    if s.contains('.') {
                        if let Some(dec) = s.split('.').nth(1) {
                            if dec.len() >= 2 { return; }
                        }
                    }
                    s.push_str(d);
                }
            }
        });
    };

    let on_charge = move |_| {
        let amt: f64 = amount_str.get().parse().unwrap_or(0.0);
        if amt <= 0.0 { return; }
        if let Some(msats) = exchange::fiat_to_msats(amt, &currency.get(), &rates.get()) {
            view.set(AppView::AwaitPayment(msats));
        }
    };

    let has_change_gap = move || wallet.get().has_change_gaps();
    let currencies = move || {
        let mut c: Vec<String> = rates.get().keys().cloned().collect();
        if !c.contains(&"sat".to_string()) { c.insert(0, "sat".to_string()); }
        c
    };

    view! {
        <div>
            {move || has_change_gap().then(|| view! {
                <div class="warning-banner">"Low on small change! Deposit more ecash from admin."</div>
            })}
            <div class="amount-display">
                <div class="amount">{move || { let a = amount_str.get(); if a.is_empty() { "0".to_string() } else { a } }}</div>
                <div class="currency">{move || currency.get()}</div>
                <div class="sats-equiv">{move || format!("\u{2248} {} sats", sats_equiv())}</div>
            </div>
            <div class="currency-selector">
                <select prop:value=move || currency.get() on:change=move |ev| currency.set(event_target_value(&ev))>
                    {move || currencies().into_iter().map(|c| { let v = c.clone(); view! { <option value={v}>{c}</option> } }).collect::<Vec<_>>()}
                </select>
            </div>
            <div class="numpad-grid">
                {["1","2","3","4","5","6","7","8","9",".","0","\u{232b}"].iter().map(|&b| {
                    let b_owned = b.to_string();
                    let display = if b == "\u{232b}" {
                        view! { <span style="font-size: 1.2rem;">{"\u{232b}"}</span> }.into_any()
                    } else {
                        view! { <span>{b.to_string()}</span> }.into_any()
                    };
                    view! {
                        <button type="button" class="numpad-btn" on:click=move |_| handle_numpad(&b_owned)>{display}</button>
                    }
                }).collect::<Vec<_>>()}
            </div>
            <button class="btn btn-blue" style="margin-top: 0.5rem;" on:click=on_charge>"Charge"</button>
            <div style="text-align: center; margin-top: 1rem;">
                <button style="background: none; border: none; color: #6b7280; font-size: 0.75rem; cursor: pointer;"
                    on:click=move |_| view.set(AppView::PinEntry)>"Admin"</button>
            </div>
        </div>
    }
}

// ─── Await Payment ──────────────────────────────────────────────

#[component]
fn AwaitPaymentView(
    requested_msats: u64,
    view: RwSignal<AppView>,
    wallet: RwSignal<Wallet>,
    config: RwSignal<Option<PosConfig>>,
) -> impl IntoView {
    let scanning = RwSignal::new(false);
    let paste_input = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let progress = RwSignal::new(0.0f64);
    let video_ref = NodeRef::<Video>::new();
    let collector = RwSignal::new(AnimatedQrCollector::new());

    let process_payment = move |data: String| {
        let cfg = config.get();
        let Some(cfg) = cfg else { error.set(Some("No config".to_string())); return; };
        let expected_prefix = match get_federation_prefix(&cfg) {
            Ok(p) => p,
            Err(e) => { error.set(Some(e)); return; }
        };

        match ecash::parse_notes(&data) {
            Ok(notes) => {
                if !ecash::check_federation(&notes, &expected_prefix) {
                    error.set(Some("Wrong federation! Ecash rejected.".to_string()));
                    return;
                }
                let received_msats = notes.total_msats();
                let min_acceptable = (requested_msats as f64 * 0.99) as u64;

                if received_msats < min_acceptable {
                    error.set(Some(format!(
                        "Received {} sats but expected {} sats. Too low!",
                        received_msats / 1000, requested_msats / 1000
                    )));
                    return;
                }

                // Deposit received notes
                let split = ecash::split_notes_by_denomination(&notes);
                if wallet.get().contains_any(&split) {
                    error.set(Some("These notes are already in the wallet! Possible double-spend attempt.".to_string()));
                    return;
                }
                wallet.update(|w| w.deposit(split));

                let change_msats = received_msats.saturating_sub(requested_msats);

                storage::add_transaction(Transaction {
                    timestamp: js_sys::Date::now() as u64,
                    amount_msats: requested_msats,
                    currency: "sat".to_string(),
                    fiat_amount: requested_msats as f64 / 1000.0,
                    change_msats,
                    tx_type: TxType::Sale,
                });

                if change_msats > 0 {
                    let mut w = wallet.get();
                    if let Some(change_notes) = w.withdraw_exact(change_msats) {
                        wallet.set(w);
                        match ecash::combine_note_strings(&change_notes) {
                            Ok(ecash_str) => {
                                view.set(AppView::GiveChange {
                                    ecash_str,
                                    received_sats: received_msats / 1000,
                                    change_sats: change_msats / 1000,
                                });
                                return;
                            }
                            Err(_) => {}
                        }
                    }
                    error.set(Some(format!("Payment accepted! Could not give exact change of {} sats.", change_msats / 1000)));
                }
                view.set(AppView::PaymentSuccess {
                    received_sats: received_msats / 1000,
                    change_sats: 0,
                });
            }
            Err(e) => error.set(Some(e)),
        }
    };

    let process_payment_scan = process_payment.clone();

    let start_scan = move |_| {
        scanning.set(true);
        collector.update(|c| c.reset());
        let video_ref_clone = video_ref;
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(200).await;
            if let Some(video) = video_ref_clone.get() {
                match scanner::start_camera(&video).await {
                    Ok(stream) => {
                        loop {
                            gloo_timers::future::TimeoutFuture::new(200).await;
                            if !scanning.get_untracked() {
                                scanner::stop_camera(&stream);
                                break;
                            }
                            if let Some(raw) = scanner::scan_frame(&video).await {
                                // Try direct ecash parse first (static QR)
                                if ecash::parse_notes(&raw).is_ok() {
                                    scanner::stop_camera(&stream);
                                    scanning.set(false);
                                    process_payment_scan(raw);
                                    break;
                                }
                                // Try as qrloop animated frame
                                let result = collector.try_update(|c| c.process_scan(&raw));
                                match result {
                                    Some(ProcessResult::Complete(data)) => {
                                        scanner::stop_camera(&stream);
                                        scanning.set(false);
                                        process_payment_scan(data);
                                        break;
                                    }
                                    Some(ProcessResult::Progress(p)) => {
                                        progress.set(p);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Err(e) => { error.set(Some(e)); scanning.set(false); }
                }
            }
        });
    };

    let on_paste = move |_| {
        let data = paste_input.get();
        if !data.is_empty() { process_payment(data); }
    };

    view! {
        <div class="setup-card">
            <h2>"Awaiting Payment"</h2>
            <p style="color: #6b7280; margin-bottom: 1rem; font-size: 0.875rem;">
                {format!("Expecting ~{} sats", requested_msats / 1000)}
            </p>
            {move || if scanning.get() {
                view! {
                    <div class="scanner-container">
                        <video node_ref=video_ref autoplay=true playsinline=true
                            style="width: 100%; max-height: 70vh; object-fit: cover;"></video>
                        <div style="position: absolute; top: 1rem; left: 1rem; right: 1rem;">
                            <div class="progress-bar">
                                <div class="progress-bar-fill" style=move || format!("width: {}%", progress.get() * 100.0)></div>
                            </div>
                        </div>
                        <div class="scanner-overlay">
                            <button class="btn btn-gray" on:click=move |_| scanning.set(false)>"Cancel"</button>
                        </div>
                    </div>
                }.into_any()
            } else {
                view! {
                    <div>
                        <textarea rows=3 placeholder="Paste ecash here..."
                            style="margin-bottom: 0.75rem; font-size: 0.75rem;"
                            prop:value=move || paste_input.get()
                            on:input=move |ev| paste_input.set(event_target_value(&ev))
                        ></textarea>
                        <div style="display: flex; gap: 0.5rem;">
                            <button class="btn btn-blue" style="flex: 1;" on:click=on_paste>"Submit"</button>
                            <button class="btn btn-gray" style="flex: 1;" on:click=start_scan>"Scan QR"</button>
                        </div>
                    </div>
                }.into_any()
            }}
            <button class="btn btn-gray" style="margin-top: 0.75rem;" on:click=move |_| view.set(AppView::Pos)>"Cancel"</button>
            {move || error.get().map(|e| view! {
                <div class="warning-banner" style="margin-top: 0.75rem;">{e}</div>
            })}
        </div>
    }
}

// ─── Animated QR View ───────────────────────────────────────────

#[component]
fn AnimatedQrView(title: String, ecash_str: String, view: RwSignal<AppView>) -> impl IntoView {
    let frames = qr::split_for_animated_qr(&ecash_str, QR_CHUNK_SIZE);
    let frame_count = frames.len();
    let current_frame = RwSignal::new(0usize);
    let frames_signal = RwSignal::new(frames);

    if frame_count > 1 {
        spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(250).await;
                current_frame.update(|f| *f = (*f + 1) % frame_count);
            }
        });
    }

    view! {
        <div class="setup-card" style="text-align: center;">
            <h2>{title}</h2>
            <div class="qr-container">
                {move || {
                    let idx = current_frame.get();
                    let fs = frames_signal.get();
                    let data = &fs[idx % fs.len()];
                    match qr::generate_qr_svg(data) {
                        Ok(svg) => view! { <div inner_html=svg></div> }.into_any(),
                        Err(e) => view! { <p style="color: red;">{e}</p> }.into_any(),
                    }
                }}
            </div>
            {(frame_count > 1).then(|| view! {
                <div class="progress-bar">
                    <div class="progress-bar-fill"
                        style=move || format!("width: {}%", ((current_frame.get() + 1) as f64 / frame_count as f64) * 100.0)>
                    </div>
                </div>
                <p style="font-size: 0.75rem; color: #6b7280;">
                    {move || format!("Frame {}/{}", current_frame.get() + 1, frame_count)}
                </p>
            })}
            <button class="btn btn-blue" style="margin-top: 1rem;" on:click=move |_| view.set(AppView::Pos)>"Done"</button>
        </div>
    }
}

// ─── Payment Success ────────────────────────────────────────────

#[component]
fn PaymentSuccessView(received_sats: u64, change_sats: u64, view: RwSignal<AppView>) -> impl IntoView {
    view! {
        <div class="setup-card" style="text-align: center;">
            <div style="font-size: 3rem; margin-bottom: 0.5rem;">{"\u{2705}"}</div>
            <h2 style="color: #059669;">"Payment Received!"</h2>
            <p style="font-size: 1.5rem; font-weight: 700; margin: 1rem 0;">
                {format!("{} sats", received_sats)}
            </p>
            {(change_sats > 0).then(|| view! {
                <p style="color: #6b7280; font-size: 0.875rem;">
                    {format!("Change: {} sats", change_sats)}
                </p>
            })}
            <button class="btn btn-blue" style="margin-top: 1.5rem;" on:click=move |_| view.set(AppView::Pos)>
                "New Sale"
            </button>
        </div>
    }
}

// ─── Give Change ────────────────────────────────────────────────

#[component]
fn GiveChangeView(ecash_str: String, received_sats: u64, change_sats: u64, view: RwSignal<AppView>) -> impl IntoView {
    let frames = qr::split_for_animated_qr(&ecash_str, QR_CHUNK_SIZE);
    let frame_count = frames.len();
    let current_frame = RwSignal::new(0usize);
    let frames_signal = RwSignal::new(frames);

    if frame_count > 1 {
        spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(250).await;
                current_frame.update(|f| *f = (*f + 1) % frame_count);
            }
        });
    }

    view! {
        <div class="setup-card" style="text-align: center;">
            <h2 style="color: #059669;">"Payment Received!"</h2>
            <p style="font-size: 1.25rem; font-weight: 700; margin: 0.5rem 0;">
                {format!("{} sats", received_sats)}
            </p>
            <div class="warning-banner" style="background: #dbeafe; border-color: #3b82f6; color: #1e40af;">
                {format!("Customer change: {} sats", change_sats)}
            </div>
            <p style="color: #6b7280; font-size: 0.875rem; margin-bottom: 0.5rem;">
                "Customer: scan this QR to receive your change"
            </p>
            <div class="qr-container">
                {move || {
                    let idx = current_frame.get();
                    let fs = frames_signal.get();
                    let data = &fs[idx % fs.len()];
                    match qr::generate_qr_svg(data) {
                        Ok(svg) => view! { <div inner_html=svg></div> }.into_any(),
                        Err(e) => view! { <p style="color: red;">{e}</p> }.into_any(),
                    }
                }}
            </div>
            {(frame_count > 1).then(|| view! {
                <div class="progress-bar">
                    <div class="progress-bar-fill"
                        style=move || format!("width: {}%", ((current_frame.get() + 1) as f64 / frame_count as f64) * 100.0)>
                    </div>
                </div>
            })}
            <button class="btn btn-blue" style="margin-top: 1rem;" on:click=move |_| view.set(AppView::Pos)>
                "Done"
            </button>
        </div>
    }
}

// ─── PIN Entry ──────────────────────────────────────────────────

#[component]
fn PinEntryView(view: RwSignal<AppView>, config: RwSignal<Option<PosConfig>>) -> impl IntoView {
    let pin = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    let on_digit = move |d: char| {
        pin.update(|p| { if p.len() < 6 { p.push(d); } });
    };
    let on_backspace = move || { pin.update(|p| { p.pop(); }); };
    let on_confirm = move || {
        if let Some(cfg) = config.get() {
            if storage::hash_pin(&pin.get()) == cfg.pin_hash {
                view.set(AppView::Admin);
            } else {
                error.set(Some("Wrong PIN".to_string()));
                pin.set(String::new());
            }
        }
    };

    view! {
        <div class="setup-card">
            <h2>"Enter Admin PIN"</h2>
            <div class="pin-input">
                {move || {
                    let current = pin.get();
                    (0..6).map(|i| {
                        let filled = i < current.len();
                        view! { <div class={if filled { "pin-dot filled" } else { "pin-dot" }}></div> }
                    }).collect::<Vec<_>>()
                }}
            </div>
            <PinNumpad on_digit=on_digit on_backspace=on_backspace on_confirm=on_confirm />
            <button class="btn btn-gray" style="margin-top: 0.75rem;" on:click=move |_| view.set(AppView::Pos)>"Cancel"</button>
            {move || error.get().map(|e| view! {
                <div class="warning-banner" style="margin-top: 0.75rem;">{e}</div>
            })}
        </div>
    }
}

// ─── Admin View ─────────────────────────────────────────────────

#[component]
fn AdminView(
    view: RwSignal<AppView>,
    wallet: RwSignal<Wallet>,
    config: RwSignal<Option<PosConfig>>,
    rates: RwSignal<RateMap>,
) -> impl IntoView {
    let leave_change = RwSignal::new("10000".to_string());

    let balance_sats = move || wallet.get().total_msats() / 1000;
    let denomination_info = move || {
        wallet.get().denomination_counts().iter()
            .map(|(d, c)| format!("{} msat x{}", d, c))
            .collect::<Vec<_>>().join(", ")
    };

    let transactions = move || {
        let txs = storage::load_transactions();
        let mut grouped: BTreeMap<String, Vec<Transaction>> = BTreeMap::new();
        for tx in txs.into_iter().rev() {
            grouped.entry(format_date(tx.timestamp)).or_default().push(tx);
        }
        grouped
    };

    let on_withdraw = move |_| {
        let leave_msats = leave_change.get().parse::<u64>().unwrap_or(10_000) * 1000;
        let total = wallet.get().total_msats();
        if total <= leave_msats { return; }
        let withdraw_msats = total - leave_msats;

        let mut w = wallet.get();
        if let Some(notes) = w.withdraw_exact(withdraw_msats) {
            wallet.set(w);
            if let Ok(ecash_str) = ecash::combine_note_strings(&notes) {
                storage::add_transaction(Transaction {
                    timestamp: js_sys::Date::now() as u64,
                    amount_msats: withdraw_msats,
                    currency: "sat".to_string(),
                    fiat_amount: withdraw_msats as f64 / 1000.0,
                    change_msats: 0,
                    tx_type: TxType::Withdrawal,
                });
                view.set(AppView::AdminWithdraw(ecash_str));
            }
        }
    };

    view! {
        <div>
            <div class="admin-section">
                <h2 style="font-size: 1.25rem; font-weight: 700; margin-bottom: 0.5rem;">"Balance"</h2>
                <p style="font-size: 2rem; font-weight: 700;">{move || format!("{} sats", balance_sats())}</p>
                <p style="font-size: 0.75rem; color: #6b7280; margin-top: 0.25rem;">{move || denomination_info()}</p>
            </div>

            {move || wallet.get().has_change_gaps().then(|| view! {
                <div class="warning-banner">"Low on small change denominations!"</div>
            })}

            <div class="admin-section">
                <h2 style="font-size: 1rem; font-weight: 600; margin-bottom: 0.5rem;">"Withdraw"</h2>
                <div style="display: flex; gap: 0.5rem; align-items: center; margin-bottom: 0.5rem;">
                    <label style="font-size: 0.875rem; white-space: nowrap;">"Leave (sats):"</label>
                    <input type="number" prop:value=move || leave_change.get()
                        on:input=move |ev| leave_change.set(event_target_value(&ev)) style="flex: 1;" />
                </div>
                <button class="btn btn-blue" on:click=on_withdraw>"Withdraw"</button>
                <button class="btn btn-gray" style="margin-top: 0.5rem;"
                    on:click=move |_| view.set(AppView::Setup(SetupStep::ScanDeposit))>"Deposit More"</button>
            </div>

            <div class="admin-section">
                <h2 style="font-size: 1rem; font-weight: 600; margin-bottom: 0.5rem;">"Transactions"</h2>
                {move || {
                    let groups = transactions();
                    if groups.is_empty() {
                        view! { <p style="color: #6b7280; font-size: 0.875rem;">"No transactions yet"</p> }.into_any()
                    } else {
                        groups.into_iter().map(|(date, txs)| view! {
                            <div style="margin-bottom: 0.75rem;">
                                <p style="font-weight: 600; font-size: 0.875rem; color: #374151; margin-bottom: 0.25rem;">{date}</p>
                                {txs.into_iter().map(|tx| {
                                    let (sign, color) = match tx.tx_type {
                                        TxType::Sale => ("+", "#059669"),
                                        TxType::Deposit => ("+", "#2563eb"),
                                        TxType::Withdrawal => ("-", "#dc2626"),
                                    };
                                    view! {
                                        <div class="transaction-item">
                                            <span style="font-size: 0.875rem;">{tx.tx_type.to_string()}</span>
                                            <span style=format!("font-weight: 600; color: {};", color)>
                                                {format!("{}{} sats", sign, tx.amount_msats / 1000)}
                                            </span>
                                        </div>
                                    }
                                }).collect::<Vec<_>>()}
                            </div>
                        }).collect::<Vec<_>>().into_any()
                    }
                }}
            </div>

            <button class="btn btn-gray" on:click=move |_| view.set(AppView::Pos)>"Back to PoS"</button>
        </div>
    }
}

// ─── Shared Components ──────────────────────────────────────────

#[component]
fn PinNumpad(
    on_digit: impl Fn(char) + 'static + Copy,
    on_backspace: impl Fn() + 'static + Copy,
    on_confirm: impl Fn() + 'static + Copy,
) -> impl IntoView {
    view! {
        <div class="numpad-grid" style="max-height: 320px;">
            {['1','2','3','4','5','6','7','8','9'].iter().map(|&d| {
                view! { <button type="button" class="numpad-btn" on:click=move |_| on_digit(d)>{d.to_string()}</button> }
            }).collect::<Vec<_>>()}
            <button type="button" class="numpad-btn" on:click=move |_| on_backspace()>{"\u{232b}"}</button>
            <button type="button" class="numpad-btn" on:click=move |_| on_digit('0')>"0"</button>
            <button type="button" class="numpad-btn primary" on:click=move |_| on_confirm()>{"\u{2713}"}</button>
        </div>
    }
}

// ─── Helpers ────────────────────────────────────────────────────

fn get_federation_prefix(cfg: &PosConfig) -> Result<ecash::FederationIdPrefix, String> {
    let fed_id_bytes = hex::decode(&cfg.federation_id)
        .map_err(|e| format!("Invalid federation ID hex: {e}"))?;
    if fed_id_bytes.len() < 4 {
        return Err("Federation ID too short".to_string());
    }
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&fed_id_bytes[..4]);
    Ok(prefix)
}

fn format_date(timestamp_ms: u64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(timestamp_ms as f64));
    let year = date.get_full_year();
    let month = date.get_month() + 1;
    let day = date.get_date();
    format!("{year}-{month:02}-{day:02}")
}

fn event_target_value(ev: &leptos::ev::Event) -> String {
    use wasm_bindgen::JsCast;
    let target = ev.target().unwrap_or_else(|| panic!("event target"));
    if let Ok(input) = target.clone().dyn_into::<web_sys::HtmlInputElement>() {
        return input.value();
    }
    if let Ok(ta) = target.clone().dyn_into::<web_sys::HtmlTextAreaElement>() {
        return ta.value();
    }
    if let Ok(sel) = target.dyn_into::<web_sys::HtmlSelectElement>() {
        return sel.value();
    }
    String::new()
}
