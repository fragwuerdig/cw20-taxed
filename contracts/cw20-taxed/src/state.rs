use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Uint128};
use cw_storage_plus::{Item, Map};

use cw20::{AllowanceResponse, Logo, MarketingInfoResponse};

use crate::tax::TaxMap;
use crate::whale::WhaleInfo;

#[cw_serde]
pub struct TokenInfo {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: Uint128,
    pub mint: Option<MinterData>,
}

#[cw_serde]
// Subset of the token info that makes sense
// to change after instantiation
pub struct MigrateTokenInfo {
    pub name: String,
    pub symbol: String,
    pub mint: Option<MinterData>,
}

#[cw_serde]
pub struct MinterData {
    pub minter: Addr,
    /// cap is how many more tokens can be issued by the minter
    pub cap: Option<Uint128>,
}

impl TokenInfo {
    pub fn get_cap(&self) -> Option<Uint128> {
        self.mint.as_ref().and_then(|v| v.cap)
    }
}

pub const TOKEN_INFO: Item<TokenInfo> = Item::new("token_info");
pub const MARKETING_INFO: Item<MarketingInfoResponse> = Item::new("marketing_info");
pub const LOGO: Item<Logo> = Item::new("logo");
pub const BALANCES: Map<&Addr, Uint128> = Map::new("balance");
pub const ALLOWANCES: Map<(&Addr, &Addr), AllowanceResponse> = Map::new("allowance");
// TODO: After https://github.com/CosmWasm/cw-plus/issues/670 is implemented, replace this with a `MultiIndex` over `ALLOWANCES`
pub const ALLOWANCES_SPENDER: Map<(&Addr, &Addr), AllowanceResponse> =
    Map::new("allowance_spender");

// specific for TAXED token
pub const TAX_INFO: Item<TaxMap> = Item::new("tax_info");

// anti whale measures
pub const ANTI_WHALE_INFO: Item<WhaleInfo> = Item::new("whale_info");

// specific only for migration from Terraport Tokens
pub mod migrate_v1 {
    use std::str::FromStr;

    use cosmwasm_std::{Addr, StdError, StdResult, Storage, Uint128};
    use cw2::{get_contract_version, set_contract_version};
    use cw_storage_plus::{Map, SnapshotMap, Strategy};
    use semver::Version;

    use crate::contract::{CONTRACT_NAME, CONTRACT_NAME_TERRAPORT, CONTRACT_NAME_TERRASWAP};

    pub const BALANCES: SnapshotMap<&Addr, Uint128> = SnapshotMap::new(
        "balance",
        "balance__checkpoints",
        "balance__changelog",
        Strategy::EveryBlock,
    );

    pub const TOTAL_SUPPLY_HISTORY: Map<u64, Uint128> = Map::new("total_supply_history");

    pub fn is_terraport_token_v0(store: &dyn Storage) -> StdResult<bool> {
        let version = get_contract_version(store)?;
        Ok(version.contract == CONTRACT_NAME_TERRAPORT && version.version == "0.0.0")
    }

    pub fn is_terraswap_token_v0(store: &dyn Storage) -> StdResult<bool> {
        let version = get_contract_version(store)?;
        Ok(version.contract == CONTRACT_NAME_TERRASWAP && version.version == "0.0.0")
    }

    pub fn is_cw_base_v0(store: &dyn Storage) -> StdResult<bool> {
        let version = get_contract_version(store)?;
        Ok(version.contract == "crates.io:cw20-base" && version.version == "0.0.0")
    }

    pub fn is_cw_base_1_0_1(store: &dyn Storage) -> StdResult<bool> {
        let version = get_contract_version(store)?;
        Ok(version.contract == "crates.io:cw20-base" && version.version == "1.0.1")
    }

    pub fn is_cw20_taxed_v0(store: &dyn Storage) -> StdResult<bool> {
        let version = get_contract_version(store)?;
        let this_version = Version::from_str(version.version.as_str())
            .map_err(|_| StdError::generic_err("no valid version in store"))?;
        let expect_v0 = Version::from_str("1.1.0")
            .map_err(|_| StdError::generic_err("could not parse version 1.0.0"))?;
        Ok(version.contract == CONTRACT_NAME && expect_v0 <= this_version)
    }

    /// this is to ensure contract version is normalized to 1.1.0 of the
    /// cw20-taxed contract in cases we have tokens that were not cw20-taxed
    /// before. Only allow specific known migration paths and normalize
    /// the contract version to 1.1.0
    pub fn ensure_known_upgrade_path(store: &mut dyn Storage) -> StdResult<()> {
        // this is for terraport tokens - normalize to v1.1.0
        if is_terraport_token_v0(store)? {
            set_contract_version(store, "crates.io:cw20-base", "1.1.0")?;
            return Ok(());

        // this is for terraswap tokens - normalize to v1.1.0
        } else if is_terraswap_token_v0(store)? {
            set_contract_version(store, "crates.io:cw20-base", "1.1.0")?;
            return Ok(());

        // this is for first revision of taxed - no normalization needed
        } else if is_cw20_taxed_v0(store)? {
            return Ok(());

        // this is for FRG tokens - normalize to v1.1.0
        } else if is_cw_base_1_0_1(store)? {
            set_contract_version(store, CONTRACT_NAME, "1.1.0")?;
            return Ok(());

        // this is for terraswap tokens - normalize to v1.1.0
        } else if is_cw_base_v0(store)? {
            set_contract_version(store, CONTRACT_NAME, "1.1.0")?;
            return Ok(());

        // no known migration path -play safe and throw
        } else {
            return Err(StdError::generic_err(
                "This is not a knowledable migration path",
            ));
        }
    }

    #[cfg(test)]
    pub mod tests {
        use super::*;
        use cosmwasm_std::{
            testing::{mock_dependencies, MockApi, MockQuerier, MockStorage},
            OwnedDeps,
        };
        use cw2::set_contract_version;
        use cw20_base::state::{TokenInfo, TOKEN_INFO};

        // mock a terraport style store
        pub fn mock_dependencies_with_terraport_balances(
            balances: Vec<(Addr, Uint128, u64)>,
        ) -> OwnedDeps<MockStorage, MockApi, MockQuerier> {
            let mut deps = mock_dependencies();
            set_contract_version(&mut deps.storage, "crates.io:terraport-token", "0.0.0").unwrap();
            let mut latest_height = 0;
            for (addr, balance, height) in balances {
                BALANCES
                    .save(&mut deps.storage, &addr, &balance, height)
                    .unwrap();
            }
            let total_supply: Uint128 = BALANCES
                .range(&mut deps.storage, None, None, cosmwasm_std::Order::Descending)
                .map(|res| -> Uint128 {
                    let unwrapped_res = res.unwrap_or((Addr::unchecked(""), Uint128::zero()));
                    return unwrapped_res.1;
                })
                .sum();
            TOKEN_INFO.save(&mut deps.storage, &TokenInfo {
                name: "terraport".to_string(),
                symbol: "TPT".to_string(),
                decimals: 6,
                total_supply: total_supply,
                mint: None,
            }).unwrap();
            deps
        }

        #[test]
        fn test_is_terraport_token_v0() {
            let mut store = MockStorage::new();

            set_contract_version(&mut store, "crates.io:cw20-base", "1.0.6").unwrap();
            assert_eq!(is_terraport_token_v0(&store).unwrap(), false);

            set_contract_version(&mut store, "crates.io:cw20-base", "0.0.0").unwrap();
            assert_eq!(is_terraport_token_v0(&store).unwrap(), false);

            set_contract_version(&mut store, "crates.io:terraport-token", "0.0.0").unwrap();
            assert_eq!(is_terraport_token_v0(&store).unwrap(), true);

            set_contract_version(&mut store, "crates.io:terraport-token", "1.0.0").unwrap();
            assert_eq!(is_terraport_token_v0(&store).unwrap(), false);
        }

        #[test]
        fn test_is_cw20_base_1_0_1() {
            let mut store = MockStorage::new();

            set_contract_version(&mut store, "crates.io:cw20-base", "1.0.6").unwrap();
            assert_eq!(is_cw_base_1_0_1(&store).unwrap(), false);

            set_contract_version(&mut store, "crates.io:cw20-base", "1.0.1").unwrap();
            assert_eq!(is_cw_base_1_0_1(&store).unwrap(), true);

            set_contract_version(&mut store, "crates.io:cw20-base", "1.0.0").unwrap();
            assert_eq!(is_cw_base_1_0_1(&store).unwrap(), false);
        }

        #[test]
        fn test_is_terraswap_token_v0() {
            let mut store = MockStorage::new();

            set_contract_version(&mut store, "crates.io:cw20-base", "1.0.6").unwrap();
            assert_eq!(is_terraswap_token_v0(&store).unwrap(), false);

            set_contract_version(&mut store, "crates.io:cw20-base", "0.0.0").unwrap();
            assert_eq!(is_terraswap_token_v0(&store).unwrap(), false);

            set_contract_version(&mut store, "crates.io:terraswap-token", "0.0.0").unwrap();
            assert_eq!(is_terraswap_token_v0(&store).unwrap(), true);

            set_contract_version(&mut store, "crates.io:terraswap-token", "1.0.0").unwrap();
            assert_eq!(is_terraswap_token_v0(&store).unwrap(), false);
        }

        #[test]
        fn test_terraport_snapshot_map_is_compatible_with_map() {
            let deps = mock_dependencies_with_terraport_balances(vec![
                // initial balances
                (Addr::unchecked("addr1"), Uint128::new(1234), 123),
                (Addr::unchecked("addr2"), Uint128::new(1234), 123),
                (Addr::unchecked("addr3"), Uint128::new(4455), 123),
                // mock a transfer at later height
                (Addr::unchecked("addr1"), Uint128::new(1233), 456),
                (Addr::unchecked("addr2"), Uint128::new(1235), 456),
            ]);

            // ensure the new data is compatible
            assert_eq!(
                super::BALANCES
                    .load(&deps.storage, &Addr::unchecked("addr1"))
                    .unwrap(),
                Uint128::new(1233)
            );
            assert_eq!(
                super::BALANCES
                    .load(&deps.storage, &Addr::unchecked("addr2"))
                    .unwrap(),
                Uint128::new(1235)
            );
            assert_eq!(
                super::BALANCES
                    .load(&deps.storage, &Addr::unchecked("addr3"))
                    .unwrap(),
                Uint128::new(4455)
            );
        }
    }
}
