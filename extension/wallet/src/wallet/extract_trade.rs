use crate::{
    wallet::{compute_balances, current, get_txouts, Wallet},
    TradeSide,
};
use anyhow::{bail, Context, Result};
use elements::{confidential, secp256k1_zkp::SECP256K1, Transaction, TxOut};
use futures::lock::Mutex;
use itertools::Itertools;
use serde::{Deserialize, Serialize};

// TODO: Public APIs should return specific error struct/enum
pub async fn extract_trade(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
    transaction: Transaction,
) -> Result<Trade> {
    let wallet = current(&name, current_wallet).await?;

    let txouts = get_txouts(&wallet, |utxo, txout| Ok(Some((utxo, txout)))).await?;
    let balances = compute_balances(
        &wallet,
        &txouts
            .iter()
            .map(|(_, txout)| txout)
            .cloned()
            .collect::<Vec<_>>(),
    );

    let blinding_key = wallet.blinding_key();

    let our_inputs = transaction
        .input
        .iter()
        .filter_map(|txin| {
            txouts
                .iter()
                .map(|(utxo, txout)| {
                    let is_ours = utxo.txid == txin.previous_output.txid
                        && utxo.vout == txin.previous_output.vout;
                    if !is_ours {
                        return Ok(None);
                    }

                    Ok(match txout {
                        TxOut {
                            asset: confidential::Asset::Explicit(asset),
                            value: confidential::Value::Explicit(value),
                            ..
                        } => Some((*asset, *value)),
                        txout => {
                            let unblinded = txout.unblind(SECP256K1, blinding_key)?;

                            Some((unblinded.asset, unblinded.value))
                        }
                    })
                })
                .find_map(|res| res.transpose())
        })
        .collect::<Result<Vec<_>>>()
        .context("failed to unblind one of our inputs")?;

    let (sell_asset, sell_input) = our_inputs
        .into_iter()
        .into_grouping_map()
        .fold(0, |sum, _asset, value| sum + value)
        .into_iter()
        .exactly_one()
        .context("expected single input asset type")?;

    let our_address = wallet.get_address();
    let our_outputs = transaction
        .output
        .iter()
        .filter_map(|txout| match txout {
            TxOut {
                asset: confidential::Asset::Explicit(asset),
                value: confidential::Value::Explicit(value),
                script_pubkey,
                ..
            } if script_pubkey == &our_address.script_pubkey() => Some((*asset, *value)),
            TxOut {
                asset: confidential::Asset::Explicit(_),
                value: confidential::Value::Explicit(_),
                ..
            } => {
                log::debug!(
                    "ignoring explicit outputs that do not pay to our address, including fees"
                );
                None
            }
            txout => match txout.unblind(SECP256K1, blinding_key) {
                Ok(unblinded) => Some((unblinded.asset, unblinded.value)),
                _ => None,
            },
        })
        .into_grouping_map()
        .fold(0, |sum, _asset, value| sum + value)
        .into_iter()
        .collect_tuple()
        .context("wrong number of outputs, expected 2")?;

    let ((buy_asset, buy_amount), change_amount) = match our_outputs {
        ((change_asset, change_amount), buy_output) if change_asset == sell_asset => {
            (buy_output, change_amount)
        }
        (buy_output, (change_asset, change_amount)) if change_asset == sell_asset => {
            (buy_output, change_amount)
        }
        _ => bail!("no output corresponds to the sell asset"),
    };
    let sell_amount = sell_input - change_amount;

    let sell_balance = balances
        .iter()
        .find_map(|entry| {
            if entry.asset == sell_asset {
                Some(entry.value)
            } else {
                None
            }
        })
        .context("no balance for sell asset")?;

    let buy_balance = balances
        .iter()
        .find_map(|entry| {
            if entry.asset == buy_asset {
                Some(entry.value)
            } else {
                None
            }
        })
        .unwrap_or_default();

    Ok(Trade {
        sell: TradeSide::new_sell(sell_asset, sell_amount, sell_balance)?,
        buy: TradeSide::new_buy(buy_asset, buy_amount, buy_balance)?,
    })
}

#[derive(Clone, Deserialize, Serialize, Debug, PartialEq)]
pub struct Trade {
    pub sell: TradeSide,
    pub buy: TradeSide,
}
