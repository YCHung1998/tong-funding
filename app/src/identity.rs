//! Process identity (spec live-process): decided once at start from the command line and never
//! changed. `--live` is the ONLY way to become `Live`: not an environment variable, not a config
//! file, not a database value, not a UI action. Everything environment-dependent (hosts, database,
//! keychain item, banner) derives from this one value, passed down from the composition root.
#![allow(dead_code)]

/// Which world this process lives in. `Demo` also covers pure SIMULATION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessIdentity {
    #[default]
    Demo,
    Live,
}

/// Why the command line was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// `--live` must be the first argument of the GUI process.
    LiveNotFirst,
    /// `--live` with a developer / non-trading subcommand.
    LiveWithOtherMode(String),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::LiveNotFirst => write!(f, "`--live` must be the first argument (tong-funding --live)"),
            IdentityError::LiveWithOtherMode(m) => write!(f, "`--live` cannot be combined with `{m}`: it starts the trading window only"),
        }
    }
}

impl ProcessIdentity {
    /// Parses `std::env::args()` (including the program name at index 0) for the GUI process.
    /// The headless subcommands (`secrets`, `config`, `import-legacy-events`) are dispatched before this is
    /// called and take their own `--live` flag; for them the identity here stays `Demo`.
    pub fn from_args(args: &[String]) -> Result<ProcessIdentity, IdentityError> {
        let rest = args.get(1..).unwrap_or(&[]);
        let live_at = rest.iter().position(|a| a == "--live");
        match live_at {
            None => Ok(ProcessIdentity::Demo),
            Some(0) => match rest.get(1) {
                Some(next) if next.starts_with("--bench") => Err(IdentityError::LiveWithOtherMode(next.clone())),
                _ => Ok(ProcessIdentity::Live),
            },
            Some(_) => {
                // A subcommand carries its own `--live` (e.g. `secrets set --live ...`).
                let sub = rest.first().map(String::as_str);
                use crate::store::{config_cli, import_cli, secrets_cli};
                let headless = [secrets_cli::SUBCOMMAND, config_cli::SUBCOMMAND, import_cli::SUBCOMMAND];
                if sub.is_some_and(|s| headless.contains(&s)) { Ok(ProcessIdentity::Demo) } else { Err(IdentityError::LiveNotFirst) }
            }
        }
    }

    pub fn is_live(self) -> bool {
        self == ProcessIdentity::Live
    }

    /// Database file name of this identity (same data directory).
    pub fn db_file_name(self) -> &'static str {
        match self {
            ProcessIdentity::Demo => "funding.db",
            ProcessIdentity::Live => "funding-live.db",
        }
    }

    /// Keychain service of this identity's credentials bundle.
    pub fn keychain_service(self) -> &'static str {
        match self {
            ProcessIdentity::Demo => "tong-funding",
            ProcessIdentity::Live => "tong-funding-live",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_flag_is_demo_and_live_flag_first_is_live() {
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding"])), Ok(ProcessIdentity::Demo));
        assert_eq!(ProcessIdentity::from_args(&a(&[])), Ok(ProcessIdentity::Demo));
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "--live"])), Ok(ProcessIdentity::Live));
    }

    #[test]
    fn the_environment_never_decides_the_identity() {
        // SAFETY of the test: only this test touches these variables.
        for name in ["TONG_FUNDING_LIVE", "LIVE", "TONG_FUNDING_BINANCE_HOST", "TONG_FUNDING_IDENTITY"] {
            // SAFETY: single-threaded access to process env inside this test.
            unsafe { std::env::set_var(name, "live") };
        }
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding"])), Ok(ProcessIdentity::Demo));
        for name in ["TONG_FUNDING_LIVE", "LIVE", "TONG_FUNDING_BINANCE_HOST", "TONG_FUNDING_IDENTITY"] {
            // SAFETY: as above.
            unsafe { std::env::remove_var(name) };
        }
    }

    #[test]
    fn live_with_a_developer_mode_or_not_first_is_refused() {
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "--live", "--bench-table"])), Err(IdentityError::LiveWithOtherMode("--bench-table".into())));
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "--bench-donut", "--live"])), Err(IdentityError::LiveNotFirst));
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "x", "--live"])), Err(IdentityError::LiveNotFirst));
    }

    #[test]
    fn headless_subcommands_carry_their_own_live_flag_and_stay_demo_here() {
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "secrets", "set", "--live", "Bybit", "api_key"])), Ok(ProcessIdentity::Demo));
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "config", "--live"])), Ok(ProcessIdentity::Demo));
    }

    #[test]
    fn every_real_headless_subcommand_name_may_carry_its_own_live_flag() {
        use crate::store::{config_cli, import_cli, secrets_cli};
        for sub in [secrets_cli::SUBCOMMAND, config_cli::SUBCOMMAND, import_cli::SUBCOMMAND] {
            assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", sub, "x", "--live"])), Ok(ProcessIdentity::Demo), "{sub}");
        }
        // the old, wrong name is not a subcommand: `--live` after it is a misplaced flag
        assert_eq!(ProcessIdentity::from_args(&a(&["tong-funding", "import", "--live"])), Err(IdentityError::LiveNotFirst));
    }

    #[test]
    fn each_identity_has_its_own_database_and_keychain_service() {
        assert_eq!((ProcessIdentity::Demo.db_file_name(), ProcessIdentity::Demo.keychain_service()), ("funding.db", "tong-funding"));
        assert_eq!((ProcessIdentity::Live.db_file_name(), ProcessIdentity::Live.keychain_service()), ("funding-live.db", "tong-funding-live"));
        assert!(ProcessIdentity::Live.is_live() && !ProcessIdentity::Demo.is_live());
    }
}
