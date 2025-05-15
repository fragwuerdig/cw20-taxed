# CW20 with Source Tax Extension

This repository is a fork of the [cw-plus](https://github.com/cosmwasm/cw-plus) repository - stripping down the original original repository to the bare minimum and implementing a new type of CW20 token: `cw20-taxed`. This new kind of taxed CW20 token is a type of token contract, that has a configurable tax system in place. A tax deduction can be triggered depending on which endpoint was called (e.g. `MsgSend`, `MsgTransfer`, ...) and depending on various conditions that the source and recipient wallets may have to fulfill (which are configurable).

Taxes will always be deducted "at the source" and proceeded to a configurable "proceeds" wallet. This means: Taxes will always be paid by the sender of a transaction and taxes will be deducted directly from the send amount, resulting in the side-effect, that the recipient does not receive the gross amount of the transaction but only a tax-corrected net amount... This type of tax makes the token transparent for smart contracts, so that the token can be listed on DEXs or other DeFi protocols without the need for these protocols to explicitly support the tax.

The configuration options are laid out in a way such that the tokens use cases include (non exhaustive list):

- Simple flat p2p transaction tax
- Buy/Sell tax with configurable tax rates for buy and/or sell
- Buy-only taxes
- Sell-only taxes

However, due to technical limitations it is not always possible to map these use-cases 1:1 to a corresponding tax configuration layout. For example: Suppose you list your token on a DEX pool and then want to tax "buy" transactions from that pool. Then a possible tax configuration implementation would be to charge taxes on the `TransferMsg`s that come from that pool: Because `MsgTransfer` is used to send you tokens. But at the same time the pool also uses `MsgTransfer` to send tokens when the user issues an LP withdrawal. There fore it is impossible to distinguish a "buy" transaction from a "LP withdraw" operation from inside the token context, where the source tax is deducted.

## On-Chain Deployments

This is a list of on-chain deployments for this contract:

| Version  | Code ID |
| -------- | ------- |
| `v1.1.0` | 8551 |
| `v1.1.0+taxed001` | 8654 |
| `v1.1.0+taxed003` | 9257 |
| `v1.1.0+taxed004` | <TBD> |

## Instructions to Migrate from Terraport/Terraswap Tokens

Terraport and Terraswap offer token factories in order to create and manage tokens in a user-friendly manner. These are stock cw20 contracts. Some might want to migrate from these tokens to this contract. Please follow these instructions:

1. Head to [Galaxy Station](https://station.terraclassic.community/contract) OR [TC Wallet](https://wallet.terra-classic.io/contracts)
2. Make sure Galaxy Extension or Keplr is connected to your tokens admin wallet 
3. In the search field type your token contract address address
4. When your token contract appears, click on "Migrate"
5. In the migration dialog use code id 8654
6. Type the migration message (see below these instructions)
7. Hit "Confirm" or "Ok" to sign your transaction

The migration message depends on what tax type your token shall have. Refer to the [instantiation examples] below to find the tax map that suits you. The general structure of the migration message will be:

```
{
  "tax_map": {
    "admin": " ... ",
    "on_transfer": { ... },
    "on_send": { ... },
    "on_transfer_from": { ... },
    "on_send_from": { ... },
  }
}
```

## Instantiation Examples

In order to get a feeling for the tax configuration please refer to these configuration examples that cover some of the common use cases. The aim is to have a configuration that is as flexible and extensible as possible. This goes at a certain cost in terms of configuration verbosity.

### Buy/Sell Tax on Terraport

This configuration will trigger taxes on sell/buy on a Terraport pair as well as on liquidity withdraws from the pool.

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

### Plain P2P Tax

This is a token that charges taxes on plain p2p transfers. Sending to contracts with `Cw20ReceiveMsg` (triggering some action within the contract) is tax free. However, when the contract transfers tokens back to a plain wallet, taxes will be charged.

```
{

    ...    // see above

    "tax_map": {
        "on_transfer": {
            "src_cond": {
                "Always": {
                    "tax_rate": "0.01"    // tax rate plain p2p sending = 1%
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
                "Never": {}
            },
            "dst_cond": {
                "Never": {}
            },
            "proceeds": ""
        },
        "on_transfer_from": {
            "src_cond": {
                "Always": {
                    "tax_rate": "0.01"    // tax rate plain p2p sending = 1%
                }
            },
            "dst_cond": {
                "Always": {
                    "tax_rate": "0.0"    // this tax rate does not matter
                }
            },
            "proceeds": "<proceeds-wallet>"    // the proceeds wallet receives all taxes
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

## Changing The Tax Map

If you want to change the tax layout, then the existing tax map can be modified by sending an `UpdateTaxMap` message to the token contract. For axample, you can open Galaxy Station, click on "Contract" on the left navigation bar. Then enter your contract address and click on "Execute". Now you have the chance to drop the execute message:

```
{
   "set_tax_map": {
      "tax_map": <your-tax-map-obj-here>
}
```

Now you can fire the message. After successful tx execution the tax map should be updated properly. A contract smart-query to retrieve the currently active tax map is yet to be implemented. 

## Disclaimer

The code of this project **IS NOT AUDITED**. So please, proceed very carfully when using this software.
