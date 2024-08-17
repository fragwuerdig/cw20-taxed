use std::char::REPLACEMENT_CHARACTER;

use cosmwasm_std::{DepsMut, Env, MessageInfo, Response};
use cosmwasm_std::{Addr, Decimal, StdError, StdResult, Storage, Uint128};
use crate::ContractError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use crate::state::ANTI_WHALE_INFO;

use crate::state::TOKEN_INFO;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct WhaleInfo {
    // percent of total supply that can be acquired by a single address
    pub threshold: Decimal,

    // list of addresses that are allowed to bypass the threshold
    pub whitelist: Vec<Addr>,

    // address of the governance contract/admin that can modify the info
    pub admin: Addr,
}

impl WhaleInfo {
    pub fn assert_no_whale(
        &self,
        storage: &dyn Storage,
        addr: &Addr,
        amount: Uint128,
    ) -> StdResult<()> {
        if self.is_allowed(addr) {
            return Ok(());
        }

        let info = TOKEN_INFO.load(storage)?;
        let total_supply = info.total_supply;

        // can used unchecked mul here, as threshold is between 0 and 1
        let max_allowed = total_supply * self.threshold;
        if amount.gt(&max_allowed) {
            return Err(StdError::generic_err(format!(
                "Address {} is holding too many tokens. Max allowed: {}. Tx results in: {}",
                addr, max_allowed, amount
            )));
        }

        Ok(())
    }

    pub fn is_allowed(&self, addr: &Addr) -> bool {
        self.whitelist.contains(addr)
    }

    pub fn validate(&self) -> StdResult<()> {
        if self.threshold > Decimal::one() {
            return Err(StdError::generic_err("Threshold must be between 0 and 1"));
        }
        Ok(())
    }
}

pub fn execute_set_whale_info(
    deps: DepsMut,
    env: Env, info: MessageInfo,
    whale_info: WhaleInfo
) -> Result<Response, ContractError> {
    let mut old_whale_info = ANTI_WHALE_INFO.load(deps.storage)?;
    whale_info.validate()?;
    if info.sender != old_whale_info.admin {
        return Err(ContractError::Unauthorized {});
    }
    ANTI_WHALE_INFO.save(deps.storage, &whale_info)?;
    Ok(Response::new())
}

pub fn execute_set_whale_admin(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    admin: Addr
) -> Result<Response, ContractError> {
    let mut old_info = ANTI_WHALE_INFO.load(deps.storage)?;
    if info.sender != old_info.admin {
        return Err(ContractError::Unauthorized{});
    }
    old_info.admin = admin;
    ANTI_WHALE_INFO.save(deps.storage, &old_info)?;
    Ok(Response::new())
}

#[cfg(test)]
mod test {
    use std::ops::Add;

    use cosmwasm_std::{
        testing::{mock_dependencies, mock_env, mock_info, MockStorage}, Addr, Decimal, Uint128
    };
    use crate::ContractError;
    use serde::de;

    use crate::state::TokenInfo;

    #[test]
    fn test_whale_info_validate() {
        let mut info = super::WhaleInfo {
            threshold: Decimal::zero(),
            whitelist: vec![],
            admin: Addr::unchecked("admin"),
        };
        assert!(info.validate().is_ok());

        info.threshold = Decimal::one();
        assert!(info.validate().is_ok());

        info.threshold = Decimal::percent(50);
        assert!(info.validate().is_ok());

        info.threshold = Decimal::percent(110);
        assert!(info.validate().is_err());
    }

    #[test]
    fn test_whale_info_is_allowed() {
        let addr1 = Addr::unchecked("addr1");
        let addr2 = Addr::unchecked("addr2");
        let addr3 = Addr::unchecked("addr3");

        let info = super::WhaleInfo {
            threshold: Decimal::percent(10),
            whitelist: vec![addr1.clone(), addr2.clone()],
            admin: Addr::unchecked("admin"),
        };

        assert!(info.is_allowed(&addr1));
        assert!(info.is_allowed(&addr2));
        assert!(!info.is_allowed(&addr3));
    }

    #[test]
    fn test_whale_info_assert_no_whale() {
        let addr1 = Addr::unchecked("addr1");
        let addr2 = Addr::unchecked("addr2");
        let addr3 = Addr::unchecked("addr3");

        let info = super::WhaleInfo {
            threshold: Decimal::percent(10),
            whitelist: vec![addr1.clone(), addr2.clone()],
            admin: Addr::unchecked("admin"),
        };

        let storage = &mut MockStorage::new();
        let total_supply = Uint128::new(1_000_000_000_000);
        let fish_amount = Uint128::new(10_000_000_000);
        let whale_amount = Uint128::new(110_000_000_000);

        let token_info = TokenInfo {
            name: String::from("test"),
            symbol: String::from("TEST"),
            decimals: 6,
            total_supply: total_supply,
            mint: None,
        };
        super::TOKEN_INFO.save(storage, &token_info).unwrap();

        // addr1 and addr2 are allowed to have any amount
        assert!(info.assert_no_whale(storage, &addr1, whale_amount).is_ok());
        assert!(info.assert_no_whale(storage, &addr2, whale_amount).is_ok());
        assert!(info.assert_no_whale(storage, &addr1, fish_amount).is_ok());
        assert!(info.assert_no_whale(storage, &addr2, fish_amount).is_ok());

        // not allowed to have more than 10% of total supply
        assert!(info.assert_no_whale(storage, &addr3, whale_amount).is_err());
        assert!(info.assert_no_whale(storage, &addr3, fish_amount).is_ok());
    }

    #[test]
    fn test_set_whale_info_works() {
        let mut deps = mock_dependencies();
        let info = mock_info("admin", &[]);
        let expected_whale_info = super::WhaleInfo {
            threshold: Decimal::percent(10),
            whitelist: vec![Addr::unchecked("whale1"), Addr::unchecked("whale2")],
            admin: Addr::unchecked("admin"),
        };

        // mock info being set by instantiation
        super::ANTI_WHALE_INFO.save(deps.as_mut().storage, &super::WhaleInfo {
            threshold: Decimal::one(),
            whitelist: vec![],
            admin: Addr::unchecked("admin"),
        }).unwrap();

        super::execute_set_whale_info(deps.as_mut(), mock_env(), info, expected_whale_info).unwrap();

        let new_info = super::ANTI_WHALE_INFO.load(deps.as_ref().storage).unwrap();
        assert_eq!(new_info, new_info);
        
    }

    #[test]
    fn test_set_whale_info_rejects_no_admin() {
        let mut deps = mock_dependencies();
        let info = mock_info("no_admin", &[]);
        let expected_whale_info = super::WhaleInfo {
            threshold: Decimal::percent(10),
            whitelist: vec![Addr::unchecked("whale1"), Addr::unchecked("whale2")],
            admin: Addr::unchecked("admin"),
        };

        // mock info being set by instantiation
        super::ANTI_WHALE_INFO.save(deps.as_mut().storage, &super::WhaleInfo {
            threshold: Decimal::one(),
            whitelist: vec![],
            admin: Addr::unchecked("admin"),
        }).unwrap();

        let err = super::execute_set_whale_info(deps.as_mut(), mock_env(), info, expected_whale_info);
        match err {
            Ok(_) => { panic!("expected failrue"); },
            Err(e) => {
                assert_eq!( e, ContractError::Unauthorized {  } )
            }
        }
        
    }

    #[test]
    fn test_set_whale_admin() {
        let mut deps = mock_dependencies();
        let info = mock_info("admin", &[]);
        let old_whale_info = super::WhaleInfo {
            threshold: Decimal::percent(10),
            whitelist: vec![Addr::unchecked("whale1"), Addr::unchecked("whale2")],
            admin: Addr::unchecked("admin"),
        };
        let mut expected_whale_info = old_whale_info.clone();
        expected_whale_info.admin = Addr::unchecked("admin2");

        // mock info being set by instantiation
        super::ANTI_WHALE_INFO.save(deps.as_mut().storage, &old_whale_info).unwrap();

        super::execute_set_whale_admin(deps.as_mut(), mock_env(), info, Addr::unchecked("admin2")).unwrap();

        let new_info = super::ANTI_WHALE_INFO.load(deps.as_mut().storage).unwrap();
        assert_eq!(new_info, expected_whale_info)
    }

    #[test]
    fn test_set_whale_admin_unauthorized() {
        let mut deps = mock_dependencies();
        let info = mock_info("no_admin", &[]);
        let old_whale_info = super::WhaleInfo {
            threshold: Decimal::percent(10),
            whitelist: vec![Addr::unchecked("whale1"), Addr::unchecked("whale2")],
            admin: Addr::unchecked("admin"),
        };

        // mock info being set by instantiation
        super::ANTI_WHALE_INFO.save(deps.as_mut().storage, &old_whale_info).unwrap();

        let res = super::execute_set_whale_admin(deps.as_mut(), mock_env(), info, Addr::unchecked("admin2"));
        match res {
            Ok(_) => {panic!("unexpected success of setting admin!")},
            Err(e) => {assert_eq!(e, ContractError::Unauthorized {  })}
        }

    }
}
