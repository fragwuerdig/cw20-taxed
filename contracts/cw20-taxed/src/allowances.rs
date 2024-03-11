use cosmwasm_std::{
    attr, to_json_binary, Addr, Binary, BlockInfo, CosmosMsg, Deps, DepsMut, Env, MessageInfo, Response, StdError, StdResult, Storage, Uint128, WasmMsg
};
use cw20::{AllowanceResponse, Cw20ExecuteMsg, Cw20ReceiveMsg, Expiration};

use crate::msg::Cw20TaxedExecuteMsg as ExecuteMsg;

use crate::error::ContractError;
use crate::state::{ALLOWANCES, ALLOWANCES_SPENDER, BALANCES, TAX_INFO, TOKEN_INFO};

pub fn execute_increase_allowance(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    spender: String,
    amount: Uint128,
    expires: Option<Expiration>,
) -> Result<Response, ContractError> {
    let spender_addr = deps.api.addr_validate(&spender)?;
    if spender_addr == info.sender {
        return Err(ContractError::CannotSetOwnAccount {});
    }

    let update_fn = |allow: Option<AllowanceResponse>| -> Result<_, _> {
        let mut val = allow.unwrap_or_default();
        if let Some(exp) = expires {
            if exp.is_expired(&env.block) {
                return Err(ContractError::InvalidExpiration {});
            }
            val.expires = exp;
        }
        val.allowance += amount;
        Ok(val)
    };
    ALLOWANCES.update(deps.storage, (&info.sender, &spender_addr), update_fn)?;
    ALLOWANCES_SPENDER.update(deps.storage, (&spender_addr, &info.sender), update_fn)?;

    let res = Response::new().add_attributes(vec![
        attr("action", "increase_allowance"),
        attr("owner", info.sender),
        attr("spender", spender),
        attr("amount", amount),
    ]);
    Ok(res)
}

pub fn execute_decrease_allowance(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    spender: String,
    amount: Uint128,
    expires: Option<Expiration>,
) -> Result<Response, ContractError> {
    let spender_addr = deps.api.addr_validate(&spender)?;
    if spender_addr == info.sender {
        return Err(ContractError::CannotSetOwnAccount {});
    }

    let key = (&info.sender, &spender_addr);

    fn reverse<'a>(t: (&'a Addr, &'a Addr)) -> (&'a Addr, &'a Addr) {
        (t.1, t.0)
    }

    // load value and delete if it hits 0, or update otherwise
    let mut allowance = ALLOWANCES.load(deps.storage, key)?;
    if amount < allowance.allowance {
        // update the new amount
        allowance.allowance = allowance
            .allowance
            .checked_sub(amount)
            .map_err(StdError::overflow)?;
        if let Some(exp) = expires {
            if exp.is_expired(&env.block) {
                return Err(ContractError::InvalidExpiration {});
            }
            allowance.expires = exp;
        }
        ALLOWANCES.save(deps.storage, key, &allowance)?;
        ALLOWANCES_SPENDER.save(deps.storage, reverse(key), &allowance)?;
    } else {
        ALLOWANCES.remove(deps.storage, key);
        ALLOWANCES_SPENDER.remove(deps.storage, reverse(key));
    }

    let res = Response::new().add_attributes(vec![
        attr("action", "decrease_allowance"),
        attr("owner", info.sender),
        attr("spender", spender),
        attr("amount", amount),
    ]);
    Ok(res)
}

// this can be used to update a lower allowance - call bucket.update with proper keys
pub fn deduct_allowance(
    storage: &mut dyn Storage,
    owner: &Addr,
    spender: &Addr,
    block: &BlockInfo,
    amount: Uint128,
) -> Result<AllowanceResponse, ContractError> {
    let update_fn = |current: Option<AllowanceResponse>| -> _ {
        match current {
            Some(mut a) => {
                if a.expires.is_expired(block) {
                    Err(ContractError::Expired {})
                } else {
                    // deduct the allowance if enough
                    a.allowance = a
                        .allowance
                        .checked_sub(amount)
                        .map_err(StdError::overflow)?;
                    Ok(a)
                }
            }
            None => Err(ContractError::NoAllowance {}),
        }
    };
    ALLOWANCES.update(storage, (owner, spender), update_fn)?;
    ALLOWANCES_SPENDER.update(storage, (spender, owner), update_fn)
}

pub fn execute_transfer_from(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    owner: String,
    recipient: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let rcpt_addr = deps.api.addr_validate(&recipient)?;
    let owner_addr = deps.api.addr_validate(&owner)?;
    let map = TAX_INFO.load(deps.storage)?;
    let rcpt_proceeds = map.on_transfer_from.proceeds.clone().into_string(); 
    let (net, tax) = map.on_transfer_from.deduct_tax(&deps.querier, owner_addr.clone(), rcpt_addr.clone(), amount)?;

    // deduct allowance before doing anything else have enough allowance
    deduct_allowance(deps.storage, &owner_addr, &info.sender, &env.block, amount)?;

    // reduce owners balance
    BALANCES.update(
        deps.storage,
        &owner_addr,
        |balance: Option<Uint128>| -> StdResult<_> {
            Ok(balance.unwrap_or_default().checked_sub(amount)?)
        },
    )?;

    // move tax to token contract
    BALANCES.update(
        deps.storage,
        &env.contract.address,
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() + tax) },
    )?;

    // move net amount to receiver
    BALANCES.update(
        deps.storage,
        &rcpt_addr,
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() + net) },
    )?;

    // construct msg to send tax to proceeds wallet
    let tax_msg = CosmosMsg::Wasm( WasmMsg::Execute {
        contract_addr: env.contract.address.into(),
        msg: to_json_binary(
            &ExecuteMsg::Transfer {
                recipient: rcpt_proceeds.clone(),
                amount: tax,
        })?,
        funds: vec![],
    });

    let res = Response::new().add_attributes(vec![
        attr("action", "transfer_from"),
        attr("from", owner),
        attr("to", recipient),
        attr("by", info.sender),
        attr("amount", amount),
    ]);

    if tax.gt(&Uint128::zero()) {
        let tax_res = res.clone()
            .add_attribute("net", net)
            .add_attribute("tax", tax)
            .add_attribute("proceeds", &rcpt_proceeds)
            .add_message(tax_msg);
        return Ok(tax_res);
    }

    Ok(res)
}

pub fn execute_burn_from(
    deps: DepsMut,

    env: Env,
    info: MessageInfo,
    owner: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let owner_addr = deps.api.addr_validate(&owner)?;

    // deduct allowance before doing anything else have enough allowance
    deduct_allowance(deps.storage, &owner_addr, &info.sender, &env.block, amount)?;

    // lower balance
    BALANCES.update(
        deps.storage,
        &owner_addr,
        |balance: Option<Uint128>| -> StdResult<_> {
            Ok(balance.unwrap_or_default().checked_sub(amount)?)
        },
    )?;
    // reduce total_supply
    TOKEN_INFO.update(deps.storage, |mut meta| -> StdResult<_> {
        meta.total_supply = meta.total_supply.checked_sub(amount)?;
        Ok(meta)
    })?;

    let res = Response::new().add_attributes(vec![
        attr("action", "burn_from"),
        attr("from", owner),
        attr("by", info.sender),
        attr("amount", amount),
    ]);
    Ok(res)
}

pub fn execute_send_from(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    owner: String,
    contract: String,
    amount: Uint128,
    msg: Binary,
) -> Result<Response, ContractError> {
    let rcpt_addr = deps.api.addr_validate(&contract)?;
    let owner_addr = deps.api.addr_validate(&owner)?;
    let map = TAX_INFO.load(deps.storage)?;
    let rcpt_proceeds = map.on_send_from.proceeds.clone().into_string();
    let (net, tax) = map.on_send_from.deduct_tax(&deps.querier, info.sender.clone(), rcpt_addr.clone(), amount)?;

    // deduct allowance before doing anything else have enough allowance
    deduct_allowance(deps.storage, &owner_addr, &info.sender, &env.block, amount)?;

    // move net tokens to the contract
    BALANCES.update(
        deps.storage,
        &owner_addr.clone(),
        |balance: Option<Uint128>| -> StdResult<_> {
            Ok(balance.unwrap_or_default().checked_sub(amount)?)
        },
    )?;
    BALANCES.update(
        deps.storage,
        &rcpt_addr,
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() + net) },
    )?;

    // move tax to this token
    BALANCES.update(
        deps.storage,
        &env.contract.address,
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() + tax) },
    )?;

    // construct msg for net amount
    let net_msg = Cw20ReceiveMsg {
        sender: info.sender.clone().into(),
        amount: net,
        msg,
    }
    .into_cosmos_msg(contract)?;

    // construct msg to send tax to proceeds wallet
    let tax_msg = CosmosMsg::Wasm( WasmMsg::Execute {
        contract_addr: env.contract.address.into(),
        msg: to_json_binary(
            &ExecuteMsg::Transfer {
                recipient: rcpt_proceeds.clone(),
                amount: tax
        })?,
        funds: vec![],
    });

    // emit
    let res = Response::new()
        .add_attribute("action", "send_from")
        .add_attribute("from", &info.sender.clone().into_string())
        .add_attribute("to", &rcpt_addr)
        .add_attribute("by", &info.sender)
        .add_attribute("amount", amount)
        .add_message(net_msg);

    if tax.gt(&Uint128::zero()) {
        let tax_res = res.clone()
            .add_attribute("net", net)
            .add_attribute("tax", tax)
            .add_attribute("proceeds", &rcpt_proceeds)
            .add_message(tax_msg);
        return Ok(tax_res);
    }

    Ok(res)
}

pub fn query_allowance(deps: Deps, owner: String, spender: String) -> StdResult<AllowanceResponse> {
    let owner_addr = deps.api.addr_validate(&owner)?;
    let spender_addr = deps.api.addr_validate(&spender)?;
    let allowance = ALLOWANCES
        .may_load(deps.storage, (&owner_addr, &spender_addr))?
        .unwrap_or_default();
    Ok(allowance)
}

#[cfg(test)]
mod tests {
    use super::*;

    use cosmwasm_std::testing::{mock_dependencies_with_balance, mock_env, mock_info};
    use cosmwasm_std::{coins, CosmosMsg, Decimal, Empty, SubMsg, Timestamp, WasmMsg};
    use cw20::{Cw20Coin, TokenInfoResponse};
    use cw20_base::msg;

    use crate::contract::{execute, instantiate, query_balance, query_token_info};
    use crate::msg::{Cw20TaxedExecuteMsg as ExecuteMsg, InstantiateMsg};
    use crate::tax::{TaxAlwaysCondition, TaxCondition, TaxInfo, TaxMap, TaxNeverCondition};

    fn get_balance<T: Into<String>>(deps: Deps, address: T) -> Uint128 {
        query_balance(deps, address.into()).unwrap().balance
    }

    // this will set up the instantiation for other tests
    fn do_instantiate<T: Into<String>>(
        mut deps: DepsMut,
        addr: T,
        amount: Uint128,
    ) -> TokenInfoResponse {
        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr.into(),
                amount,
            }],
            mint: None,
            marketing: None,
            tax_map: None, 
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        instantiate(deps.branch(), env, info, instantiate_msg).unwrap();
        query_token_info(deps.as_ref()).unwrap()
    }

    fn do_instantiate_with_tax_on_transfer_from(
        mut deps: DepsMut,
        addr: &str,
        amount: Uint128,
    ) -> TokenInfoResponse {

        // simple flat p2p tax
        let tax_map_in = Some(TaxMap{
            on_transfer: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_send: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_send_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_transfer_from: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            admin: Addr::unchecked(""),
        });

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr.to_string(),
                amount,
            }],
            mint: None,
            marketing: None,
            tax_map: tax_map_in,
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.branch(), env, info, instantiate_msg).unwrap();
        assert_eq!(0, res.messages.len());

        let meta = query_token_info(deps.as_ref()).unwrap();
        assert_eq!(
            meta,
            TokenInfoResponse {
                name: "Auto Gen".to_string(),
                symbol: "AUTO".to_string(),
                decimals: 3,
                total_supply: amount,
            }
        );
        assert_eq!(get_balance(deps.as_ref(), addr), amount);
        meta
    }

    fn do_instantiate_with_tax_on_send_from(
        mut deps: DepsMut,
        addr: &str,
        amount: Uint128,
    ) -> TokenInfoResponse {

        // simple flat p2p tax
        let tax_map_in = Some(TaxMap{
            on_transfer: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_send: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_send_from: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            on_transfer_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            admin: Addr::unchecked(""),
        });

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr.to_string(),
                amount,
            }],
            mint: None,
            marketing: None,
            tax_map: tax_map_in,
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.branch(), env, info, instantiate_msg).unwrap();
        assert_eq!(0, res.messages.len());

        let meta = query_token_info(deps.as_ref()).unwrap();
        assert_eq!(
            meta,
            TokenInfoResponse {
                name: "Auto Gen".to_string(),
                symbol: "AUTO".to_string(),
                decimals: 3,
                total_supply: amount,
            }
        );
        assert_eq!(get_balance(deps.as_ref(), addr), amount);
        meta
    }

    #[test]
    fn transfer_from_with_tax() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
        let addr0 = String::from("addr0000");
        let addr1 = String::from("addr0001");
        let addr2 = String::from("addr0002");
        let amount1 = Uint128::from(12340000u128);
        let transfer = Uint128::from(76543u128);
        let expected_remainder = amount1.checked_sub(transfer).unwrap();
        let expected_tax = Uint128::from(7654u128);
        let expected_net = Uint128::from(68889u128);
        let expected_tfer_msg = ExecuteMsg::Transfer {
            recipient: String::from("proceeds"),
            amount: expected_tax.clone(),
        };
        let expected_proceeds_msg: CosmosMsg<Empty> = CosmosMsg::Wasm( WasmMsg::Execute {
            contract_addr: String::from("cosmos2contract"),
            msg: to_json_binary(&expected_tfer_msg).unwrap(),
            funds: vec![],
        });

        do_instantiate_with_tax_on_transfer_from(deps.as_mut(), &addr1, amount1);

        // increase allowance
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: addr0.clone(),
            amount: transfer,
            expires: None,
        };
        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();

        // test valid transfer
        let info = mock_info(addr0.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::TransferFrom {
            owner: addr1.clone(),
            recipient: addr2.clone(),
            amount: transfer,
        };
        let res = execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();
        assert_eq!(res.messages.len(), 1); //expecting proceeds message
        assert_eq!(res.messages[0].clone().msg, expected_proceeds_msg);
        assert_eq!(get_balance(deps.as_ref(), addr1.clone()), expected_remainder);
        assert_eq!(get_balance(deps.as_ref(), addr2.clone()), expected_net);
        assert_eq!(get_balance(deps.as_ref(), "cosmos2contract"), expected_tax);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );

        // test proceedings of tax were successful
        let proceeds_info = mock_info("cosmos2contract", &[]);
        let tax_res = execute(deps.as_mut(), env.clone(), proceeds_info, expected_tfer_msg).unwrap();
        assert_eq!(tax_res.messages.len(), 0); //expecting no furhter messages
        assert_eq!(get_balance(deps.as_ref(), addr1.clone()), expected_remainder);
        assert_eq!(get_balance(deps.as_ref(), addr2.clone()), expected_net);
        assert_eq!(get_balance(deps.as_ref(), "cosmos2contract"), Uint128::zero());
        assert_eq!(get_balance(deps.as_ref(), "proceeds"), expected_tax);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );

    }

    #[test]
    fn send_from_with_tax() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
        let addr0 = String::from("addr0000");
        let addr1 = String::from("addr0001");
        let contract = String::from("addr0002");
        let amount1 = Uint128::from(12340000u128);
        let transfer = Uint128::from(76543u128);
        let expected_remainder = amount1.checked_sub(transfer).unwrap();
        let expected_tax = Uint128::from(7654u128);
        let expected_net = Uint128::from(68889u128);
        let expected_tfer_msg = ExecuteMsg::Transfer {
            recipient: String::from("proceeds"),
            amount: expected_tax.clone(),
        };
        let send_msg = Binary::from(r#"{"some":123}"#.as_bytes());
        let expected_proceeds_msg: CosmosMsg<Empty> = CosmosMsg::Wasm( WasmMsg::Execute {
            contract_addr: String::from("cosmos2contract"),
            msg: to_json_binary(&expected_tfer_msg).unwrap(),
            funds: vec![],
        });

        do_instantiate_with_tax_on_send_from(deps.as_mut(), &addr1, amount1);

        // increase allowance
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: addr0.clone(),
            amount: transfer,
            expires: None,
        };
        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();

        // valid send
        let info = mock_info(addr0.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::SendFrom {
            owner: addr1.clone(),
            contract: contract.clone(),
            amount: transfer,
            msg: send_msg.clone(),
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
        assert_eq!(res.messages.len(), 2);

        // ensure proper send message sent
        // this is the message we want delivered to the other side
        let binary_msg = Cw20ReceiveMsg {
            sender: addr0.clone(),
            amount: expected_net,
            msg: send_msg,
        }
        .into_binary()
        .unwrap();
        // and this is how it must be wrapped for the vm to process it
        assert_eq!(
            res.messages[0],
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: contract.clone(),
                msg: binary_msg,
                funds: vec![],
            }))
        );

        // ensure balance and tax is properly transferred
        assert_eq!(res.messages[1].clone().msg, expected_proceeds_msg);
        assert_eq!(get_balance(deps.as_ref(), addr1.clone()), expected_remainder);
        assert_eq!(get_balance(deps.as_ref(), contract.clone()), expected_net);
        assert_eq!(get_balance(deps.as_ref(), "cosmos2contract"), expected_tax);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );

        // test proceedings of tax were successful
        let proceeds_info = mock_info("cosmos2contract", &[]);
        let tax_res = execute(deps.as_mut(), env.clone(), proceeds_info, expected_tfer_msg).unwrap();
        assert_eq!(tax_res.messages.len(), 0); //expecting no furhter messages
        assert_eq!(get_balance(deps.as_ref(), addr1.clone()), expected_remainder);
        assert_eq!(get_balance(deps.as_ref(), contract.clone()), expected_net);
        assert_eq!(get_balance(deps.as_ref(), "cosmos2contract"), Uint128::zero());
        assert_eq!(get_balance(deps.as_ref(), "proceeds"), expected_tax);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );

    }

    #[test]
    fn increase_decrease_allowances() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));

        let owner = String::from("addr0001");
        let spender = String::from("addr0002");
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        do_instantiate(deps.as_mut(), owner.clone(), Uint128::new(12340000));

        // no allowance to start
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        assert_eq!(allowance, AllowanceResponse::default());

        // set allowance with height expiration
        let allow1 = Uint128::new(7777);
        let expires = Expiration::AtHeight(123_456);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow1,
            expires: Some(expires),
        };
        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        assert_eq!(
            allowance,
            AllowanceResponse {
                allowance: allow1,
                expires
            }
        );

        // decrease it a bit with no expire set - stays the same
        let lower = Uint128::new(4444);
        let allow2 = allow1.checked_sub(lower).unwrap();
        let msg = ExecuteMsg::DecreaseAllowance {
            spender: spender.clone(),
            amount: lower,
            expires: None,
        };
        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        assert_eq!(
            allowance,
            AllowanceResponse {
                allowance: allow2,
                expires
            }
        );

        // increase it some more and override the expires
        let raise = Uint128::new(87654);
        let allow3 = allow2 + raise;
        let new_expire = Expiration::AtTime(Timestamp::from_seconds(8888888888));
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: raise,
            expires: Some(new_expire),
        };
        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        assert_eq!(
            allowance,
            AllowanceResponse {
                allowance: allow3,
                expires: new_expire
            }
        );

        // decrease it below 0
        let msg = ExecuteMsg::DecreaseAllowance {
            spender: spender.clone(),
            amount: Uint128::new(99988647623876347),
            expires: None,
        };
        execute(deps.as_mut(), env, info, msg).unwrap();
        let allowance = query_allowance(deps.as_ref(), owner, spender).unwrap();
        assert_eq!(allowance, AllowanceResponse::default());
    }

    #[test]
    fn allowances_independent() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));

        let owner = String::from("addr0001");
        let spender = String::from("addr0002");
        let spender2 = String::from("addr0003");
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        do_instantiate(deps.as_mut(), &owner, Uint128::new(12340000));

        // no allowance to start
        assert_eq!(
            query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap(),
            AllowanceResponse::default()
        );
        assert_eq!(
            query_allowance(deps.as_ref(), owner.clone(), spender2.clone()).unwrap(),
            AllowanceResponse::default()
        );
        assert_eq!(
            query_allowance(deps.as_ref(), spender.clone(), spender2.clone()).unwrap(),
            AllowanceResponse::default()
        );

        // set allowance with height expiration
        let allow1 = Uint128::new(7777);
        let expires = Expiration::AtHeight(123_456);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow1,
            expires: Some(expires),
        };
        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();

        // set other allowance with no expiration
        let allow2 = Uint128::new(87654);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender2.clone(),
            amount: allow2,
            expires: None,
        };
        execute(deps.as_mut(), env, info, msg).unwrap();

        // check they are proper
        let expect_one = AllowanceResponse {
            allowance: allow1,
            expires,
        };
        let expect_two = AllowanceResponse {
            allowance: allow2,
            expires: Expiration::Never {},
        };
        assert_eq!(
            query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap(),
            expect_one
        );
        assert_eq!(
            query_allowance(deps.as_ref(), owner.clone(), spender2.clone()).unwrap(),
            expect_two
        );
        assert_eq!(
            query_allowance(deps.as_ref(), spender.clone(), spender2.clone()).unwrap(),
            AllowanceResponse::default()
        );

        // also allow spender -> spender2 with no interference
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let allow3 = Uint128::new(1821);
        let expires3 = Expiration::AtTime(Timestamp::from_seconds(3767626296));
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender2.clone(),
            amount: allow3,
            expires: Some(expires3),
        };
        execute(deps.as_mut(), env, info, msg).unwrap();
        let expect_three = AllowanceResponse {
            allowance: allow3,
            expires: expires3,
        };
        assert_eq!(
            query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap(),
            expect_one
        );
        assert_eq!(
            query_allowance(deps.as_ref(), owner, spender2.clone()).unwrap(),
            expect_two
        );
        assert_eq!(
            query_allowance(deps.as_ref(), spender, spender2).unwrap(),
            expect_three
        );
    }

    #[test]
    fn no_self_allowance() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));

        let owner = String::from("addr0001");
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        do_instantiate(deps.as_mut(), &owner, Uint128::new(12340000));

        // self-allowance
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: owner.clone(),
            amount: Uint128::new(7777),
            expires: None,
        };
        let err = execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap_err();
        assert_eq!(err, ContractError::CannotSetOwnAccount {});

        // decrease self-allowance
        let msg = ExecuteMsg::DecreaseAllowance {
            spender: owner,
            amount: Uint128::new(7777),
            expires: None,
        };
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::CannotSetOwnAccount {});
    }

    #[test]
    fn transfer_from_respects_limits() {
        let mut deps = mock_dependencies_with_balance(&[]);
        let owner = String::from("addr0001");
        let spender = String::from("addr0002");
        let rcpt = String::from("addr0003");

        let start = Uint128::new(999999);
        do_instantiate(deps.as_mut(), &owner, start);

        // provide an allowance
        let allow1 = Uint128::new(77777);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow1,
            expires: None,
        };
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        execute(deps.as_mut(), env, info, msg).unwrap();

        // valid transfer of part of the allowance
        let transfer = Uint128::new(44444);
        let msg = ExecuteMsg::TransferFrom {
            owner: owner.clone(),
            recipient: rcpt.clone(),
            amount: transfer,
        };
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(res.attributes[0], attr("action", "transfer_from"));

        // make sure money arrived
        assert_eq!(
            get_balance(deps.as_ref(), owner.clone()),
            start.checked_sub(transfer).unwrap()
        );
        assert_eq!(get_balance(deps.as_ref(), rcpt.clone()), transfer);

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        let expect = AllowanceResponse {
            allowance: allow1.checked_sub(transfer).unwrap(),
            expires: Expiration::Never {},
        };
        assert_eq!(expect, allowance);

        // cannot send more than the allowance
        let msg = ExecuteMsg::TransferFrom {
            owner: owner.clone(),
            recipient: rcpt.clone(),
            amount: Uint128::new(33443),
        };
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));

        // let us increase limit, but set the expiration to expire in the next block
        let info = mock_info(owner.as_ref(), &[]);
        let mut env = mock_env();
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: Uint128::new(1000),
            expires: Some(Expiration::AtHeight(env.block.height + 1)),
        };
        execute(deps.as_mut(), env.clone(), info, msg).unwrap();

        env.block.height += 1;

        // we should now get the expiration error
        let msg = ExecuteMsg::TransferFrom {
            owner,
            recipient: rcpt,
            amount: Uint128::new(33443),
        };
        let info = mock_info(spender.as_ref(), &[]);
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Expired {});
    }

    #[test]
    fn burn_from_respects_limits() {
        let mut deps = mock_dependencies_with_balance(&[]);
        let owner = String::from("addr0001");
        let spender = String::from("addr0002");

        let start = Uint128::new(999999);
        do_instantiate(deps.as_mut(), &owner, start);

        // provide an allowance
        let allow1 = Uint128::new(77777);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow1,
            expires: None,
        };
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        execute(deps.as_mut(), env, info, msg).unwrap();

        // valid burn of part of the allowance
        let transfer = Uint128::new(44444);
        let msg = ExecuteMsg::BurnFrom {
            owner: owner.clone(),
            amount: transfer,
        };
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(res.attributes[0], attr("action", "burn_from"));

        // make sure money burnt
        assert_eq!(
            get_balance(deps.as_ref(), owner.clone()),
            start.checked_sub(transfer).unwrap()
        );

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        let expect = AllowanceResponse {
            allowance: allow1.checked_sub(transfer).unwrap(),
            expires: Expiration::Never {},
        };
        assert_eq!(expect, allowance);

        // cannot burn more than the allowance
        let msg = ExecuteMsg::BurnFrom {
            owner: owner.clone(),
            amount: Uint128::new(33443),
        };
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));

        // let us increase limit, but set the expiration to expire in the next block
        let info = mock_info(owner.as_ref(), &[]);
        let mut env = mock_env();
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: Uint128::new(1000),
            expires: Some(Expiration::AtHeight(env.block.height + 1)),
        };
        execute(deps.as_mut(), env.clone(), info, msg).unwrap();

        // increase block height, so the limit is expired now
        env.block.height += 1;

        // we should now get the expiration error
        let msg = ExecuteMsg::BurnFrom {
            owner,
            amount: Uint128::new(33443),
        };
        let info = mock_info(spender.as_ref(), &[]);
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Expired {});
    }

    #[test]
    fn send_from_respects_limits() {
        let mut deps = mock_dependencies_with_balance(&[]);
        let owner = String::from("addr0001");
        let spender = String::from("addr0002");
        let contract = String::from("cool-dex");
        let send_msg = Binary::from(r#"{"some":123}"#.as_bytes());

        let start = Uint128::new(999999);
        do_instantiate(deps.as_mut(), &owner, start);

        // provide an allowance
        let allow1 = Uint128::new(77777);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow1,
            expires: None,
        };
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        execute(deps.as_mut(), env, info, msg).unwrap();

        // valid send of part of the allowance
        let transfer = Uint128::new(44444);
        let msg = ExecuteMsg::SendFrom {
            owner: owner.clone(),
            amount: transfer,
            contract: contract.clone(),
            msg: send_msg.clone(),
        };
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(res.attributes[0], attr("action", "send_from"));
        assert_eq!(1, res.messages.len());

        // we record this as sent by the one who requested, not the one who was paying
        let binary_msg = Cw20ReceiveMsg {
            sender: spender.clone(),
            amount: transfer,
            msg: send_msg.clone(),
        }
        .into_binary()
        .unwrap();
        assert_eq!(
            res.messages[0],
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: contract.clone(),
                msg: binary_msg,
                funds: vec![],
            }))
        );

        // make sure money sent
        assert_eq!(
            get_balance(deps.as_ref(), owner.clone()),
            start.checked_sub(transfer).unwrap()
        );
        assert_eq!(get_balance(deps.as_ref(), contract.clone()), transfer);

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        let expect = AllowanceResponse {
            allowance: allow1.checked_sub(transfer).unwrap(),
            expires: Expiration::Never {},
        };
        assert_eq!(expect, allowance);

        // cannot send more than the allowance
        let msg = ExecuteMsg::SendFrom {
            owner: owner.clone(),
            amount: Uint128::new(33443),
            contract: contract.clone(),
            msg: send_msg.clone(),
        };
        let info = mock_info(spender.as_ref(), &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));

        // let us increase limit, but set the expiration to the next block
        let info = mock_info(owner.as_ref(), &[]);
        let mut env = mock_env();
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: Uint128::new(1000),
            expires: Some(Expiration::AtHeight(env.block.height + 1)),
        };
        execute(deps.as_mut(), env.clone(), info, msg).unwrap();

        // increase block height, so the limit is expired now
        env.block.height += 1;

        // we should now get the expiration error
        let msg = ExecuteMsg::SendFrom {
            owner,
            amount: Uint128::new(33443),
            contract,
            msg: send_msg,
        };
        let info = mock_info(spender.as_ref(), &[]);
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Expired {});
    }

    #[test]
    fn no_past_expiration() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));

        let owner = String::from("addr0001");
        let spender = String::from("addr0002");
        let info = mock_info(owner.as_ref(), &[]);
        let env = mock_env();
        do_instantiate(deps.as_mut(), owner.clone(), Uint128::new(12340000));

        // set allowance with height expiration at current block height
        let expires = Expiration::AtHeight(env.block.height);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: Uint128::new(7777),
            expires: Some(expires),
        };

        // ensure it is rejected
        assert_eq!(
            Err(ContractError::InvalidExpiration {}),
            execute(deps.as_mut(), env.clone(), info.clone(), msg)
        );

        // set allowance with time expiration in the past
        let expires = Expiration::AtTime(env.block.time.minus_seconds(1));
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: Uint128::new(7777),
            expires: Some(expires),
        };

        // ensure it is rejected
        assert_eq!(
            Err(ContractError::InvalidExpiration {}),
            execute(deps.as_mut(), env.clone(), info.clone(), msg)
        );

        // set allowance with height expiration at next block height
        let expires = Expiration::AtHeight(env.block.height + 1);
        let allow = Uint128::new(7777);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow,
            expires: Some(expires),
        };

        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        assert_eq!(
            allowance,
            AllowanceResponse {
                allowance: allow,
                expires
            }
        );

        // set allowance with time expiration in the future
        let expires = Expiration::AtTime(env.block.time.plus_seconds(10));
        let allow = Uint128::new(7777);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow,
            expires: Some(expires),
        };

        execute(deps.as_mut(), env.clone(), info.clone(), msg).unwrap();

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner.clone(), spender.clone()).unwrap();
        assert_eq!(
            allowance,
            AllowanceResponse {
                allowance: allow + allow, // we increased twice
                expires
            }
        );

        // decrease with height expiration at current block height
        let expires = Expiration::AtHeight(env.block.height);
        let allow = Uint128::new(7777);
        let msg = ExecuteMsg::IncreaseAllowance {
            spender: spender.clone(),
            amount: allow,
            expires: Some(expires),
        };

        // ensure it is rejected
        assert_eq!(
            Err(ContractError::InvalidExpiration {}),
            execute(deps.as_mut(), env.clone(), info.clone(), msg)
        );

        // decrease with height expiration at next block height
        let expires = Expiration::AtHeight(env.block.height + 1);
        let allow = Uint128::new(7777);
        let msg = ExecuteMsg::DecreaseAllowance {
            spender: spender.clone(),
            amount: allow,
            expires: Some(expires),
        };

        execute(deps.as_mut(), env, info, msg).unwrap();

        // ensure it looks good
        let allowance = query_allowance(deps.as_ref(), owner, spender).unwrap();
        assert_eq!(
            allowance,
            AllowanceResponse {
                allowance: allow,
                expires
            }
        );
    }
}
