use cosmwasm_schema::write_api;

use cw20_taxed::msg::{Cw20TaxedExecuteMsg as ExecuteMsg, InstantiateMsg, QueryMsg};

fn main() {
    write_api! {
        instantiate: InstantiateMsg,
        execute: ExecuteMsg,
        query: QueryMsg,
    }
}
