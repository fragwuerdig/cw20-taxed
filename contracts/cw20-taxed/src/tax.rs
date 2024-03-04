use std::any::Any;

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{ Addr, Decimal, Empty, Querier, QuerierWrapper, StdError, StdResult, Uint128, WasmQuery};
use crate::error::ContractError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

trait TaxDeductible {
    fn is_taxed(&self, q: &QuerierWrapper, addr: Addr) -> bool;
    fn tax_rate(&self, q: &QuerierWrapper, addr: Addr) -> Decimal;
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, PartialEq)]
pub enum TaxCondition {
    Never(TaxNeverCondition),
    Always(TaxAlwaysCondition),
    ContractCode(TaxContractCodeCondition),
}

impl TaxCondition {
    pub fn is_taxed(&self, q: &QuerierWrapper, addr: Addr) -> bool {
        match self {
            TaxCondition::Never(c) => c.is_taxed(q, addr),
            TaxCondition::Always(c) => c.is_taxed(q, addr),
            TaxCondition::ContractCode(c) => c.is_taxed(q, addr),
        }
    }

    pub fn tax_rate(&self, q: &QuerierWrapper, addr: Addr) -> Decimal {
        match self {
            TaxCondition::Never(c) => c.tax_rate(q, addr),
            TaxCondition::Always(c) => c.tax_rate(q, addr),
            TaxCondition::ContractCode(c) => c.tax_rate(q, addr),
        }
    }

    fn tax_deduction(&self, q: &QuerierWrapper, addr: Addr, amount: Uint128) -> Result<(Uint128, Uint128), ContractError> {
        
        let tax_rate = self.tax_rate(q, addr);
        let gross_amount = Decimal::from_atomics(amount, 0)
            .map_err(|_| ContractError::Std(StdError::generic_err("Invalid amount")))?;
        let tax = tax_rate.checked_mul(gross_amount).unwrap();
        let net_amount = gross_amount.checked_sub(tax)
            .map_err(|_| ContractError::Std(StdError::generic_err("Taxed amount cannot be negative")))?;
        let net_out = net_amount.to_uint_ceil();
        let net_tax =  amount
            .checked_sub(net_out)
            .map_err(|_| ContractError::Std(StdError::generic_err("Taxed amount cannot be negative")))?;
        Ok((net_out, net_tax))
    
    }

    pub fn get_tax(&self, q: &QuerierWrapper, addr: Addr, amount: Uint128) -> Uint128 {
        match self.tax_deduction(q, addr, amount){
            Ok((_, tax)) => tax,
            Err(_) => Uint128::zero(),
        }
    }

    pub fn get_net(&self, q: &QuerierWrapper, addr: Addr, amount: Uint128) -> Uint128 {
        match self.tax_deduction(q, addr, amount){
            Ok((net, _)) => net,
            Err(_) => Uint128::zero(),
        }
    }

    pub fn validate(&self) -> bool {
        match self {
            TaxCondition::Never(x) => x.validate(),
            TaxCondition::Always(x) => x.validate(),
            TaxCondition::ContractCode(x) => x.validate(),
        }
    }
    
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, PartialEq)]
pub struct TaxInfo {
    pub src_cond: TaxCondition,
    pub dst_cond: TaxCondition,
    pub proceeds: Addr,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, PartialEq)]
pub struct TaxMap {
    pub on_transfer: TaxInfo,
    pub on_transfer_from: TaxInfo,
    pub on_send: TaxInfo,
    pub on_send_from: TaxInfo,
}

impl Default for TaxMap {
    fn default() -> Self {
        TaxMap {
            on_transfer: TaxInfo::default(),
            on_transfer_from: TaxInfo::default(),
            on_send: TaxInfo::default(),
            on_send_from: TaxInfo::default(),
        }
    }
}

impl TaxMap {
    pub fn validate(&self) -> StdResult<()> {
        match self.on_transfer.validate() &&
            self.on_transfer_from.validate() &&
            self.on_send.validate() &&
            self.on_send_from.validate() {
            true => {Ok(())},
            false => {Err(StdError::generic_err(String::from("invalid tax map")))},
        }
    }
}

impl Default for TaxInfo {
    fn default() -> Self {
        TaxInfo {
            src_cond: TaxCondition::Never(TaxNeverCondition{}),
            dst_cond: TaxCondition::Never(TaxNeverCondition{}),
            proceeds: Addr::unchecked(""),
        }
    }
}

impl TaxInfo {
    pub fn validate(&self) -> bool {
        self.src_cond.validate() && self.dst_cond.validate()
    }
}

#[cw_serde]
pub struct TaxNeverCondition {}

impl TaxNeverCondition {
    pub fn validate(&self) -> bool {
        true
    }
}

#[cw_serde]
pub struct TaxAlwaysCondition {
    pub tax_rate: Decimal,
}

impl TaxAlwaysCondition {
    pub fn validate(&self) -> bool {
        self.tax_rate.ge(&Decimal::zero()) && self.tax_rate.le(&Decimal::one())
    }
}

#[cw_serde]
pub struct TaxContractCodeCondition {
    pub code_ids: Vec<u64>,
    pub tax_rate: Decimal,
}

impl TaxContractCodeCondition {
    pub fn validate(&self) -> bool {
        self.tax_rate.ge(&Decimal::zero()) && self.tax_rate.le(&Decimal::one())
    }
}

impl TaxInfo {
    pub fn deduct_tax(&self, q: &QuerierWrapper, addr: Addr, amount: Uint128) -> Result<(Uint128, Uint128), ContractError> {
        let is_taxed = self.src_cond.is_taxed(q, addr.clone())
            && self.dst_cond.is_taxed(q, addr.clone())
            && self.proceeds != addr;
        match is_taxed {
            true => self.src_cond.tax_deduction(q, addr, amount),
            false => Ok((amount, Uint128::zero())),
            
        }
    }
}

impl TaxDeductible for TaxNeverCondition {
    fn is_taxed(&self, _: &QuerierWrapper, addr: Addr) -> bool {
        false
    }

    fn tax_rate(&self, _: &QuerierWrapper,  addr: Addr) -> Decimal {
        Decimal::zero()
    }
}

impl TaxDeductible for TaxAlwaysCondition {
    fn is_taxed(&self, _: &QuerierWrapper, addr: Addr) -> bool {
        true
    }

    fn tax_rate(&self, _: &QuerierWrapper,  addr: Addr) -> Decimal {
        self.tax_rate
    }
}

impl TaxDeductible for TaxContractCodeCondition {
    fn is_taxed(&self, q: &QuerierWrapper, addr: Addr) -> bool {
        let info = q.query_wasm_contract_info(addr);
        match info {
            Ok(info) => self.code_ids.contains(&info.code_id),
            Err(_) => false,
        }
    }

    fn tax_rate(&self, qw: &QuerierWrapper,  addr: Addr) -> Decimal {
        if self.is_taxed(qw, addr.clone()) {
            self.tax_rate
        } else {
            Decimal::zero()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::MockQuerier;
    use cosmwasm_std::{to_json_binary, Addr, ContractInfoResponse, ContractResult, Decimal, QuerierResult, StdResult, Uint128};

    fn get_info(addr: &Addr) -> StdResult<ContractInfoResponse> {

        let contract_infos: [(String, ContractInfoResponse); 3] = [
            ( String::from("0"), { let mut r = ContractInfoResponse::default(); r.code_id = 0; r } ),
            ( String::from("1"), { let mut r = ContractInfoResponse::default(); r.code_id = 1; r } ),
            ( String::from("2"), { let mut r = ContractInfoResponse::default(); r.code_id = 2; r } ),
        ];

        let addr = addr.as_str();
        let info = contract_infos.iter().find(|(a, _)| a == addr); 
        
        match info {
            Some((_, info)) => Ok(info.clone()),
            None => Err(StdError::generic_err("Not found")),   
        }
    
    }
    
    fn wasm_query_handler(request: &WasmQuery) -> QuerierResult {
        match request {
            WasmQuery::ContractInfo { contract_addr } => {
                let info = get_info(&Addr::unchecked(contract_addr));
                match info {
                    Ok(info) => QuerierResult::Ok(ContractResult::Ok(to_json_binary(&info).unwrap())),
                    Err(_) => QuerierResult::Ok(ContractResult::Err("Not found".to_string())),
                }
            },
            &_ => unimplemented!(),
        }
    }

    #[test]
    fn test_tax_condition_is_taxed() {

        let mut deps = cosmwasm_std::testing::mock_dependencies();
        deps.querier.update_wasm(|r| wasm_query_handler(r));
        let qw = QuerierWrapper::new(&deps.querier);

        let addr0 = Addr::unchecked("0");
        let addr1 = Addr::unchecked("1");
        let addr2 = Addr::unchecked("2");
        let addr3 = Addr::unchecked("3");

        // tax condition not fulfilled for any address
        let none_condition = TaxCondition::Never(TaxNeverCondition {});
        assert_eq!(none_condition.is_taxed(&qw, addr0.clone()), false);

        // tax condition only fulfilled for listed contract addresses
        let contract_code_condition = TaxCondition::ContractCode(TaxContractCodeCondition {
            code_ids: vec![0, 1],
            tax_rate: Decimal::percent(10),
        });

        // is a contract and is listed
        assert_eq!(contract_code_condition.is_taxed(&qw, addr0.clone()), true);
        // is a contract and is listed
        assert_eq!(contract_code_condition.is_taxed(&qw, addr1.clone()), true);
        // is a contract but not listed
        assert_eq!(contract_code_condition.is_taxed(&qw, addr2.clone()), false);
        // is not a contract
        assert_eq!(contract_code_condition.is_taxed(&qw, addr3.clone()), false);

        // tax condition fulfilled for all addresses
        let contract_code_condition = TaxCondition::Always(TaxAlwaysCondition {
            tax_rate: Decimal::percent(10),
        });

        assert_eq!(contract_code_condition.is_taxed(&qw, addr0.clone()), true);
        assert_eq!(contract_code_condition.is_taxed(&qw, addr1.clone()), true);
        assert_eq!(contract_code_condition.is_taxed(&qw, addr2.clone()), true);
        assert_eq!(contract_code_condition.is_taxed(&qw, addr3.clone()), true);

    }

    #[test]
    fn test_tax_condition_tax_rate() {

        let mut deps = cosmwasm_std::testing::mock_dependencies();
        deps.querier.update_wasm(|r| wasm_query_handler(r));
        let qw = QuerierWrapper::new(&deps.querier);

        let addr0 = Addr::unchecked("0");
        let addr1 = Addr::unchecked("1");
        let addr2 = Addr::unchecked("2");
        let addr3 = Addr::unchecked("3");

        // tax rate is zero for any address
        let none_condition = TaxCondition::Never(TaxNeverCondition {});
        assert_eq!(none_condition.tax_rate(&qw, addr0.clone()), Decimal::zero());

        // tax condition only fulfilled for listed contract addresses
        let contract_code_condition = TaxCondition::ContractCode(TaxContractCodeCondition {
            code_ids: vec![0, 1],
            tax_rate: Decimal::percent(10),
        });

        // is a contract and is listed
        assert_eq!(contract_code_condition.tax_rate(&qw, addr0.clone()), Decimal::percent(10));
        // is a contract and is listed
        assert_eq!(contract_code_condition.tax_rate(&qw, addr1.clone()), Decimal::percent(10));
        // is a contract but not listed
        assert_eq!(contract_code_condition.tax_rate(&qw, addr2.clone()), Decimal::zero());
        // is not a contract
        assert_eq!(contract_code_condition.tax_rate(&qw, addr3.clone()), Decimal::zero());

        // tax condition fulfilled for all addresses
        let contract_code_condition = TaxCondition::Always(TaxAlwaysCondition {
            tax_rate: Decimal::percent(10),
        });
        assert_eq!(contract_code_condition.tax_rate(&qw, addr0.clone()), Decimal::percent(10));
        assert_eq!(contract_code_condition.tax_rate(&qw, addr1.clone()), Decimal::percent(10));
        assert_eq!(contract_code_condition.tax_rate(&qw, addr2.clone()), Decimal::percent(10));
        assert_eq!(contract_code_condition.tax_rate(&qw, addr3.clone()), Decimal::percent(10));


    }

    #[test]
    fn test_tax_info_deduct_tax() {

        let mut deps = cosmwasm_std::testing::mock_dependencies();
        deps.querier.update_wasm(|r| wasm_query_handler(r));
        let qw = QuerierWrapper::new(&deps.querier);

        let addr0 = Addr::unchecked("0");
        let addr1 = Addr::unchecked("1");
        let addr2 = Addr::unchecked("2");
        let addr3 = Addr::unchecked("3");

        // == Test Tax Deduction for Tax Condition "Never"
        let tax_info = TaxInfo {
            src_cond: TaxCondition::Never(TaxNeverCondition {}),
            dst_cond: TaxCondition::Never(TaxNeverCondition {}),
            proceeds: addr0.clone(),
        };
        assert_eq!(tax_info.deduct_tax(&qw, addr0.clone(), Uint128::new(100)), Ok((Uint128::new(100), Uint128::zero())));

        // == Test Tax Deduction for Tax Condition "Contract Code"
        let tax_info_with_tax = TaxInfo {
            src_cond: TaxCondition::ContractCode(TaxContractCodeCondition {
                code_ids: vec![0, 1],
                tax_rate: Decimal::percent(10),
            }),
            dst_cond: TaxCondition::ContractCode(TaxContractCodeCondition {
                code_ids: vec![0, 1],
                tax_rate: Decimal::percent(10),
            }),
            proceeds: addr0.clone(),
        };

        // is listed contract but proceeds wallet -> no tax
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr0.clone(), Uint128::new(100)), Ok((Uint128::new(100), Uint128::new(0))));
        // is a contract and is listed -> tax
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr1.clone(), Uint128::new(100)), Ok((Uint128::new(90), Uint128::new(10))));
        // is a contract but not listed -> no tax
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr2.clone(), Uint128::new(100)), Ok((Uint128::new(100), Uint128::new(0))));
        // is not a contract -> no tax
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr3.clone(), Uint128::new(100)), Ok((Uint128::new(100), Uint128::new(0))));

        // == Test Tax Deduction for tax condition "always" ==
        let tax_info_with_tax = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition {
                tax_rate: Decimal::percent(10),
            }),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition {
                tax_rate: Decimal::percent(10),
            }),
            proceeds: addr0.clone(),
        };

        // is proceeds wallet -> no tax
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr0.clone(), Uint128::new(100)), Ok((Uint128::new(100), Uint128::new(0))));
        // is normal wallet -> tax
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr1.clone(), Uint128::new(100)), Ok((Uint128::new(90), Uint128::new(10))));
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr2.clone(), Uint128::new(100)), Ok((Uint128::new(90), Uint128::new(10))));
        assert_eq!(tax_info_with_tax.deduct_tax(&qw, addr3.clone(), Uint128::new(100)), Ok((Uint128::new(90), Uint128::new(10))));

    }

    #[test]
    fn test_tax_condition_validate() {
        assert_eq!(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}.validate(), false);
        assert_eq!(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}.validate(), true);
        assert_eq!(TaxNeverCondition{}.validate(), true);
    }

    #[test]
    fn test_tax_info_validate() {
        let invalid_tax_info1 = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
            proceeds: Addr::unchecked("blubb"),
        };
        let invalid_tax_info2 = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
            proceeds: Addr::unchecked("blubb"),
        };
        let invalid_tax_info3 = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(11)}),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
            proceeds: Addr::unchecked("blubb"),
        };
        let valid_tax_info = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(11)}),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
            proceeds: Addr::unchecked("blubb"),
        };
        assert_eq!(invalid_tax_info1.validate(), false);
        assert_eq!(invalid_tax_info2.validate(), false);
        assert_eq!(invalid_tax_info3.validate(), false);
        assert_eq!(valid_tax_info.validate(), true);
    }

    #[test]
    fn test_tax_map_validate() {
        let invalid_tax_info = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
            proceeds: Addr::unchecked("blubb"),
        };
        let valid_tax_info = TaxInfo {
            src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(11)}),
            dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
            proceeds: Addr::unchecked("blubb"),
        };
        let valid_tax_map = TaxMap {
            on_transfer: valid_tax_info.clone(),
            on_send: valid_tax_info.clone(),
            on_send_from: valid_tax_info.clone(),
            on_transfer_from: valid_tax_info.clone(),
        };
        let invalid_tax_map = TaxMap {
            on_transfer: valid_tax_info.clone(),
            on_send: invalid_tax_info.clone(),
            on_send_from: valid_tax_info.clone(),
            on_transfer_from: valid_tax_info.clone(),
        };
        assert_eq!(valid_tax_map.validate().is_ok(), true);
        assert_eq!(invalid_tax_map.validate().is_err(), true);
    }

}