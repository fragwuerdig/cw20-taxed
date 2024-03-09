# CW20 with Source Tax Extension

This repository is a fork of the [cw-plus](https://github.com/cosmwasm/cw-plus) repository - stripping down the original original repository to the bare minimum and implementing a new type of CW20 token: `cw20-taxed`. This new kind of taxed CW20 token is a type of token contract, that has a configurable tax system in place. A tax deduction can be triggered depending on which endpoint was called (e.g. `MsgSend`, `MsgTransfer`, ...) and depending on various conditions that the source and recipient wallets may have to fulfill (which are configurable).

Taxes will always be deducted "at the source" and proceeded to a configurable "proceeds" wallet. This means: Taxes will always be paid by the sender of a transaction and taxes will be deducted directly from the send amount, resulting in the side-effect, that the recipient does not receive the gross amount of the transaction but only a tax-corrected net amount... This type of tax makes the token transparent for smart contracts, so that the token can be listed on DEXs or other DeFi protocols without the need for these protocols to explicitly support the tax.

The configuration options are laid out in a way such that the tokens use cases include (non exhaustive list):

- Simple flat p2p transaction tax
- Buy/Sell tax with configurable tax rates for buy and/or sell
- Buy-only taxes
- Sell-only taxes

However, due to technical limitations it is not always possible to map these use-cases 1:1 to a corresponding tax configuration layout. For example: Suppose you list your token on a DEX pool and then want to tax "buy" transactions from that pool. Then a possible tax configuration implementation would be to charge taxes on the `TransferMsg`s that come from that pool: Because `MsgTransfer` is used to send you tokens. But at the same time the pool also uses `MsgTransfer` to send tokens when the user issues an LP withdrawal. There fore it is impossible to distinguish a "buy" transaction from a "LP withdraw" operation from inside the token context, where the source tax is deducted.

## Instantiation Examples

In order to get a feeling for the tax configuration please refer to these configuration examples that cover some of the common use cases. The aim is to have a configuration that is as flexible and extensible as possible. This goes at a certain cost in terms of configuration verbosity.

### Buy/Sell Tax on Terraport

```
{
    "name": "Token",
    "symbol": "TOKEN",
    "decimals": 6,
    "initial_balances": [
        {
            // put your initial holder here
            "address": "<initial-holder-address>",
            // put 100bn x 1M here to have 100bn tokens
            "amount": "100000000000000000"
        }
    ],
    // tax config
    "tax_map": {
        "on_transfer": {
            "src_cond": {
                "ContractCode": {
                    "code_ids": [
                        8260    // Terraport pair code ID
                    ],
                    "tax_rate": "0.01"    // tax rate sell = 1%
                }
            },
            "dst_cond": {
                "Always": {
                    "tax_rate": "0.0"    // this tax rate does not matter
                }
            },
            "proceeds": "<proceeds-wallet>"    // the proceeds wallet receives all taxes
        },
        "on_send": {
            "src_cond": {
                "Always": {
                    "tax_rate": "0.01"    // tax rate buy = 1%
                }
            },
            "dst_cond": {
                // this triggers buy taxes on terraport pair
                "ContractCode": {
                    "code_ids": [
                        8260    // Terraport pair code ID
                    ],
                    "tax_rate": "0.0"    // tax rate does not matter
                }
            },
            "proceeds": "<proceeds-wallet>"
        },
        "on_transfer_from": {
            "src_cond": {
                "Never": {}
            },
            "dst_cond": {
                "Never": {}
            },
            "proceeds": ""
        },
        "on_send_from": {
            "src_cond": {
                "Never": {}
            },
            "dst_cond": {
                "Never": {}
            },
            "proceeds": ""
        },
        // this wallet can change tax policy
        "admin": "<tax-admin-wallet>"
    }
}
```

### Disclaimer

The code of this project **IS NOT AUDITED**. So please, proceed very carfully when using this software.
