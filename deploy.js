#!/usr/bin/env node

import yargs from 'yargs/yargs';
import { hideBin } from 'yargs/helpers';
import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';
import { Secp256k1HdWallet } from '@cosmjs/amino';
import { GasPrice } from '@cosmjs/stargate';
import { stringToPath } from "@cosmjs/crypto";
import dotenv from 'dotenv';

dotenv.config({ quiet: true });

const mnemonic = process.env.MNEMONIC;

const argv = yargs(hideBin(process.argv))
    .positional('name', {
        describe: 'Token name',
        type: 'string',
        demandOption: true
    })
    .positional('symbol', {
        describe: 'Token symbol',
        type: 'string',
        demandOption: true
    })
    .positional('supply', {
        describe: 'Initial supply',
        type: 'string',
        demandOption: true
    })
    .positional('owner', {
        describe: 'Owner address (terra1...)',
        type: 'string',
        demandOption: true
    })
    .option('mintable', {
        alias: 'm',
        describe: 'Mintable address',
        type: 'string',
        default: ''
    })
    .option('sell-tax', {
        alias: 's',
        describe: 'Sell tax in percent (e.g., 2 for 2%)',
        type: 'number',
        default: 0
    })
    .option('buy-tax', {
        alias: 'b',
        describe: 'Buy tax in percent (e.g., 2 for 2%)',
        type: 'number',
        default: 0
    })
    .option('pool-id', {
        alias: 'p',
        describe: 'Pool ID for tax calculation',
        type: 'number'
    })
    .option('rpc', {
        alias: 'r',
        describe: 'RPC endpoint',
        type: 'string',
        demandOption: true,
    })
    .option('code', {
        describe: 'Smart contract code of the token',
        type: 'string',
        demandOption: true
    })
    .help()
    .argv;

const [name, symbol, supply, owner] = argv._;

if (argv.sellTax > 0) {
    // if sell tax provided, check the pool-id flag is also provided
    if (!argv.poolId) {
        console.error('Error: --pool-id flag is required when using --sell-tax');
        process.exit(1);
    }
    console.log(`Sell tax: ${argv['sell-tax']}%`);
} else {
    console.log('No sell tax specified.');
}

// supply needs to be a positve integer
if (supply <= 0 || !Number.isInteger(Number(supply))) {
    console.error('Error: supply must be a positive integer');
    process.exit(1);
}

// symbol must be between 3 and 6 upper case characters
if (!/^[A-Z]{3,6}$/.test(symbol)) {
    console.error('Error: symbol must be between 3 and 6 upper case characters');
    process.exit(1);
}

// owner must be a valid terra address (starts with terra1 and is 44 characters long)
if (!/^terra1[0-9a-z]{38}$/.test(owner)) {
    console.error('Error: owner must be a valid terra address (starts with terra1 and is 44 characters long)');
    process.exit(1);
}

// mintable addresses must be valid terra addresses (starts with terra1 and is 44 characters long)

if (argv.mintable && !/^terra1[0-9a-z]{38}$/.test(argv.mintable)) {
    console.error(`Error: mintable address ${argv.mintable} is not a valid terra address (starts with terra1 and is 44 characters long)`);
    process.exit(1);
}

if (argv['sell-tax'] && (argv['sell-tax'] < 0 || argv['sell-tax'] > 100)) {
    console.error('Error: --sell-tax must be between 0 and 100');
    process.exit(1);
}

if (argv['buy-tax'] && (argv['buy-tax'] < 0 || argv['buy-tax'] > 100)) {
    console.error('Error: --buy-tax must be between 0 and 100');
    process.exit(1);
}

if (!mnemonic) {
    console.error("Error: MNEMONIC environment variable not set");
    process.exit(1);
}

if (!argv.rpc) {
    console.error("Error: --rpc flag is required");
    process.exit(1);
}

const rpcEndpoint = argv.rpc;
// validate rpc endpoint is a valid url
try {
    new URL(rpcEndpoint);
} catch (e) {
    console.error("Error: --rpc flag must be a valid URL");
    process.exit(1);
}

console.log('Deploying contract with:');
console.log(`Name: ${name}`);
console.log(`Symbol: ${symbol}`);
console.log(`Supply: ${supply}`);
console.log(`Owner: ${owner}`);
console.log(`Mintable addresses: ${argv.mintable || 'None'}`);
console.log(`Pool ID: ${argv.poolId || 'N/A'}`);
console.log(`Sell tax: ${argv.sellTax || 0}%`);
console.log(`Buy tax: ${argv.buyTax || 0}%`);

// build the instantiate message
const msg = {
    name: name,
    symbol: symbol,
    decimals: 6,
    initial_balances: [
        { address: owner, amount: supply.toString() }
    ]
}

if (argv.sellTax > 0 || argv.buyTax > 0) {
    msg['tax_map'] = {};

    msg['tax_map']['on_transfer'] = {};
    msg['tax_map']['on_send'] = {};
    msg['tax_map']['on_transfer']['proceeds'] = owner;
    msg['tax_map']['on_send']['proceeds'] = owner;

    // dummy conditions to make the structure valid#
    msg['tax_map']['on_transfer_from'] = {};
    msg['tax_map']['on_send_from'] = {};
    msg['tax_map']['on_transfer_from']['src_cond'] = { Always: { tax_rate: "0" } };
    msg['tax_map']['on_transfer_from']['dst_cond'] = { Always: { tax_rate: "0" } };
    msg['tax_map']['on_transfer_from']['proceeds'] = "";
    msg['tax_map']['on_send_from']['src_cond'] = { Always: { tax_rate: "0" } };
    msg['tax_map']['on_send_from']['dst_cond'] = { Always: { tax_rate: "0" } };
    msg['tax_map']['on_send_from']['proceeds'] = "";

    // buy <=> transfer from pool to buyer
    msg['tax_map']['on_transfer']['src_cond'] = {
        ContractCode: { code_ids: [argv.poolId], tax_rate: (argv.buyTax / 100).toString() },
    };
    msg['tax_map']['on_transfer']['dst_cond'] = {
        Always: { tax_rate: "0" },

    };

    // sell <=> send from seller to pool
    msg['tax_map']['on_send']['src_cond'] = {
        Always: { tax_rate: "0" },
    };
    msg['tax_map']['on_send']['dst_cond'] = {
        ContractCode: { code_ids: [argv.poolId], tax_rate: (argv.sellTax / 100).toString() },
    };

    msg['tax_map']['admin'] = owner;

}

if (argv.mintable) {
    msg['mint'] = { minter: argv.mintable };
}

const code = Number(argv.code);
if (isNaN(code) || code <= 0) {
    console.error('Error: --code must be a positive integer');
    process.exit(1);
}

console.log('Instantiate message:');
console.log(JSON.stringify(msg, null, 2));

// Deployment logic goes here
// You should set these environment variables or replace with your values

const wallet = await Secp256k1HdWallet.fromMnemonic(mnemonic, { prefix: "terra", hdPaths: [stringToPath("m/44'/330'/0'/0/0")] });
const [account] = await wallet.getAccounts();
const client = await SigningCosmWasmClient.connectWithSigner(
    rpcEndpoint,
    wallet,
    { gasPrice: GasPrice.fromString("29uluna"), prefix: "terra"  }
);

client.instantiate(account.address, code, msg, name, "auto", { memo: "Deployed with cw20-taxed deploy script" })
    .then((res) => {
        console.log('Contract deployed at address:', res.contractAddress);
        console.log('Transaction hash:', res.transactionHash);
    })
    .catch((err) => {
        console.error('Error deploying contract:', err);
    });