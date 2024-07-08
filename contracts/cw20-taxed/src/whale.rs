use cosmwasm_std::{Addr, Decimal, StdError, StdResult, Storage, Uint128};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::TOKEN_INFO;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct WhaleInfo  {
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

mod test {
    use cosmwasm_std::{testing::MockStorage, Addr, Decimal, Uint128};

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
}