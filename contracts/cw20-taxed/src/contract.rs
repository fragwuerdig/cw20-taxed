#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::Order::Ascending;
use cosmwasm_std::{
    to_json_binary, Addr, Binary, CosmosMsg, Decimal, Deps, DepsMut, Empty, Env, MessageInfo, QuerierWrapper, Response, StdError, StdResult, Uint128, WasmMsg
};

use cw2::{ensure_from_older_version, set_contract_version};
use cw20::{
    BalanceResponse, Cw20Coin, Cw20ReceiveMsg, DownloadLogoResponse, EmbeddedLogo, Logo, LogoInfo,
    MarketingInfoResponse, MinterResponse, TokenInfoResponse,
};


use crate::allowances::{
    execute_burn_from, execute_decrease_allowance, execute_increase_allowance, execute_send_from,
    execute_transfer_from, query_allowance,
};
use crate::enumerable::{query_all_accounts, query_owner_allowances, query_spender_allowances};
use crate::error::ContractError;
use crate::msg::{Cw20TaxedExecuteMsg as ExecuteMsg, InstantiateMsg, MigrateMsg, QueryMsg};
use crate::state::{
    self, MinterData, TokenInfo, ALLOWANCES, ALLOWANCES_SPENDER, BALANCES, LOGO, MARKETING_INFO, TAX_INFO, TOKEN_INFO
};

use crate::tax::{self, TaxMap};

// version info for migration info
pub const CONTRACT_NAME: &str = "crates.io:cw20-base";
pub const CONTRACT_NAME_TERRAPORT: &str = "crates.io:terraport-token";
pub const CONTRACT_NAME_TERRASWAP: &str = "crates.io:terraswap-token";
pub const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const LOGO_SIZE_CAP: usize = 5 * 1024;

/// Checks if data starts with XML preamble
fn verify_xml_preamble(data: &[u8]) -> Result<(), ContractError> {
    // The easiest way to perform this check would be just match on regex, however regex
    // compilation is heavy and probably not worth it.

    let preamble = data
        .split_inclusive(|c| *c == b'>')
        .next()
        .ok_or(ContractError::InvalidXmlPreamble {})?;

    const PREFIX: &[u8] = b"<?xml ";
    const POSTFIX: &[u8] = b"?>";

    if !(preamble.starts_with(PREFIX) && preamble.ends_with(POSTFIX)) {
        Err(ContractError::InvalidXmlPreamble {})
    } else {
        Ok(())
    }

    // Additionally attributes format could be validated as they are well defined, as well as
    // comments presence inside of preable, but it is probably not worth it.
}

/// Validates XML logo
fn verify_xml_logo(logo: &[u8]) -> Result<(), ContractError> {
    verify_xml_preamble(logo)?;

    if logo.len() > LOGO_SIZE_CAP {
        Err(ContractError::LogoTooBig {})
    } else {
        Ok(())
    }
}

/// Validates png logo
fn verify_png_logo(logo: &[u8]) -> Result<(), ContractError> {
    // PNG header format:
    // 0x89 - magic byte, out of ASCII table to fail on 7-bit systems
    // "PNG" ascii representation
    // [0x0d, 0x0a] - dos style line ending
    // 0x1a - dos control character, stop displaying rest of the file
    // 0x0a - unix style line ending
    const HEADER: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if logo.len() > LOGO_SIZE_CAP {
        Err(ContractError::LogoTooBig {})
    } else if !logo.starts_with(&HEADER) {
        Err(ContractError::InvalidPngHeader {})
    } else {
        Ok(())
    }
}

/// Checks if passed logo is correct, and if not, returns an error
fn verify_logo(logo: &Logo) -> Result<(), ContractError> {
    match logo {
        Logo::Embedded(EmbeddedLogo::Svg(logo)) => verify_xml_logo(logo),
        Logo::Embedded(EmbeddedLogo::Png(logo)) => verify_png_logo(logo),
        Logo::Url(_) => Ok(()), // Any reasonable url validation would be regex based, probably not worth it
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    mut deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    
    // check valid token info
    msg.validate()?;
    
    // create initial accounts
    let total_supply = create_accounts(&mut deps, &msg.initial_balances)?;

    if let Some(limit) = msg.get_cap() {
        if total_supply > limit {
            return Err(StdError::generic_err("Initial supply greater than cap").into());
        }
    }

    let mint = match msg.mint {
        Some(m) => Some(MinterData {
            minter: deps.api.addr_validate(&m.minter)?,
            cap: m.cap,
        }),
        None => None,
    };

    if let Some(marketing) = msg.marketing {
        let logo = if let Some(logo) = marketing.logo {
            verify_logo(&logo)?;
            LOGO.save(deps.storage, &logo)?;

            match logo {
                Logo::Url(url) => Some(LogoInfo::Url(url)),
                Logo::Embedded(_) => Some(LogoInfo::Embedded),
            }
        } else {
            None
        };

        let data = MarketingInfoResponse {
            project: marketing.project,
            description: marketing.description,
            marketing: marketing
                .marketing
                .map(|addr| deps.api.addr_validate(&addr))
                .transpose()?,
            logo,
        };
        MARKETING_INFO.save(deps.storage, &data)?;
    }

    // store token info
    let data = TokenInfo {
        name: msg.name,
        symbol: msg.symbol,
        decimals: msg.decimals,
        total_supply,
        mint,
    };
    TOKEN_INFO.save(deps.storage, &data)?;

    let tax_info = match msg.tax_map {
        Some(x) => x,
        None => TaxMap::default(),
    };
    tax_info.validate()?;
    TAX_INFO.save(deps.storage, &tax_info)?;

    Ok(Response::default())
}

pub fn create_accounts(
    deps: &mut DepsMut,
    accounts: &[Cw20Coin],
) -> Result<Uint128, ContractError> {
    validate_accounts(accounts)?;

    let mut total_supply = Uint128::zero();
    for row in accounts {
        let address = deps.api.addr_validate(&row.address)?;
        BALANCES.save(deps.storage, &address, &row.amount)?;
        total_supply += row.amount;
    }

    Ok(total_supply)
}

pub fn validate_accounts(accounts: &[Cw20Coin]) -> Result<(), ContractError> {
    let mut addresses = accounts.iter().map(|c| &c.address).collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();

    if addresses.len() != accounts.len() {
        Err(ContractError::DuplicateInitialBalanceAddresses {})
    } else {
        Ok(())
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Transfer { recipient, amount } => {
            execute_transfer(deps, env, info, recipient, amount)
        }
        ExecuteMsg::Burn { amount } => execute_burn(deps, env, info, amount),
        ExecuteMsg::Send {
            contract,
            amount,
            msg,
        } => execute_send(deps, env, info, contract, amount, msg),
        ExecuteMsg::Mint { recipient, amount } => execute_mint(deps, env, info, recipient, amount),
        ExecuteMsg::IncreaseAllowance {
            spender,
            amount,
            expires,
        } => execute_increase_allowance(deps, env, info, spender, amount, expires),
        ExecuteMsg::DecreaseAllowance {
            spender,
            amount,
            expires,
        } => execute_decrease_allowance(deps, env, info, spender, amount, expires),
        ExecuteMsg::TransferFrom {
            owner,
            recipient,
            amount,
        } => execute_transfer_from(deps, env, info, owner, recipient, amount),
        ExecuteMsg::BurnFrom { owner, amount } => execute_burn_from(deps, env, info, owner, amount),
        ExecuteMsg::SendFrom {
            owner,
            contract,
            amount,
            msg,
        } => execute_send_from(deps, env, info, owner, contract, amount, msg),
        ExecuteMsg::UpdateMarketing {
            project,
            description,
            marketing,
        } => execute_update_marketing(deps, env, info, project, description, marketing),
        ExecuteMsg::UploadLogo(logo) => execute_upload_logo(deps, env, info, logo),
        ExecuteMsg::UpdateMinter { new_minter } => {
            execute_update_minter(deps, env, info, new_minter)
        }

        // Tax related extension
        ExecuteMsg::SetTaxMap { tax_map } => execute_set_tax_map(deps, env, info, tax_map),
        ExecuteMsg::SetTaxAdmin { tax_admin } => execute_set_tax_admin(deps, env, info, tax_admin),
    }
}

pub fn execute_set_tax_map(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    tax_map: Option<TaxMap>
) -> Result<Response, ContractError> {
    let curr_tax_map = TAX_INFO.load(deps.storage)?;
    if curr_tax_map.admin != info.sender {
        return Err(ContractError::Unauthorized {  })
    }
    let new_tax_map = match tax_map {
        Some(x) => x,
        None => {
            // reset default but preserve admin
            let mut def = TaxMap::default();
            def.admin = curr_tax_map.admin;
            def
        },
    };

    new_tax_map.validate()?;
    TAX_INFO.save(deps.storage, &new_tax_map)?;

    Ok(Response::new()
        .add_attribute("admin", new_tax_map.admin)
    )
}

pub fn execute_set_tax_admin(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    tax_admin: Option<String>
) -> Result<Response, ContractError> {
    let mut tax_map = TAX_INFO.load(deps.storage)?;
    if tax_map.admin != info.sender {
        return Err(ContractError::Unauthorized {  })
    }
    tax_map.admin = match tax_admin {
        Some(x) => deps.api.addr_validate(&x)?,
        None => Addr::unchecked(""),
    };
    TAX_INFO.save(deps.storage, &tax_map)?;

    Ok(Response::new()
        .add_attribute("admin", tax_map.admin)
    )
}

pub fn execute_transfer(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    recipient: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let rcpt_addr = deps.api.addr_validate(&recipient)?;
    let map = TAX_INFO.load(deps.storage)?;
    let rcpt_proceeds = map.on_transfer.proceeds.clone().into_string(); 
    let (net, tax) = map.on_transfer.deduct_tax(&deps.querier, info.sender.clone(), rcpt_addr.clone(), amount)?;
    
    // remove tokens from sender balance
    BALANCES.update(
        deps.storage,
        &info.sender,
        |balance: Option<Uint128>| -> StdResult<_> {
            Ok(balance.unwrap_or_default().checked_sub(amount)?)
        },
    )?;

    // move tax to contract
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

    let res = Response::new()
        .add_attribute("action", "transfer")
        .add_attribute("from", info.sender)
        .add_attribute("to", recipient)
        .add_attribute("amount", amount);

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

pub fn execute_burn(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    amount: Uint128,
) -> Result<Response, ContractError> {
    // lower balance
    BALANCES.update(
        deps.storage,
        &info.sender,
        |balance: Option<Uint128>| -> StdResult<_> {
            Ok(balance.unwrap_or_default().checked_sub(amount)?)
        },
    )?;
    // reduce total_supply
    TOKEN_INFO.update(deps.storage, |mut info| -> StdResult<_> {
        info.total_supply = info.total_supply.checked_sub(amount)?;
        Ok(info)
    })?;

    let res = Response::new()
        .add_attribute("action", "burn")
        .add_attribute("from", info.sender)
        .add_attribute("amount", amount);
    Ok(res)
}

pub fn execute_mint(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    recipient: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let mut config = TOKEN_INFO
        .may_load(deps.storage)?
        .ok_or(ContractError::Unauthorized {})?;

    if config
        .mint
        .as_ref()
        .ok_or(ContractError::Unauthorized {})?
        .minter
        != info.sender
    {
        return Err(ContractError::Unauthorized {});
    }

    // update supply and enforce cap
    config.total_supply += amount;
    if let Some(limit) = config.get_cap() {
        if config.total_supply > limit {
            return Err(ContractError::CannotExceedCap {});
        }
    }
    TOKEN_INFO.save(deps.storage, &config)?;

    // add amount to recipient balance
    let rcpt_addr = deps.api.addr_validate(&recipient)?;
    BALANCES.update(
        deps.storage,
        &rcpt_addr,
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() + amount) },
    )?;

    let res = Response::new()
        .add_attribute("action", "mint")
        .add_attribute("to", recipient)
        .add_attribute("amount", amount);
    Ok(res)
}

pub fn execute_send(
    deps: DepsMut<Empty>,
    env: Env,
    info: MessageInfo,
    contract: String,
    amount: Uint128,
    msg: Binary,
) -> Result<Response, ContractError> {
    let rcpt_addr = deps.api.addr_validate(&contract.clone())?;
    let map = TAX_INFO.load(deps.storage)?;
    let rcpt_proceeds = map.on_send.proceeds.clone().into_string();
    let rcpt = deps.api.addr_validate(contract.clone().as_str())?;   
    let (net, tax) = map.on_send.deduct_tax(&deps.querier, info.sender.clone(), rcpt, amount)?;

    // move net tokens to the contract
    BALANCES.update(
        deps.storage,
        &info.sender,
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
        .add_attribute("action", "send")
        .add_attribute("from", &info.sender.clone().into_string())
        .add_attribute("to", &rcpt_addr)
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

pub fn execute_update_minter(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    new_minter: Option<String>,
) -> Result<Response, ContractError> {
    let mut config = TOKEN_INFO
        .may_load(deps.storage)?
        .ok_or(ContractError::Unauthorized {})?;

    let mint = config.mint.as_ref().ok_or(ContractError::Unauthorized {})?;
    if mint.minter != info.sender {
        return Err(ContractError::Unauthorized {});
    }

    let minter_data = new_minter
        .map(|new_minter| deps.api.addr_validate(&new_minter))
        .transpose()?
        .map(|minter| MinterData {
            minter,
            cap: mint.cap,
        });

    config.mint = minter_data;

    TOKEN_INFO.save(deps.storage, &config)?;

    Ok(Response::default()
        .add_attribute("action", "update_minter")
        .add_attribute(
            "new_minter",
            config
                .mint
                .map(|m| m.minter.into_string())
                .unwrap_or_else(|| "None".to_string()),
        ))
}

pub fn execute_update_marketing(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    project: Option<String>,
    description: Option<String>,
    marketing: Option<String>,
) -> Result<Response, ContractError> {
    let mut marketing_info = MARKETING_INFO
        .may_load(deps.storage)?
        .ok_or(ContractError::Unauthorized {})?;

    if marketing_info
        .marketing
        .as_ref()
        .ok_or(ContractError::Unauthorized {})?
        != info.sender
    {
        return Err(ContractError::Unauthorized {});
    }

    match project {
        Some(empty) if empty.trim().is_empty() => marketing_info.project = None,
        Some(project) => marketing_info.project = Some(project),
        None => (),
    }

    match description {
        Some(empty) if empty.trim().is_empty() => marketing_info.description = None,
        Some(description) => marketing_info.description = Some(description),
        None => (),
    }

    match marketing {
        Some(empty) if empty.trim().is_empty() => marketing_info.marketing = None,
        Some(marketing) => marketing_info.marketing = Some(deps.api.addr_validate(&marketing)?),
        None => (),
    }

    if marketing_info.project.is_none()
        && marketing_info.description.is_none()
        && marketing_info.marketing.is_none()
        && marketing_info.logo.is_none()
    {
        MARKETING_INFO.remove(deps.storage);
    } else {
        MARKETING_INFO.save(deps.storage, &marketing_info)?;
    }

    let res = Response::new().add_attribute("action", "update_marketing");
    Ok(res)
}

pub fn execute_upload_logo(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    logo: Logo,
) -> Result<Response, ContractError> {
    let mut marketing_info = MARKETING_INFO
        .may_load(deps.storage)?
        .ok_or(ContractError::Unauthorized {})?;

    verify_logo(&logo)?;

    if marketing_info
        .marketing
        .as_ref()
        .ok_or(ContractError::Unauthorized {})?
        != info.sender
    {
        return Err(ContractError::Unauthorized {});
    }

    LOGO.save(deps.storage, &logo)?;

    let logo_info = match logo {
        Logo::Url(url) => LogoInfo::Url(url),
        Logo::Embedded(_) => LogoInfo::Embedded,
    };

    marketing_info.logo = Some(logo_info);
    MARKETING_INFO.save(deps.storage, &marketing_info)?;

    let res = Response::new().add_attribute("action", "upload_logo");
    Ok(res)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Balance { address } => to_json_binary(&query_balance(deps, address)?),
        QueryMsg::TokenInfo {} => to_json_binary(&query_token_info(deps)?),
        QueryMsg::Minter {} => to_json_binary(&query_minter(deps)?),
        QueryMsg::Allowance { owner, spender } => {
            to_json_binary(&query_allowance(deps, owner, spender)?)
        }
        QueryMsg::AllAllowances {
            owner,
            start_after,
            limit,
        } => to_json_binary(&query_owner_allowances(deps, owner, start_after, limit)?),
        QueryMsg::AllSpenderAllowances {
            spender,
            start_after,
            limit,
        } => to_json_binary(&query_spender_allowances(
            deps,
            spender,
            start_after,
            limit,
        )?),
        QueryMsg::AllAccounts { start_after, limit } => {
            to_json_binary(&query_all_accounts(deps, start_after, limit)?)
        }
        QueryMsg::MarketingInfo {} => to_json_binary(&query_marketing_info(deps)?),
        QueryMsg::DownloadLogo {} => to_json_binary(&query_download_logo(deps)?),
        QueryMsg::TaxMap {} => to_json_binary(&TAX_INFO.load(deps.storage)?),
    }
}

pub fn query_balance(deps: Deps, address: String) -> StdResult<BalanceResponse> {
    let address = deps.api.addr_validate(&address)?;
    let balance = BALANCES
        .may_load(deps.storage, &address)?
        .unwrap_or_default();
    Ok(BalanceResponse { balance })
}

pub fn query_token_info(deps: Deps) -> StdResult<TokenInfoResponse> {
    let info = TOKEN_INFO.load(deps.storage)?;
    let res = TokenInfoResponse {
        name: info.name,
        symbol: info.symbol,
        decimals: info.decimals,
        total_supply: info.total_supply,
    };
    Ok(res)
}

pub fn query_minter(deps: Deps) -> StdResult<Option<MinterResponse>> {
    let meta = TOKEN_INFO.load(deps.storage)?;
    let minter = match meta.mint {
        Some(m) => Some(MinterResponse {
            minter: m.minter.into(),
            cap: m.cap,
        }),
        None => None,
    };
    Ok(minter)
}

pub fn query_marketing_info(deps: Deps) -> StdResult<MarketingInfoResponse> {
    Ok(MARKETING_INFO.may_load(deps.storage)?.unwrap_or_default())
}

pub fn query_download_logo(deps: Deps) -> StdResult<DownloadLogoResponse> {
    let logo = LOGO.load(deps.storage)?;
    match logo {
        Logo::Embedded(EmbeddedLogo::Svg(logo)) => Ok(DownloadLogoResponse {
            mime_type: "image/svg+xml".to_owned(),
            data: logo,
        }),
        Logo::Embedded(EmbeddedLogo::Png(logo)) => Ok(DownloadLogoResponse {
            mime_type: "image/png".to_owned(),
            data: logo,
        }),
        Logo::Url(_) => Err(StdError::not_found("logo")),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, msg: MigrateMsg) -> Result<Response, ContractError> {

    // merge upgrade paths
    // crates.io:terraport-token 0.0.0 -> crates.io:cw20-base 1.1.0
    // crates.io:cw20-base 1.1.0 -> crates.io:cw20-base 1.1.0
    state::migrate_v1::ensure_known_upgrade_path(deps.storage)?;

    let original_version =
        ensure_from_older_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    if original_version < "0.14.0".parse::<semver::Version>().unwrap() {
        // Build reverse map of allowances per spender
        let data = ALLOWANCES
            .range(deps.storage, None, None, Ascending)
            .collect::<StdResult<Vec<_>>>()?;
        for ((owner, spender), allowance) in data {
            ALLOWANCES_SPENDER.save(deps.storage, (&spender, &owner), &allowance)?;
        }
    }

    if original_version < "1.1.0+taxed001".parse::<semver::Version>().unwrap() {
        match TAX_INFO.load(deps.storage) {
            // there seems to be an existing tax map, so we don't need to do anything
            Ok(_) => {},

            // no tax map, so we need to add one
            Err(_) => {
                // Add tax map
                let tax_map = match msg.tax_map {
                    Some(x) => x,
                    None => TaxMap::default(),
                };
                TAX_INFO.save(deps.storage, &tax_map)?;
            }
        }
    }
    Ok(Response::default())
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::testing::{
        mock_dependencies, mock_dependencies_with_balance, mock_env, mock_info,
    };
    use cosmwasm_std::{coins, from_json, Addr, CosmosMsg, Never, StdError, SubMsg, WasmMsg};

    use super::*;
    use crate::msg::InstantiateMarketingInfo;
    use crate::tax::{TaxAlwaysCondition, TaxCondition, TaxInfo, TaxNeverCondition};

    fn get_balance<T: Into<String>>(deps: Deps, address: T) -> Uint128 {
        query_balance(deps, address.into()).unwrap().balance
    }

    // this will set up the instantiation for other tests
    fn do_instantiate_with_minter(
        deps: DepsMut,
        addr: &str,
        amount: Uint128,
        minter: &str,
        cap: Option<Uint128>,
    ) -> TokenInfoResponse {
        _do_instantiate(
            deps,
            addr,
            amount,
            Some(MinterResponse {
                minter: minter.to_string(),
                cap,
            }),
        )
    }

    // this will set up the instantiation for other tests
    fn do_instantiate(deps: DepsMut, addr: &str, amount: Uint128) -> TokenInfoResponse {
        _do_instantiate(deps, addr, amount, None)
    }

    fn do_instantiate_with_tax_on_transfer(deps: DepsMut, addr: &str, amount: Uint128) -> TokenInfoResponse {
        _do_instantiate_with_tax_on_transfer(deps, addr, amount)
    }

    fn do_instantiate_with_tax_on_send(deps: DepsMut, addr: &str, amount: Uint128) -> TokenInfoResponse {
        _do_instantiate_with_tax_on_send(deps, addr, amount)
    }

    // this will set up the instantiation for other tests
    fn _do_instantiate(
        mut deps: DepsMut,
        addr: &str,
        amount: Uint128,
        mint: Option<MinterResponse>,
    ) -> TokenInfoResponse {
        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr.to_string(),
                amount,
            }],
            mint: mint.clone(),
            marketing: None,
            tax_map: None,
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
        assert_eq!(query_minter(deps.as_ref()).unwrap(), mint,);
        meta
    }

    // this will set up the instantiation for other tests that
    // involve taxation
    fn _do_instantiate_with_tax_on_transfer(
        mut deps: DepsMut,
        addr: &str,
        amount: Uint128,
    ) -> TokenInfoResponse {

        // simple flat p2p tax
        let tax_map_in = Some(TaxMap{
            on_transfer: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
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

    fn _do_instantiate_with_tax_on_send(
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
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            on_send_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
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

    const PNG_HEADER: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    mod instantiate {
        use super::*;

        #[test]
        fn basic() {
            let mut deps = mock_dependencies();
            let amount = Uint128::from(11223344u128);
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![Cw20Coin {
                    address: String::from("addr0000"),
                    amount,
                }],
                mint: None,
                marketing: None,
                tax_map: None,
            };
            let info = mock_info("creator", &[]);
            let env = mock_env();
            let res = instantiate(deps.as_mut(), env, info, instantiate_msg).unwrap();
            assert_eq!(0, res.messages.len());

            assert_eq!(
                query_token_info(deps.as_ref()).unwrap(),
                TokenInfoResponse {
                    name: "Cash Token".to_string(),
                    symbol: "CASH".to_string(),
                    decimals: 9,
                    total_supply: amount,
                }
            );
            assert_eq!(
                get_balance(deps.as_ref(), "addr0000"),
                Uint128::new(11223344)
            );
        }

        #[test]
        fn mintable() {
            let mut deps = mock_dependencies();
            let amount = Uint128::new(11223344);
            let minter = String::from("asmodat");
            let limit = Uint128::new(511223344);
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![Cw20Coin {
                    address: "addr0000".into(),
                    amount,
                }],
                mint: Some(MinterResponse {
                    minter: minter.clone(),
                    cap: Some(limit),
                }),
                marketing: None,
                tax_map: None,
            };
            let info = mock_info("creator", &[]);
            let env = mock_env();
            let res = instantiate(deps.as_mut(), env, info, instantiate_msg).unwrap();
            assert_eq!(0, res.messages.len());

            assert_eq!(
                query_token_info(deps.as_ref()).unwrap(),
                TokenInfoResponse {
                    name: "Cash Token".to_string(),
                    symbol: "CASH".to_string(),
                    decimals: 9,
                    total_supply: amount,
                }
            );
            assert_eq!(
                get_balance(deps.as_ref(), "addr0000"),
                Uint128::new(11223344)
            );
            assert_eq!(
                query_minter(deps.as_ref()).unwrap(),
                Some(MinterResponse {
                    minter,
                    cap: Some(limit),
                }),
            );
        }

        #[test]
        fn mintable_over_cap() {
            let mut deps = mock_dependencies();
            let amount = Uint128::new(11223344);
            let minter = String::from("asmodat");
            let limit = Uint128::new(11223300);
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![Cw20Coin {
                    address: String::from("addr0000"),
                    amount,
                }],
                mint: Some(MinterResponse {
                    minter,
                    cap: Some(limit),
                }),
                marketing: None,
                tax_map: None,
            };
            let info = mock_info("creator", &[]);
            let env = mock_env();
            let err = instantiate(deps.as_mut(), env, info, instantiate_msg).unwrap_err();
            assert_eq!(
                err,
                StdError::generic_err("Initial supply greater than cap").into()
            );
        }

        mod marketing {
            use super::*;

            #[test]
            fn basic() {
                let mut deps = mock_dependencies();
                let instantiate_msg = InstantiateMsg {
                    name: "Cash Token".to_string(),
                    symbol: "CASH".to_string(),
                    decimals: 9,
                    initial_balances: vec![],
                    mint: None,
                    marketing: Some(InstantiateMarketingInfo {
                        project: Some("Project".to_owned()),
                        description: Some("Description".to_owned()),
                        marketing: Some("marketing".to_owned()),
                        logo: Some(Logo::Url("url".to_owned())),
                    }),
                    tax_map: None,
                };

                let info = mock_info("creator", &[]);
                let env = mock_env();
                let res = instantiate(deps.as_mut(), env, info, instantiate_msg).unwrap();
                assert_eq!(0, res.messages.len());

                assert_eq!(
                    query_marketing_info(deps.as_ref()).unwrap(),
                    MarketingInfoResponse {
                        project: Some("Project".to_owned()),
                        description: Some("Description".to_owned()),
                        marketing: Some(Addr::unchecked("marketing")),
                        logo: Some(LogoInfo::Url("url".to_owned())),
                    }
                );

                let err = query_download_logo(deps.as_ref()).unwrap_err();
                assert!(
                    matches!(err, StdError::NotFound { .. }),
                    "Expected StdError::NotFound, received {err}",
                );
            }

            #[test]
            fn invalid_marketing() {
                let mut deps = mock_dependencies();
                let instantiate_msg = InstantiateMsg {
                    name: "Cash Token".to_string(),
                    symbol: "CASH".to_string(),
                    decimals: 9,
                    initial_balances: vec![],
                    mint: None,
                    marketing: Some(InstantiateMarketingInfo {
                        project: Some("Project".to_owned()),
                        description: Some("Description".to_owned()),
                        marketing: Some("m".to_owned()),
                        logo: Some(Logo::Url("url".to_owned())),
                    }),
                    tax_map: None,
                };

                let info = mock_info("creator", &[]);
                let env = mock_env();
                instantiate(deps.as_mut(), env, info, instantiate_msg).unwrap_err();

                let err = query_download_logo(deps.as_ref()).unwrap_err();
                assert!(
                    matches!(err, StdError::NotFound { .. }),
                    "Expected StdError::NotFound, received {err}",
                );
            }
        }
    }

    #[test]
    fn can_mint_by_minter() {
        let mut deps = mock_dependencies();

        let genesis = String::from("genesis");
        let amount = Uint128::new(11223344);
        let minter = String::from("asmodat");
        let limit = Uint128::new(511223344);
        do_instantiate_with_minter(deps.as_mut(), &genesis, amount, &minter, Some(limit));

        // minter can mint coins to some winner
        let winner = String::from("lucky");
        let prize = Uint128::new(222_222_222);
        let msg = ExecuteMsg::Mint {
            recipient: winner.clone(),
            amount: prize,
        };

        let info = mock_info(minter.as_ref(), &[]);
        let env = mock_env();
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(0, res.messages.len());
        assert_eq!(get_balance(deps.as_ref(), genesis), amount);
        assert_eq!(get_balance(deps.as_ref(), winner.clone()), prize);

        // Allows minting 0
        let msg = ExecuteMsg::Mint {
            recipient: winner.clone(),
            amount: Uint128::zero(),
        };
        let info = mock_info(minter.as_ref(), &[]);
        let env = mock_env();
        execute(deps.as_mut(), env, info, msg).unwrap();

        // but if it exceeds cap (even over multiple rounds), it fails
        // cap is enforced
        let msg = ExecuteMsg::Mint {
            recipient: winner,
            amount: Uint128::new(333_222_222),
        };
        let info = mock_info(minter.as_ref(), &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::CannotExceedCap {});
    }

    #[test]
    fn others_cannot_mint() {
        let mut deps = mock_dependencies();
        do_instantiate_with_minter(
            deps.as_mut(),
            &String::from("genesis"),
            Uint128::new(1234),
            &String::from("minter"),
            None,
        );

        let msg = ExecuteMsg::Mint {
            recipient: String::from("lucky"),
            amount: Uint128::new(222),
        };
        let info = mock_info("anyone else", &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Unauthorized {});
    }

    #[test]
    fn minter_can_update_minter_but_not_cap() {
        let mut deps = mock_dependencies();
        let minter = String::from("minter");
        let cap = Some(Uint128::from(3000000u128));
        do_instantiate_with_minter(
            deps.as_mut(),
            &String::from("genesis"),
            Uint128::new(1234),
            &minter,
            cap,
        );

        let new_minter = "new_minter";
        let msg = ExecuteMsg::UpdateMinter {
            new_minter: Some(new_minter.to_string()),
        };

        let info = mock_info(&minter, &[]);
        let env = mock_env();
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert!(res.is_ok());
        let query_minter_msg = QueryMsg::Minter {};
        let res = query(deps.as_ref(), env, query_minter_msg);
        let mint: MinterResponse = from_json(&res.unwrap()).unwrap();

        // Minter cannot update cap.
        assert!(mint.cap == cap);
        assert!(mint.minter == new_minter)
    }

    #[test]
    fn others_cannot_update_minter() {
        let mut deps = mock_dependencies();
        let minter = String::from("minter");
        do_instantiate_with_minter(
            deps.as_mut(),
            &String::from("genesis"),
            Uint128::new(1234),
            &minter,
            None,
        );

        let msg = ExecuteMsg::UpdateMinter {
            new_minter: Some("new_minter".to_string()),
        };

        let info = mock_info("not the minter", &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Unauthorized {});
    }

    #[test]
    fn unset_minter() {
        let mut deps = mock_dependencies();
        let minter = String::from("minter");
        let cap = None;
        do_instantiate_with_minter(
            deps.as_mut(),
            &String::from("genesis"),
            Uint128::new(1234),
            &minter,
            cap,
        );

        let msg = ExecuteMsg::UpdateMinter { new_minter: None };

        let info = mock_info(&minter, &[]);
        let env = mock_env();
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert!(res.is_ok());
        let query_minter_msg = QueryMsg::Minter {};
        let res = query(deps.as_ref(), env, query_minter_msg);
        let mint: Option<MinterResponse> = from_json(&res.unwrap()).unwrap();

        // Check that mint information was removed.
        assert_eq!(mint, None);

        // Check that old minter can no longer mint.
        let msg = ExecuteMsg::Mint {
            recipient: String::from("lucky"),
            amount: Uint128::new(222),
        };
        let info = mock_info("minter", &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Unauthorized {});
    }

    #[test]
    fn no_one_mints_if_minter_unset() {
        let mut deps = mock_dependencies();
        do_instantiate(deps.as_mut(), &String::from("genesis"), Uint128::new(1234));

        let msg = ExecuteMsg::Mint {
            recipient: String::from("lucky"),
            amount: Uint128::new(222),
        };
        let info = mock_info("genesis", &[]);
        let env = mock_env();
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert_eq!(err, ContractError::Unauthorized {});
    }

    #[test]
    fn instantiate_multiple_accounts() {
        let mut deps = mock_dependencies();
        let amount1 = Uint128::from(11223344u128);
        let addr1 = String::from("addr0001");
        let amount2 = Uint128::from(7890987u128);
        let addr2 = String::from("addr0002");
        let info = mock_info("creator", &[]);
        let env = mock_env();

        // Fails with duplicate addresses
        let instantiate_msg = InstantiateMsg {
            name: "Bash Shell".to_string(),
            symbol: "BASH".to_string(),
            decimals: 6,
            initial_balances: vec![
                Cw20Coin {
                    address: addr1.clone(),
                    amount: amount1,
                },
                Cw20Coin {
                    address: addr1.clone(),
                    amount: amount2,
                },
            ],
            mint: None,
            marketing: None,
            tax_map: None, 
        };
        let err =
            instantiate(deps.as_mut(), env.clone(), info.clone(), instantiate_msg).unwrap_err();
        assert_eq!(err, ContractError::DuplicateInitialBalanceAddresses {});

        // Works with unique addresses
        let instantiate_msg = InstantiateMsg {
            name: "Bash Shell".to_string(),
            symbol: "BASH".to_string(),
            decimals: 6,
            initial_balances: vec![
                Cw20Coin {
                    address: addr1.clone(),
                    amount: amount1,
                },
                Cw20Coin {
                    address: addr2.clone(),
                    amount: amount2,
                },
            ],
            mint: None,
            marketing: None,
            tax_map: None,
        };
        let res = instantiate(deps.as_mut(), env, info, instantiate_msg).unwrap();
        assert_eq!(0, res.messages.len());
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap(),
            TokenInfoResponse {
                name: "Bash Shell".to_string(),
                symbol: "BASH".to_string(),
                decimals: 6,
                total_supply: amount1 + amount2,
            }
        );
        assert_eq!(get_balance(deps.as_ref(), addr1), amount1);
        assert_eq!(get_balance(deps.as_ref(), addr2), amount2);
    }

    #[test]
    fn queries_work() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);

        let expected = do_instantiate(deps.as_mut(), &addr1, amount1);

        // check meta query
        let loaded = query_token_info(deps.as_ref()).unwrap();
        assert_eq!(expected, loaded);

        let _info = mock_info("test", &[]);
        let env = mock_env();
        // check balance query (full)
        let data = query(
            deps.as_ref(),
            env.clone(),
            QueryMsg::Balance { address: addr1 },
        )
        .unwrap();
        let loaded: BalanceResponse = from_json(&data).unwrap();
        assert_eq!(loaded.balance, amount1);

        // check balance query (empty)
        let data = query(
            deps.as_ref(),
            env,
            QueryMsg::Balance {
                address: String::from("addr0002"),
            },
        )
        .unwrap();
        let loaded: BalanceResponse = from_json(&data).unwrap();
        assert_eq!(loaded.balance, Uint128::zero());
    }

    fn mock_valid_tax_map(admin: String) -> TaxMap {
        TaxMap{
            on_transfer: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            on_send: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            on_send_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_transfer_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            admin: Addr::unchecked(admin),
        }
    }

    fn mock_invalid_tax_map(admin: String) -> TaxMap {
        TaxMap{
            on_transfer: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(110)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            on_send: TaxInfo {
                src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                dst_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(10)}),
                proceeds: Addr::unchecked(String::from("proceeds")),
            },
            on_send_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            on_transfer_from: TaxInfo {
                src_cond: TaxCondition::Never(TaxNeverCondition{}),
                dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                proceeds: Addr::unchecked(""),
            },
            admin: Addr::unchecked(admin),
        }
    }

    #[test]
    fn can_set_valid_tax_map() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env, info, instantiate_msg);
        assert_eq!(res.is_ok(), true);
    }

    #[test]
    fn cannot_set_tax_map_if_not_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env, info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let tax_map_in = mock_valid_tax_map("admin".to_string());
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let msg = ExecuteMsg::SetTaxMap {
            tax_map: Some(tax_map_in),
        };
        let res = execute(deps.as_mut(), env, info, msg);
        assert_eq!(res.is_err(), true);
    }

    #[test]
    fn can_set_valid_tax_map_if_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env, info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let tax_map_in = mock_valid_tax_map("admin".to_string());
        let info = mock_info("admin", &[]);
        let env = mock_env();
        let msg = ExecuteMsg::SetTaxMap {
            tax_map: Some(tax_map_in),
        };
        let res = execute(deps.as_mut(), env, info, msg);
        assert_eq!(res.is_ok(), true);
    }

    #[test]
    fn cannot_set_invalid_tax_map_if_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_valid = mock_valid_tax_map("admin".to_string());

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_valid),
        };
        let env = mock_env();
        let info = mock_info("creator", &[]);
        let res = instantiate(deps.as_mut(), env.clone(), info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let tax_map_invalid = mock_invalid_tax_map("admin".to_string());
        let info = mock_info("admin", &[]);
        let msg = ExecuteMsg::SetTaxMap {
            tax_map: Some(tax_map_invalid),
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert_eq!(res.is_err(), true);
    }

    #[test]
    fn cannot_instantiate_with_invalid_tax_map() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_invalid_tax_map("admin".to_string());

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env, info, instantiate_msg);
        assert_eq!(res.is_err(), true);
    }

    #[test]
    fn ensure_setting_empty_tax_map_preserves_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());
        let mut expected_tax_map = TaxMap::default();
        expected_tax_map.admin = Addr::unchecked("admin");

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env.clone(), info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let info = mock_info("admin", &[]);
        let msg = ExecuteMsg::SetTaxMap {
            tax_map: None,
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert_eq!(res.is_ok(), true);
        assert_eq!(TAX_INFO.load(&deps.storage).unwrap(), expected_tax_map);

    }

    #[test]
    fn tax_admin_can_update_tax_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());
        let mut expected_tax_map = tax_map_in.clone();
        expected_tax_map.admin = Addr::unchecked("new_admin");

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env.clone(), info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let info = mock_info("admin", &[]);
        let msg = ExecuteMsg::SetTaxAdmin {
            tax_admin: Some(String::from("new_admin")),
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert_eq!(res.is_ok(), true);
        assert_eq!(TAX_INFO.load(&deps.storage).unwrap(), expected_tax_map);
    }

    #[test]
    fn others_cannot_update_tax_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in.clone()),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env.clone(), info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let info = mock_info("not_admin", &[]);
        let msg = ExecuteMsg::SetTaxAdmin {
            tax_admin: Some(String::from("new_admin")),
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert_eq!(res.is_err(), true);
        assert_eq!(TAX_INFO.load(&deps.storage).unwrap(), tax_map_in);
    }

    #[test]
    fn ensure_sound_setting_of_empty_tax_admin() {
        let mut deps = mock_dependencies();
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let tax_map_in = mock_valid_tax_map("admin".to_string());
        let mut expected_tax_map = tax_map_in.clone();
        expected_tax_map.admin = Addr::unchecked("");

        let instantiate_msg = InstantiateMsg {
            name: "Auto Gen".to_string(),
            symbol: "AUTO".to_string(),
            decimals: 3,
            initial_balances: vec![Cw20Coin {
                address: addr1.to_string(),
                amount: amount1,
            }],
            mint: None,
            marketing: None,
            tax_map: Some(tax_map_in),
        };
        let info = mock_info("creator", &[]);
        let env = mock_env();
        let res = instantiate(deps.as_mut(), env.clone(), info, instantiate_msg);
        assert_eq!(res.is_ok(), true);

        let info = mock_info("admin", &[]);
        let msg = ExecuteMsg::SetTaxAdmin {
            tax_admin: None,
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg);
        assert_eq!(res.is_ok(), true);
        assert_eq!(TAX_INFO.load(&deps.storage).unwrap(), expected_tax_map);
    }

    #[test]
    fn transfer() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
        let addr1 = String::from("addr0001");
        let addr2 = String::from("addr0002");
        let amount1 = Uint128::from(12340000u128);
        let transfer = Uint128::from(76543u128);
        let too_much = Uint128::from(12340321u128);

        do_instantiate(deps.as_mut(), &addr1, amount1);

        // Allows transferring 0
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Transfer {
            recipient: addr2.clone(),
            amount: Uint128::zero(),
        };
        execute(deps.as_mut(), env, info, msg).unwrap();

        // cannot send more than we have
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Transfer {
            recipient: addr2.clone(),
            amount: too_much,
        };
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));

        // cannot send from empty account
        let info = mock_info(addr2.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Transfer {
            recipient: addr1.clone(),
            amount: transfer,
        };
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));

        // valid transfer
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Transfer {
            recipient: addr2.clone(),
            amount: transfer,
        };
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(res.messages.len(), 0);

        let remainder = amount1.checked_sub(transfer).unwrap();
        assert_eq!(get_balance(deps.as_ref(), addr1), remainder);
        assert_eq!(get_balance(deps.as_ref(), addr2), transfer);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );
    }

    #[test]
    fn transfer_with_tax() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
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

        do_instantiate_with_tax_on_transfer(deps.as_mut(), &addr1, amount1);

        // test valid transfer
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Transfer {
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
    fn burn() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
        let addr1 = String::from("addr0001");
        let amount1 = Uint128::from(12340000u128);
        let burn = Uint128::from(76543u128);
        let too_much = Uint128::from(12340321u128);

        do_instantiate(deps.as_mut(), &addr1, amount1);

        // Allows burning 0
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Burn {
            amount: Uint128::zero(),
        };
        execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );

        // cannot burn more than we have
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Burn { amount: too_much };
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );

        // valid burn reduces total supply
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Burn { amount: burn };
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(res.messages.len(), 0);

        let remainder = amount1.checked_sub(burn).unwrap();
        assert_eq!(get_balance(deps.as_ref(), addr1), remainder);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            remainder
        );
    }

    #[test]
    fn send() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
        let addr1 = String::from("addr0001");
        let contract = String::from("addr0002");
        let amount1 = Uint128::from(12340000u128);
        let transfer = Uint128::from(76543u128);
        let too_much = Uint128::from(12340321u128);
        let send_msg = Binary::from(r#"{"some":123}"#.as_bytes());

        do_instantiate(deps.as_mut(), &addr1, amount1);

        // Allows sending 0
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Send {
            contract: contract.clone(),
            amount: Uint128::zero(),
            msg: send_msg.clone(),
        };
        execute(deps.as_mut(), env, info, msg).unwrap();

        // cannot send more than we have
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Send {
            contract: contract.clone(),
            amount: too_much,
            msg: send_msg.clone(),
        };
        let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
        assert!(matches!(err, ContractError::Std(StdError::Overflow { .. })));

        // valid transfer
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Send {
            contract: contract.clone(),
            amount: transfer,
            msg: send_msg.clone(),
        };
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(res.messages.len(), 1);

        // ensure proper send message sent
        // this is the message we want delivered to the other side
        let binary_msg = Cw20ReceiveMsg {
            sender: addr1.clone(),
            amount: transfer,
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

        // ensure balance is properly transferred
        let remainder = amount1.checked_sub(transfer).unwrap();
        assert_eq!(get_balance(deps.as_ref(), addr1), remainder);
        assert_eq!(get_balance(deps.as_ref(), contract), transfer);
        assert_eq!(
            query_token_info(deps.as_ref()).unwrap().total_supply,
            amount1
        );
    }

    #[test]
    fn send_with_tax() {
        let mut deps = mock_dependencies_with_balance(&coins(2, "token"));
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

        do_instantiate_with_tax_on_send(deps.as_mut(), &addr1, amount1);

        // valid transfer
        let info = mock_info(addr1.as_ref(), &[]);
        let env = mock_env();
        let msg = ExecuteMsg::Send {
            contract: contract.clone(),
            amount: transfer,
            msg: send_msg.clone(),
        };
        let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
        assert_eq!(res.messages.len(), 2);

        // ensure proper send message sent
        // this is the message we want delivered to the other side
        let binary_msg = Cw20ReceiveMsg {
            sender: addr1.clone(),
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

    mod migration {
        use std::{borrow::{Borrow, BorrowMut}, mem};

        use self::state::migrate_v1::tests::mock_dependencies_with_terraport_balances;

        use super::*;

        use cosmwasm_std::Empty;
        use cw20::{AllAllowancesResponse, AllSpenderAllowancesResponse, SpenderAllowanceInfo};
        use cw_multi_test::{App, Contract, ContractWrapper, Executor};

        fn cw20_contract() -> Box<dyn Contract<Empty>> {
            let contract = ContractWrapper::new(
                crate::contract::execute,
                crate::contract::instantiate,
                crate::contract::query,
            )
            .with_migrate(crate::contract::migrate);
            Box::new(contract)
        }

        #[test]
        fn test_migrate() {
            let mut app = App::default();

            let cw20_id = app.store_code(cw20_contract());
            let cw20_addr = app
                .instantiate_contract(
                    cw20_id,
                    Addr::unchecked("sender"),
                    &InstantiateMsg {
                        name: "Token".to_string(),
                        symbol: "TOKEN".to_string(),
                        decimals: 6,
                        initial_balances: vec![Cw20Coin {
                            address: "sender".to_string(),
                            amount: Uint128::new(100),
                        }],
                        mint: None,
                        marketing: None,
                        tax_map: None,
                    },
                    &[],
                    "TOKEN",
                    Some("sender".to_string()),
                )
                .unwrap();

            // no allowance to start
            let allowance: AllAllowancesResponse = app
                .wrap()
                .query_wasm_smart(
                    cw20_addr.to_string(),
                    &QueryMsg::AllAllowances {
                        owner: "sender".to_string(),
                        start_after: None,
                        limit: None,
                    },
                )
                .unwrap();
            assert_eq!(allowance, AllAllowancesResponse::default());

            // Set allowance
            let allow1 = Uint128::new(7777);
            let expires = cw20::Expiration::AtHeight(123_456);
            let msg = CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: cw20_addr.to_string(),
                msg: to_json_binary(&ExecuteMsg::IncreaseAllowance {
                    spender: "spender".into(),
                    amount: allow1,
                    expires: Some(expires),
                })
                .unwrap(),
                funds: vec![],
            });
            app.execute(Addr::unchecked("sender"), msg).unwrap();

            // Now migrate
            app.execute(
                Addr::unchecked("sender"),
                CosmosMsg::Wasm(WasmMsg::Migrate {
                    contract_addr: cw20_addr.to_string(),
                    new_code_id: cw20_id,
                    msg: to_json_binary(&MigrateMsg { tax_map: None }).unwrap(),
                }),
            )
            .unwrap();

            // Smoke check that the contract still works.
            let balance: cw20::BalanceResponse = app
                .wrap()
                .query_wasm_smart(
                    cw20_addr.clone(),
                    &QueryMsg::Balance {
                        address: "sender".to_string(),
                    },
                )
                .unwrap();

            assert_eq!(balance.balance, Uint128::new(100));

            // Confirm that the allowance per spender is there
            let allowance: AllSpenderAllowancesResponse = app
                .wrap()
                .query_wasm_smart(
                    cw20_addr,
                    &QueryMsg::AllSpenderAllowances {
                        spender: "spender".to_string(),
                        start_after: None,
                        limit: None,
                    },
                )
                .unwrap();
            assert_eq!(
                allowance.allowances,
                &[SpenderAllowanceInfo {
                    owner: "sender".to_string(),
                    allowance: allow1,
                    expires
                }]
            );
        }

        #[test]
        fn test_migrate_from_terraport() {
            let mut deps = mock_dependencies_with_terraport_balances(vec![
                // initial balances
                (Addr::unchecked("addr1"), Uint128::new(1234), 123),
                (Addr::unchecked("addr2"), Uint128::new(1234), 123),
                (Addr::unchecked("addr3"), Uint128::new(4455), 123),

                // mock a transfer at later height
                (Addr::unchecked("addr1"), Uint128::new(1233), 456),
                (Addr::unchecked("addr2"), Uint128::new(1235), 456),
            ]);
            
            let env = mock_env();
            crate::contract::migrate(deps.as_mut(), env, MigrateMsg { tax_map: None }).unwrap();

            // balances are ok
            match query_balance(deps.as_ref(), "addr1".to_string()){
                Ok(balance) => {
                    assert_eq!(
                        balance,
                        BalanceResponse{
                            balance: Uint128::new(1233),
                        }
                    )
                },
                Err(e) => panic!("Error querying balance: {:?}", e),
            }
            match query_balance(deps.as_ref(), "addr2".to_string()){
                Ok(balance) => {
                    assert_eq!(
                        balance,
                        BalanceResponse{
                            balance: Uint128::new(1235),
                        }
                    )
                },
                Err(e) => panic!("Error querying balance: {:?}", e),
            }

            // tax map is set
            match TAX_INFO.load(deps.as_ref().storage) {
                Ok(tax_info) => {
                    assert_eq!(tax_info, TaxMap::default() )
                },
                Err(_) => panic!("Expected Tax map to be available!"),
            }
            
        }

        #[test]
        fn test_migrate_from_terraport_with_explicit_tax_map() {
            let mut deps = mock_dependencies_with_terraport_balances(vec![
                // initial balances
                (Addr::unchecked("addr1"), Uint128::new(1234), 123),
                (Addr::unchecked("addr2"), Uint128::new(1234), 123),
                (Addr::unchecked("addr3"), Uint128::new(4455), 123),

                // mock a transfer at later height
                (Addr::unchecked("addr1"), Uint128::new(1233), 456),
                (Addr::unchecked("addr2"), Uint128::new(1235), 456),
            ]);
            let tax = TaxMap {
                admin: Addr::unchecked("admin"),
                on_transfer: TaxInfo{
                    src_cond: TaxCondition::Never(TaxNeverCondition{}),
                    dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                    proceeds: Addr::unchecked("proceeds1"),
                },
                on_transfer_from: TaxInfo {
                    src_cond: TaxCondition::Never(TaxNeverCondition{}),
                    dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                    proceeds: Addr::unchecked("proceeds2"),
                },
                on_send: TaxInfo {
                    src_cond: TaxCondition::Never(TaxNeverCondition{}),
                    dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                    proceeds: Addr::unchecked("proceeds3"),
                
                },
                on_send_from: TaxInfo{
                    src_cond: TaxCondition::Always(TaxAlwaysCondition{tax_rate: Decimal::percent(1)}),
                    dst_cond: TaxCondition::Never(TaxNeverCondition{}),
                    proceeds: Addr::unchecked("proceeds4"),
                }
            };
            
            let env = mock_env();
            crate::contract::migrate(deps.as_mut(), env, MigrateMsg { tax_map: Some(tax.clone()) }).unwrap();

            // balances are ok
            match query_balance(deps.as_ref(), "addr1".to_string()){
                Ok(balance) => {
                    assert_eq!(
                        balance,
                        BalanceResponse{
                            balance: Uint128::new(1233),
                        }
                    )
                },
                Err(e) => panic!("Error querying balance: {:?}", e),
            }
            match query_balance(deps.as_ref(), "addr2".to_string()){
                Ok(balance) => {
                    assert_eq!(
                        balance,
                        BalanceResponse{
                            balance: Uint128::new(1235),
                        }
                    )
                },
                Err(e) => panic!("Error querying balance: {:?}", e),
            }

            // tax map is set
            match TAX_INFO.load(deps.as_ref().storage) {
                Ok(tax_info) => {
                    assert_eq!(tax_info, tax )
                },
                Err(_) => panic!("Expected Tax map to be available!"),
            }
            
        }
    }

    mod marketing {
        use super::*;

        #[test]
        fn update_unauthorised() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("marketing".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let err = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: Some("New project".to_owned()),
                    description: Some("Better description".to_owned()),
                    marketing: Some("creator".to_owned()),
                },
            )
            .unwrap_err();

            assert_eq!(err, ContractError::Unauthorized {});

            // Ensure marketing didn't change
            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("marketing")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_project() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: Some("New project".to_owned()),
                    description: None,
                    marketing: None,
                },
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("New project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn clear_project() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: Some("".to_owned()),
                    description: None,
                    marketing: None,
                },
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: None,
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_description() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: None,
                    description: Some("Better description".to_owned()),
                    marketing: None,
                },
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Better description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn clear_description() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: None,
                    description: Some("".to_owned()),
                    marketing: None,
                },
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: None,
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_marketing() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: None,
                    description: None,
                    marketing: Some("marketing".to_owned()),
                },
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("marketing")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_marketing_invalid() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let err = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: None,
                    description: None,
                    marketing: Some("m".to_owned()),
                },
            )
            .unwrap_err();

            assert!(
                matches!(err, ContractError::Std(_)),
                "Expected Std error, received: {err}",
            );

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn clear_marketing() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UpdateMarketing {
                    project: None,
                    description: None,
                    marketing: Some("".to_owned()),
                },
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: None,
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_logo_url() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Url("new_url".to_owned())),
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("new_url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_logo_png() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Embedded(EmbeddedLogo::Png(PNG_HEADER.into()))),
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Embedded),
                }
            );

            assert_eq!(
                query_download_logo(deps.as_ref()).unwrap(),
                DownloadLogoResponse {
                    mime_type: "image/png".to_owned(),
                    data: PNG_HEADER.into(),
                }
            );
        }

        #[test]
        fn update_logo_svg() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let img = "<?xml version=\"1.0\"?><svg></svg>".as_bytes();
            let res = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Embedded(EmbeddedLogo::Svg(img.into()))),
            )
            .unwrap();

            assert_eq!(res.messages, vec![]);

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Embedded),
                }
            );

            assert_eq!(
                query_download_logo(deps.as_ref()).unwrap(),
                DownloadLogoResponse {
                    mime_type: "image/svg+xml".to_owned(),
                    data: img.into(),
                }
            );
        }

        #[test]
        fn update_logo_png_oversized() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let img = [&PNG_HEADER[..], &[1; 6000][..]].concat();
            let err = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Embedded(EmbeddedLogo::Png(img.into()))),
            )
            .unwrap_err();

            assert_eq!(err, ContractError::LogoTooBig {});

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_logo_svg_oversized() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let img = [
                "<?xml version=\"1.0\"?><svg>",
                std::str::from_utf8(&[b'x'; 6000]).unwrap(),
                "</svg>",
            ]
            .concat()
            .into_bytes();

            let err = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Embedded(EmbeddedLogo::Svg(img.into()))),
            )
            .unwrap_err();

            assert_eq!(err, ContractError::LogoTooBig {});

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_logo_png_invalid() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let img = &[1];
            let err = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Embedded(EmbeddedLogo::Png(img.into()))),
            )
            .unwrap_err();

            assert_eq!(err, ContractError::InvalidPngHeader {});

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }

        #[test]
        fn update_logo_svg_invalid() {
            let mut deps = mock_dependencies();
            let instantiate_msg = InstantiateMsg {
                name: "Cash Token".to_string(),
                symbol: "CASH".to_string(),
                decimals: 9,
                initial_balances: vec![],
                mint: None,
                marketing: Some(InstantiateMarketingInfo {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some("creator".to_owned()),
                    logo: Some(Logo::Url("url".to_owned())),
                }),
                tax_map: None,
            };

            let info = mock_info("creator", &[]);

            instantiate(deps.as_mut(), mock_env(), info.clone(), instantiate_msg).unwrap();

            let img = &[1];

            let err = execute(
                deps.as_mut(),
                mock_env(),
                info,
                ExecuteMsg::UploadLogo(Logo::Embedded(EmbeddedLogo::Svg(img.into()))),
            )
            .unwrap_err();

            assert_eq!(err, ContractError::InvalidXmlPreamble {});

            assert_eq!(
                query_marketing_info(deps.as_ref()).unwrap(),
                MarketingInfoResponse {
                    project: Some("Project".to_owned()),
                    description: Some("Description".to_owned()),
                    marketing: Some(Addr::unchecked("creator")),
                    logo: Some(LogoInfo::Url("url".to_owned())),
                }
            );

            let err = query_download_logo(deps.as_ref()).unwrap_err();
            assert!(
                matches!(err, StdError::NotFound { .. }),
                "Expected StdError::NotFound, received {err}",
            );
        }
    }
}
