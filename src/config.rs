//! NNS-local configuration that is distinct from `vesl_core::SettlementConfig`.
//!
//! Keeping NNS-specific knobs out of vesl-core lets vesl stay a generic
//! settlement SDK. Today this module is narrow (one field: the payment
//! address), but Phase 2+ is expected to grow it with follower cadence
//! parameters, finality depth, etc.

/// Default Nockchain payment address the kernel treats as the NNS
/// treasury when C5 is enforced. This matches the legacy worker's
/// constant so existing deployments keep working without any new
/// flag. Override via CLI, `NNS_PAYMENT_ADDRESS` env, or `vesl.toml`.
pub const DEFAULT_PAYMENT_ADDRESS: &str =
    "8s29XUK8Do7QWt2MHfPdd1gDSta6db4c3bQrxP1YdJNfXpL3WPzTT5";

/// NNS-side settings that are distinct from vesl-core's generic
/// settlement config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NnsConfig {
    /// Base58-encoded Nockchain address that the C5 payment predicate
    /// (enforced in Phase 3 of the recursive-payment plan) will require
    /// every valid `%claim`'s tx to pay into. The hull issues a
    /// `%set-payment-address` poke at startup so the kernel freezes
    /// this value in state the first time a claim is accepted.
    pub payment_address: String,
}

impl NnsConfig {
    /// Build with the compiled-in default address.
    pub fn default_value() -> Self {
        Self {
            payment_address: DEFAULT_PAYMENT_ADDRESS.to_string(),
        }
    }

    /// Resolve the effective NNS config from layered sources.
    ///
    /// Override order (highest wins):
    ///   1. CLI override (passed explicitly by the caller)
    ///   2. `NNS_PAYMENT_ADDRESS` env var
    ///   3. TOML `payment_address` field
    ///   4. Compiled-in default
    pub fn resolve(cli_payment_address: Option<String>, toml: &NnsToml) -> Self {
        let payment_address = cli_payment_address
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("NNS_PAYMENT_ADDRESS")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| toml.payment_address.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| DEFAULT_PAYMENT_ADDRESS.to_string());
        Self { payment_address }
    }
}

impl Default for NnsConfig {
    fn default() -> Self {
        Self::default_value()
    }
}

/// Raw TOML shape for NNS-local fields. Deliberately serde-default so
/// a `vesl.toml` that doesn't mention NNS fields still parses.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct NnsToml {
    pub payment_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_env() {
        std::env::remove_var("NNS_PAYMENT_ADDRESS");
    }

    #[test]
    fn resolves_default_when_nothing_set() {
        clear_env();
        let cfg = NnsConfig::resolve(None, &NnsToml::default());
        assert_eq!(cfg.payment_address, DEFAULT_PAYMENT_ADDRESS);
    }

    #[test]
    fn cli_beats_env_beats_toml_beats_default() {
        clear_env();
        let toml = NnsToml {
            payment_address: Some("from-toml".into()),
        };

        // TOML only
        let cfg = NnsConfig::resolve(None, &toml);
        assert_eq!(cfg.payment_address, "from-toml");

        // Env beats TOML
        std::env::set_var("NNS_PAYMENT_ADDRESS", "from-env");
        let cfg = NnsConfig::resolve(None, &toml);
        assert_eq!(cfg.payment_address, "from-env");

        // CLI beats env
        let cfg = NnsConfig::resolve(Some("from-cli".into()), &toml);
        assert_eq!(cfg.payment_address, "from-cli");

        clear_env();
    }
}
