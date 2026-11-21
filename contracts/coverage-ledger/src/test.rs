extern crate std;

use super::*;
use reporter_registry::{ReporterRegistry, ReporterRegistryClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _, MockAuth, MockAuthInvoke},
    Address, Bytes, BytesN, Env, IntoVal, String,
};

const T0: u64 = 1_700_000_000;
const UNIT: i128 = 10_000_000;

fn upgrade_fixture_wasm() -> std::vec::Vec<u8> {
    const VENDORED: &[u8] =
        include_bytes!("../../upgrade-fixture-ledger/testdata/upgrade_fixture_ledger.wasm");
    let built = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target/wasm32v1-none/release/upgrade_fixture_ledger.wasm");
    std::fs::read(built).unwrap_or_else(|_| VENDORED.to_vec())
}

fn upload_upgrade_fixture(env: &Env) -> BytesN<32> {
    env.deployer()
        .upload_contract_wasm(Bytes::from_slice(env, &upgrade_fixture_wasm()))
}

struct Fixture {
    env: Env,
    admin: Address,
    ledger: CoverageLedgerClient<'static>,
    registry: ReporterRegistryClient<'static>,
    sac: Address,
    issuer: Address,
    poster: Address,
    auditor: Address,
    transcriber: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(T0);
    env.ledger().set_sequence_number(1_000);

    let admin = Address::generate(&env);
    let registry_id = env.register(ReporterRegistry, (admin.clone(),));
    let registry = ReporterRegistryClient::new(&env, &registry_id);
    let ledger_id = env.register(CoverageLedger, (admin.clone(), registry_id.clone()));
    let ledger = CoverageLedgerClient::new(&env, &ledger_id);

    // The SAC admin is a contract address; the asset's issuer is the G-account
    // the test host creates, which is what `name()` reports.
    let sac_admin = Address::generate(&env);
    let handle = env.register_stellar_asset_contract_v2(sac_admin);
    let sac = handle.address();
    let issuer = handle.issuer().address();

    let poster = Address::generate(&env);
    let auditor = Address::generate(&env);
    let transcriber = Address::generate(&env);
    registry.set_reporter(
        &poster,
        &Role::SupplyPoster,
        &String::from_str(&env, "indexer"),
    );
    registry.set_reporter(
        &auditor,
        &Role::Auditor,
        &String::from_str(&env, "Acme Audit"),
    );
    registry.set_reporter(
        &transcriber,
        &Role::Transcriber,
        &String::from_str(&env, "volunteer"),
    );

    Fixture {
        env,
        admin,
        ledger,
        registry,
        sac,
        issuer,
        poster,
        auditor,
        transcriber,
    }
}

fn hash(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

fn uri(env: &Env) -> String {
    String::from_str(env, "https://example.org/attestation-2026-09.pdf")
}

fn listed() -> Fixture {
    let f = setup();
    f.ledger.list_asset(&f.sac);
    f
}

// ---- list_asset -----------------------------------------------------------

#[test]
fn list_asset_reads_code_and_issuer_from_sac() {
    let f = setup();
    let record = f.ledger.list_asset(&f.sac);
    assert_eq!(record.issuer, f.issuer);
    assert_eq!(record.sac, f.sac);
    assert_eq!(record.listed_at, T0);
    assert!(!record.code.is_empty());
    assert_eq!(f.ledger.get_asset(&f.sac), Some(record));
}

#[test]
fn list_asset_twice_fails() {
    let f = listed();
    let res = f.ledger.try_list_asset(&f.sac);
    assert_eq!(res, Err(Ok(LedgerError::AlreadyListed.into())));
}

#[test]
fn list_asset_rejects_wasm_contract() {
    let f = setup();
    // The registry is a Wasm-style contract, not a Stellar Asset Contract.
    let res = f.ledger.try_list_asset(&f.registry.address);
    assert_eq!(res, Err(Ok(LedgerError::NotAStellarAsset.into())));
}

#[test]
fn list_asset_rejects_account_address() {
    let f = setup();
    let res = f.ledger.try_list_asset(&Address::generate(&f.env));
    assert!(res.is_err());
}

#[test]
fn parse_rejects_native_and_malformed() {
    let env = Env::default();
    assert!(parse_sac_name(&env, &String::from_str(&env, "native")).is_none());
    assert!(parse_sac_name(&env, &String::from_str(&env, "USDC")).is_none());
    let no_code = std::format!(":{}", "G".repeat(56));
    assert!(parse_sac_name(&env, &String::from_str(&env, &no_code)).is_none());
    let long_code = std::format!("ABCDEFGHIJKLM:{}", "G".repeat(56));
    assert!(parse_sac_name(&env, &String::from_str(&env, &long_code)).is_none());
}

#[test]
fn parse_accepts_real_name() {
    let env = Env::default();
    let name = "USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN";
    let (code, issuer) = parse_sac_name(&env, &String::from_str(&env, name)).unwrap();
    assert_eq!(code, String::from_str(&env, "USDC"));
    assert_eq!(
        issuer.to_string(),
        String::from_str(
            &env,
            "GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN"
        )
    );
}

// ---- post_supply ----------------------------------------------------------

#[test]
fn post_supply_stores_snapshot() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(1_000 * UNIT), &990, &hash(&f.env, 1));
    let s = f.ledger.get_supply(&f.sac).unwrap();
    assert_eq!(s.amount, 1_000 * UNIT);
    assert_eq!(s.ledger, 990);
    assert_eq!(s.timestamp, T0);
    assert_eq!(s.poster, f.poster);
}

#[test]
fn post_supply_requires_poster_role() {
    let f = listed();
    let res = f
        .ledger
        .try_post_supply(&f.auditor, &f.sac, &UNIT, &990, &hash(&f.env, 1));
    assert_eq!(res, Err(Ok(LedgerError::NotSupplyPoster.into())));
}

#[test]
fn post_supply_requires_listed_asset() {
    let f = setup();
    let res = f
        .ledger
        .try_post_supply(&f.poster, &f.sac, &UNIT, &990, &hash(&f.env, 1));
    assert_eq!(res, Err(Ok(LedgerError::AssetNotListed.into())));
}

#[test]
fn post_supply_rejects_negative() {
    let f = listed();
    let res = f
        .ledger
        .try_post_supply(&f.poster, &f.sac, &-1, &990, &hash(&f.env, 1));
    assert_eq!(res, Err(Ok(LedgerError::InvalidAmount.into())));
}

#[test]
fn post_supply_rejects_future_ledger() {
    let f = listed();
    let res = f
        .ledger
        .try_post_supply(&f.poster, &f.sac, &UNIT, &1_001, &hash(&f.env, 1));
    assert_eq!(res, Err(Ok(LedgerError::FutureLedger.into())));
}

#[test]
fn post_supply_must_move_forward() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &UNIT, &990, &hash(&f.env, 1));
    let res = f
        .ledger
        .try_post_supply(&f.poster, &f.sac, &UNIT, &990, &hash(&f.env, 2));
    assert_eq!(res, Err(Ok(LedgerError::StaleLedger.into())));
}

#[test]
fn revoked_poster_cannot_post() {
    let f = listed();
    f.registry.revoke(&f.poster);
    let res = f
        .ledger
        .try_post_supply(&f.poster, &f.sac, &UNIT, &990, &hash(&f.env, 1));
    assert_eq!(res, Err(Ok(LedgerError::NotSupplyPoster.into())));
}

// ---- post_reserve ---------------------------------------------------------

#[test]
fn issuer_report_is_issuer_signed() {
    let f = listed();
    let tier = f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(1_000 * UNIT),
        &(T0 - 60),
        &hash(&f.env, 9),
        &uri(&f.env),
    );
    assert_eq!(tier, Tier::IssuerSigned);
    let r = f.ledger.get_report(&f.sac, &Tier::IssuerSigned).unwrap();
    assert_eq!(r.amount, 1_000 * UNIT);
    assert_eq!(r.reporter, f.issuer);
    assert_eq!(r.posted_at, T0);
}

#[test]
fn auditor_and_transcriber_tiers() {
    let f = listed();
    let a = f.ledger.post_reserve(
        &f.auditor,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    let t = f.ledger.post_reserve(
        &f.transcriber,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    assert_eq!(a, Tier::AuditorSigned);
    assert_eq!(t, Tier::Transcribed);
}

#[test]
fn stranger_cannot_report() {
    let f = listed();
    let res = f.ledger.try_post_reserve(
        &Address::generate(&f.env),
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    assert_eq!(res, Err(Ok(LedgerError::NotReporter.into())));
}

#[test]
fn supply_poster_cannot_report_reserves() {
    let f = listed();
    let res = f.ledger.try_post_reserve(
        &f.poster,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    assert_eq!(res, Err(Ok(LedgerError::NotReporter.into())));
}

#[test]
fn report_rejects_future_as_of() {
    let f = listed();
    let res = f.ledger.try_post_reserve(
        &f.auditor,
        &f.sac,
        &UNIT,
        &(T0 + 1),
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    assert_eq!(res, Err(Ok(LedgerError::FutureTimestamp.into())));
}

#[test]
fn report_must_move_forward_per_tier() {
    let f = listed();
    f.ledger.post_reserve(
        &f.auditor,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    let res = f.ledger.try_post_reserve(
        &f.auditor,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    assert_eq!(res, Err(Ok(LedgerError::StaleReport.into())));
    // A different tier at the same timestamp is fine.
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 3),
        &uri(&f.env),
    );
}

#[test]
fn report_rejects_empty_uri() {
    let f = listed();
    let res = f.ledger.try_post_reserve(
        &f.auditor,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 1),
        &String::from_str(&f.env, ""),
    );
    assert_eq!(res, Err(Ok(LedgerError::InvalidUri.into())));
}

#[test]
fn report_rejects_negative_amount() {
    let f = listed();
    let res =
        f.ledger
            .try_post_reserve(&f.auditor, &f.sac, &-5, &T0, &hash(&f.env, 1), &uri(&f.env));
    assert_eq!(res, Err(Ok(LedgerError::InvalidAmount.into())));
}

/// Posts `amount` as an auditor report against a supply of `supply`, returning
/// what `try_post_reserve` gave back so a caller can assert on the error.
type TryReserve = Result<
    Result<Tier, soroban_sdk::ConversionError>,
    Result<soroban_sdk::Error, soroban_sdk::InvokeError>,
>;

fn report_against_supply(f: &Fixture, supply: i128, amount: i128) -> TryReserve {
    f.ledger
        .post_supply(&f.poster, &f.sac, &supply, &990, &hash(&f.env, 1));
    f.ledger.try_post_reserve(
        &f.auditor,
        &f.sac,
        &amount,
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    )
}

// The slip this guards against: 1,020 reserves typed in whole units rather
// than the 7-decimal units the contract counts in. The asset would read as
// 0.00% covered, which is what a collapse of backing looks like too.
#[test]
fn report_rejects_a_figure_typed_in_whole_units() {
    let f = listed();
    let res = report_against_supply(&f, 1_000 * UNIT, 1_020);
    assert_eq!(res, Err(Ok(LedgerError::ImplausibleAmount.into())));
}

#[test]
fn report_accepts_the_narrowest_plausible_coverage() {
    let f = listed();
    // 1 unit of reserves against 10,000 of supply is exactly 1 bps.
    assert!(report_against_supply(&f, 10_000 * UNIT, UNIT).is_ok());
}

#[test]
fn report_rejects_coverage_below_the_narrowest() {
    let f = listed();
    // One stroop less rounds to 0 bps.
    let res = report_against_supply(&f, 10_000 * UNIT, UNIT - 1);
    assert_eq!(res, Err(Ok(LedgerError::ImplausibleAmount.into())));
}

#[test]
fn report_accepts_the_widest_plausible_coverage() {
    let f = listed();
    // 100 units against 1 is 1,000,000 bps, or 10,000%.
    assert!(report_against_supply(&f, UNIT, 100 * UNIT).is_ok());
}

#[test]
fn report_rejects_coverage_above_the_widest() {
    let f = listed();
    // 1,000,001 bps: the smallest figure over the ceiling.
    let res = report_against_supply(&f, UNIT, 100 * UNIT + 1_000);
    assert_eq!(res, Err(Ok(LedgerError::ImplausibleAmount.into())));
}

// With no snapshot there is nothing to judge a figure against, and refusing
// reports until someone posts supply would stop an asset being described at
// all.
#[test]
fn report_skips_the_plausibility_check_when_no_supply_is_posted() {
    let f = listed();
    f.ledger
        .post_reserve(&f.auditor, &f.sac, &1, &T0, &hash(&f.env, 1), &uri(&f.env));
    assert_eq!(
        f.ledger
            .get_report(&f.sac, &Tier::AuditorSigned)
            .unwrap()
            .amount,
        1
    );
}

// No reserves at all is a statement the protocol exists to carry, and unlike a
// small non-zero figure it cannot be a unit slip.
#[test]
fn report_accepts_zero_reserves() {
    let f = listed();
    assert!(report_against_supply(&f, 1_000 * UNIT, 0).is_ok());
    assert_eq!(
        f.ledger.coverage(&f.sac, &Tier::Transcribed).unwrap().bps,
        0
    );
}

// ---- get_reports ----------------------------------------------------------

#[test]
fn get_reports_is_empty_when_nothing_is_posted() {
    let f = listed();
    assert_eq!(f.ledger.get_reports(&f.sac).len(), 0);
}

#[test]
fn get_reports_returns_the_one_posted_tier() {
    let f = listed();
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(1_000 * UNIT),
        &(T0 - 60),
        &hash(&f.env, 9),
        &uri(&f.env),
    );
    let reports = f.ledger.get_reports(&f.sac);
    assert_eq!(reports.len(), 1);
    let only = reports.get(0).unwrap();
    assert_eq!(only.tier, Tier::IssuerSigned);
    assert_eq!(only.amount, 1_000 * UNIT);
}

#[test]
fn get_reports_returns_every_tier_strongest_first() {
    let f = listed();
    f.ledger.post_reserve(
        &f.transcriber,
        &f.sac,
        &UNIT,
        &T0,
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(2 * UNIT),
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    f.ledger.post_reserve(
        &f.auditor,
        &f.sac,
        &(3 * UNIT),
        &T0,
        &hash(&f.env, 3),
        &uri(&f.env),
    );
    let reports = f.ledger.get_reports(&f.sac);
    assert_eq!(reports.len(), 3);
    assert_eq!(reports.get(0).unwrap().tier, Tier::AuditorSigned);
    assert_eq!(reports.get(1).unwrap().tier, Tier::IssuerSigned);
    assert_eq!(reports.get(2).unwrap().tier, Tier::Transcribed);
    // Same order as calling get_report once per tier.
    for report in reports.iter() {
        assert_eq!(f.ledger.get_report(&f.sac, &report.tier), Some(report));
    }
}

#[test]
fn get_reports_keeps_the_latest_per_tier() {
    let f = listed();
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &UNIT,
        &(T0 - 600),
        &hash(&f.env, 1),
        &uri(&f.env),
    );
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(5 * UNIT),
        &(T0 - 60),
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    let reports = f.ledger.get_reports(&f.sac);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports.get(0).unwrap().amount, 5 * UNIT);
}

#[test]
fn get_reports_on_an_unlisted_asset_is_empty() {
    let f = setup();
    assert_eq!(f.ledger.get_reports(&f.sac).len(), 0);
}

// ---- coverage / is_covered ------------------------------------------------

#[test]
fn coverage_none_without_data() {
    let f = listed();
    assert_eq!(f.ledger.coverage(&f.sac, &Tier::Transcribed), None);
    f.ledger
        .post_supply(&f.poster, &f.sac, &UNIT, &990, &hash(&f.env, 1));
    assert_eq!(f.ledger.coverage(&f.sac, &Tier::Transcribed), None);
}

#[test]
fn coverage_computes_bps() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(1_000 * UNIT), &990, &hash(&f.env, 1));
    f.ledger.post_reserve(
        &f.auditor,
        &f.sac,
        &(1_020 * UNIT),
        &(T0 - 100),
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    let c = f.ledger.coverage(&f.sac, &Tier::Transcribed).unwrap();
    assert_eq!(c.bps, 10_200);
    assert_eq!(c.tier, Tier::AuditorSigned);
    assert_eq!(c.supply, 1_000 * UNIT);
    assert_eq!(c.reserves, 1_020 * UNIT);
    assert_eq!(c.supply_ledger, 990);
    assert_eq!(c.report_as_of, T0 - 100);
}

#[test]
fn coverage_prefers_freshest_report_at_or_above_min_tier() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(100 * UNIT), &990, &hash(&f.env, 1));
    // Older audit says 90%, newer transcription says 101%.
    f.ledger.post_reserve(
        &f.auditor,
        &f.sac,
        &(90 * UNIT),
        &(T0 - 1_000),
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    f.ledger.post_reserve(
        &f.transcriber,
        &f.sac,
        &(101 * UNIT),
        &(T0 - 10),
        &hash(&f.env, 3),
        &uri(&f.env),
    );
    let any = f.ledger.coverage(&f.sac, &Tier::Transcribed).unwrap();
    assert_eq!(any.tier, Tier::Transcribed);
    assert_eq!(any.bps, 10_100);
    let audited = f.ledger.coverage(&f.sac, &Tier::AuditorSigned).unwrap();
    assert_eq!(audited.tier, Tier::AuditorSigned);
    assert_eq!(audited.bps, 9_000);
}

#[test]
fn coverage_tie_goes_to_stronger_tier() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(100 * UNIT), &990, &hash(&f.env, 1));
    f.ledger.post_reserve(
        &f.transcriber,
        &f.sac,
        &(50 * UNIT),
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(100 * UNIT),
        &T0,
        &hash(&f.env, 3),
        &uri(&f.env),
    );
    let c = f.ledger.coverage(&f.sac, &Tier::Transcribed).unwrap();
    assert_eq!(c.tier, Tier::IssuerSigned);
}

#[test]
fn is_covered_checks_ratio_age_and_tier() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(100 * UNIT), &990, &hash(&f.env, 1));
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(100 * UNIT),
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    let day = 86_400;
    assert!(f
        .ledger
        .is_covered(&f.sac, &10_000, &day, &Tier::Transcribed));
    assert!(!f
        .ledger
        .is_covered(&f.sac, &10_001, &day, &Tier::Transcribed));
    assert!(!f
        .ledger
        .is_covered(&f.sac, &10_000, &day, &Tier::AuditorSigned));

    // Two days later both inputs are stale.
    f.env.ledger().set_timestamp(T0 + 2 * day);
    assert!(!f
        .ledger
        .is_covered(&f.sac, &10_000, &day, &Tier::Transcribed));
    assert!(f
        .ledger
        .is_covered(&f.sac, &10_000, &(3 * day), &Tier::Transcribed));
}

#[test]
fn is_covered_false_for_unlisted() {
    let f = setup();
    assert!(!f
        .ledger
        .is_covered(&f.sac, &0, &u64::MAX, &Tier::Transcribed));
}

#[test]
fn zero_supply_counts_as_covered() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &0, &990, &hash(&f.env, 1));
    f.ledger
        .post_reserve(&f.issuer, &f.sac, &0, &T0, &hash(&f.env, 2), &uri(&f.env));
    let c = f.ledger.coverage(&f.sac, &Tier::Transcribed).unwrap();
    assert_eq!(c.bps, u32::MAX);
}

// ---- admin ----------------------------------------------------------------

#[test]
fn transfer_admin_changes_admin() {
    let f = setup();
    let next = Address::generate(&f.env);
    f.ledger.transfer_admin(&next);
    assert_eq!(f.ledger.admin(), next);
    assert_eq!(f.ledger.registry(), f.registry.address);
}

// ---- upgrade --------------------------------------------------------------

#[test]
fn non_admin_cannot_upgrade() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let registry_id = env.register(ReporterRegistry, (admin.clone(),));
    let ledger_id = env.register(CoverageLedger, (admin.clone(), registry_id));
    let ledger = CoverageLedgerClient::new(&env, &ledger_id);
    let wasm_hash = upload_upgrade_fixture(&env);
    let stranger = Address::generate(&env);

    ledger.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &ledger_id,
            fn_name: "upgrade",
            args: (&wasm_hash,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(ledger.try_upgrade(&wasm_hash).is_err());

    // A refused upgrade must leave the old executable and its admin in place.
    assert_eq!(ledger.admin(), admin);
}

#[test]
fn admin_upgrade_keeps_state() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(100 * UNIT), &990, &hash(&f.env, 1));
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(100 * UNIT),
        &(T0 - 60),
        &hash(&f.env, 2),
        &uri(&f.env),
    );

    let wasm_hash = upload_upgrade_fixture(&f.env);
    f.ledger.upgrade(&wasm_hash);

    assert_eq!(f.ledger.admin(), f.admin);
    assert_eq!(f.ledger.registry(), f.registry.address);
    assert_eq!(f.ledger.get_asset(&f.sac).unwrap().issuer, f.issuer);
    assert_eq!(f.ledger.get_supply(&f.sac).unwrap().amount, 100 * UNIT);
    let report = f.ledger.get_report(&f.sac, &Tier::IssuerSigned).unwrap();
    assert_eq!(report.amount, 100 * UNIT);
    assert!(f
        .ledger
        .is_covered(&f.sac, &10_000, &86_400, &Tier::Transcribed));
}

// ---- resource budgets -----------------------------------------------------

const RESOURCE_BUDGETS: &str = include_str!("../../../resource-budgets.json");

fn ceiling(function: &str) -> (u64, u64) {
    let budgets: serde_json::Value =
        serde_json::from_str(RESOURCE_BUDGETS).expect("resource-budgets.json is valid JSON");
    let entry = &budgets["functions"][function];
    (
        entry["cpu_instructions"]
            .as_u64()
            .unwrap_or_else(|| panic!("{function} has no cpu_instructions ceiling")),
        entry["memory_bytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("{function} has no memory_bytes ceiling")),
    )
}

fn assert_within_budget(f: &Fixture, function: &str, call: impl FnOnce()) {
    f.env.cost_estimate().budget().reset_default();
    call();
    let measured = f.env.cost_estimate().budget();
    let cpu = measured.cpu_instruction_cost();
    let memory = measured.memory_bytes_cost();
    let (max_cpu, max_memory) = ceiling(function);
    std::println!(
        "{function}: {cpu} CPU instructions, {memory} memory bytes (ceilings {max_cpu}, {max_memory})"
    );
    assert!(
        cpu <= max_cpu,
        "{function} used {cpu} CPU instructions, over its {max_cpu} ceiling by {}. \
         Make it cheaper, or raise the ceiling in resource-budgets.json in this commit and say why.",
        cpu - max_cpu
    );
    assert!(
        memory <= max_memory,
        "{function} used {memory} memory bytes, over its {max_memory} ceiling by {}. \
         Make it cheaper, or raise the ceiling in resource-budgets.json in this commit and say why.",
        memory - max_memory
    );
}

#[test]
fn is_covered_stays_within_its_resource_budget() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(100 * UNIT), &990, &hash(&f.env, 1));
    f.ledger.post_reserve(
        &f.issuer,
        &f.sac,
        &(100 * UNIT),
        &T0,
        &hash(&f.env, 2),
        &uri(&f.env),
    );
    assert_within_budget(&f, "is_covered", || {
        assert!(f
            .ledger
            .is_covered(&f.sac, &10_000, &86_400, &Tier::Transcribed));
    });
}

#[test]
fn post_supply_stays_within_its_resource_budget() {
    let f = listed();
    assert_within_budget(&f, "post_supply", || {
        f.ledger
            .post_supply(&f.poster, &f.sac, &UNIT, &990, &hash(&f.env, 1));
    });
}

#[test]
fn post_reserve_stays_within_its_resource_budget() {
    let f = listed();
    f.ledger
        .post_supply(&f.poster, &f.sac, &(100 * UNIT), &990, &hash(&f.env, 1));
    assert_within_budget(&f, "post_reserve", || {
        f.ledger.post_reserve(
            &f.auditor,
            &f.sac,
            &(100 * UNIT),
            &T0,
            &hash(&f.env, 2),
            &uri(&f.env),
        );
    });
}
