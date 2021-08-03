use crate::{
    wallet::{current, get_txouts, CreateSwapPayload, SwapUtxo, Wallet},
    BTC_ASSET_ID, USDT_ASSET_ID,
};
use coin_selection::{self, coin_select};
use elements::{bitcoin::Amount, secp256k1_zkp::SECP256K1, AssetId, OutPoint};
use estimate_transaction_size::avg_vbytes;
use futures::lock::Mutex;
use wasm_bindgen::UnwrapThrowExt;

pub async fn make_buy_create_swap_payload(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
    sell_amount: Amount,
) -> Result<CreateSwapPayload, Error> {
    let btc_asset_id = {
        let guard = BTC_ASSET_ID.lock().expect_throw("can get lock");
        *guard
    };
    let usdt_asset_id = {
        let guard = USDT_ASSET_ID.lock().expect_throw("can get lock");
        *guard
    };

    make_create_swap_payload(
        name,
        current_wallet,
        sell_amount,
        usdt_asset_id,
        btc_asset_id,
    )
    .await
}

pub async fn make_sell_create_swap_payload(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
    sell_amount: Amount,
) -> Result<CreateSwapPayload, Error> {
    let btc_asset_id = {
        let guard = BTC_ASSET_ID.lock().expect_throw("can get lock");
        *guard
    };
    make_create_swap_payload(
        name,
        current_wallet,
        sell_amount,
        btc_asset_id,
        btc_asset_id,
    )
    .await
}

async fn make_create_swap_payload(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
    sell_amount: Amount,
    sell_asset: AssetId,
    fee_asset: AssetId,
) -> Result<CreateSwapPayload, Error> {
    let wallet = current(&name, current_wallet)
        .await
        .map_err(Error::LoadWallet)?;
    let blinding_key = wallet.blinding_key();

    let utxos = get_txouts(&wallet, |utxo, txout| {
        Ok({
            let unblinded_txout = txout.unblind(SECP256K1, blinding_key)?;
            let outpoint = OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            };
            let candidate_asset = unblinded_txout.asset;

            if candidate_asset == sell_asset {
                Some(coin_selection::Utxo {
                    outpoint,
                    value: unblinded_txout.value,
                    script_pubkey: txout.script_pubkey,
                    asset: candidate_asset,
                })
            } else {
                log::debug!(
                    "utxo {} with asset id {} is not the sell asset, ignoring",
                    outpoint,
                    candidate_asset
                );
                None
            }
        })
    })
    .await
    .map_err(Error::GetTxOuts)?;

    let (bobs_fee_rate, fee_offset) = if fee_asset == sell_asset {
        // Bob currently hardcodes a fee-rate of 1 sat / vbyte, hence
        // there is no need for us to perform fee estimation. Later
        // on, both parties should probably agree on a block-target
        // and use the same estimation service.
        let bobs_fee_rate = Amount::from_sat(1);
        let fee_offset = calculate_fee_offset(bobs_fee_rate);

        (bobs_fee_rate, fee_offset)
    } else {
        (Amount::ZERO, Amount::ZERO)
    };

    let output = coin_select(
        utxos,
        sell_amount,
        bobs_fee_rate.as_sat() as f32,
        fee_offset,
    )
    .map_err(Error::CoinSelection)?;

    Ok(CreateSwapPayload {
        address: wallet.get_address(),
        alice_inputs: output
            .coins
            .into_iter()
            .map(|utxo| SwapUtxo {
                outpoint: utxo.outpoint,
                blinding_key,
            })
            .collect(),
        amount: output.target_amount,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Wallet is not loaded: {0}")]
    LoadWallet(anyhow::Error),
    #[error("Coin selection: {0}")]
    CoinSelection(coin_selection::Error),
    #[error("Failed to get transaction outputs: {0}")]
    GetTxOuts(anyhow::Error),
}

/// Calculate the fee offset required for the coin selection algorithm.
///
/// We are calculating this fee offset here so that we select enough coins to pay for the asset + the fee.
fn calculate_fee_offset(fee_sats_per_vbyte: Amount) -> Amount {
    let bobs_outputs = 2; // bob will create two outputs for himself (receive + change)
    let our_output = 1; // we have one additional output (the change output is priced in by the coin-selection algorithm)

    let fee_offset =
        ((bobs_outputs + our_output) * avg_vbytes::OUTPUT) * fee_sats_per_vbyte.as_sat();

    Amount::from_sat(fee_offset)
}
