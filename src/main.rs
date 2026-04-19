//! nns-vesl hull binary.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use nockapp::kernel::boot;
use nockapp::NockApp;
use tokio::sync::Mutex;

use nns_vesl::{api, state::AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = boot::default_boot_cli(false);
    boot::init_default_tracing(&cli);

    // --- Load settlement config from vesl.toml ---
    let toml_path = std::env::var("VESL_TOML").unwrap_or_else(|_| "vesl.toml".into());
    let toml_cfg = load_toml(&PathBuf::from(&toml_path));
    let settlement = vesl_core::SettlementConfig::resolve(
        None,       // cli_mode
        None,       // cli_chain_endpoint
        false,      // cli_submit
        None,       // cli_tx_fee
        None,       // cli_coinbase_timelock_min
        None,       // cli_accept_timeout
        None,       // cli_seed_phrase
        &toml_cfg,
        None,       // default_signing_key (unused for local)
    );

    println!("=== nns-vesl ===");
    println!("  settlement mode: {}", settlement.mode);

    // --- Boot the kernel ---
    let kernel_path = std::env::var("NNS_KERNEL_JAM").unwrap_or_else(|_| "out.jam".into());
    let kernel = fs::read(&kernel_path)
        .map_err(|e| format!("failed to read kernel jam {kernel_path}: {e}"))?;

    // All durable hull + kernel state lives under a single
    // `.nns-data/` directory (relative to $NNS_DATA_DIR). We pass
    // the env-configured parent as `data_dir` and `".nns-data"` as
    // the app name to `boot::setup`, which internally joins them
    // to produce `$NNS_DATA_DIR/.nns-data/checkpoints/` and
    // `$NNS_DATA_DIR/.nns-data/pma/`. The mirror JSON sits
    // alongside them in the same `.nns-data` dir so everything
    // the hull writes at runtime is contained in one folder,
    // separate from the source tree.
    let data_parent = PathBuf::from(
        std::env::var("NNS_DATA_DIR").unwrap_or_else(|_| ".".into()),
    );
    let state_dir = data_parent.join(".nns-data");
    fs::create_dir_all(&state_dir)?;

    let app: NockApp = boot::setup(
        &kernel,
        cli,
        &[],
        ".nns-data",
        Some(data_parent.clone()),
    )
    .await?;

    println!("  kernel booted ({} bytes)", kernel.len());
    println!("  state dir: {}", state_dir.display());

    let state = Arc::new(Mutex::new(AppState::new(app, state_dir, settlement)));

    // --- Start HTTP server ---
    let port: u16 = std::env::var("API_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let bind: String = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".into());

    // We don't drive `NockApp::run()` (pokes happen directly from
    // axum handlers via the shared mutex), so nockapp's built-in
    // periodic save tick and save-on-exit paths never fire. We
    // compensate in two places:
    //
    //   1. Every handler that pokes calls `AppState::persist_all`
    //      to force a kernel checkpoint + mirror write inline.
    //   2. Here — race `api::serve` against SIGINT/SIGTERM and
    //      flush once more on shutdown. This covers the case where
    //      a save between handlers raced with the signal, and
    //      guarantees the on-disk state matches the last committed
    //      poke even if the signal lands mid-millisecond.
    //
    // Errors from the final flush are logged but swallowed: the
    // signal already committed us to exiting, and any prior
    // successful poke was already persisted by path (1).
    let serve_result = tokio::select! {
        r = api::serve(state.clone(), port, &bind) => r,
        _ = shutdown_signal() => {
            println!("shutdown signal received, flushing state...");
            Ok(())
        }
    };

    {
        let mut st = state.lock().await;
        st.persist_all().await;
    }

    serve_result
}

/// Resolve when the process should shut down cleanly. Fires on
/// Ctrl-C (SIGINT) on any platform and additionally on SIGTERM
/// on Unix. SIGKILL is uncatchable by design; state integrity
/// under SIGKILL depends entirely on the per-handler
/// `persist_all` having already run, which it will have unless
/// the signal landed mid-poke.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct Raw {
    settlement_mode: Option<String>,
    chain_endpoint: Option<String>,
    tx_fee: Option<u64>,
    coinbase_timelock_min: Option<u64>,
    accept_timeout_secs: Option<u64>,
}

fn load_toml(path: &std::path::Path) -> vesl_core::SettlementToml {
    let raw: Raw = match std::fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
            eprintln!("warning: failed to parse {}: {e}", path.display());
            Raw::default()
        }),
        Err(_) => Raw::default(),
    };
    vesl_core::SettlementToml {
        settlement_mode: raw.settlement_mode,
        chain_endpoint: raw.chain_endpoint,
        tx_fee: raw.tx_fee,
        coinbase_timelock_min: raw.coinbase_timelock_min,
        accept_timeout_secs: raw.accept_timeout_secs,
    }
}
