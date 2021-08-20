use crate::{
    storage::Storage,
    wallet::{current, get_txouts, Wallet},
    BTC_ASSET_ID, USDT_ASSET_ID,
};
use baru::{
    input::Input,
    loan::{Borrower0, LoanRequest},
};
use coin_selection::{self, coin_select};
use elements::{bitcoin::util::amount::Amount, secp256k1_zkp::SECP256K1, OutPoint};
use estimate_transaction_size::avg_vbytes;
use futures::lock::Mutex;
use rand::thread_rng;
use wasm_bindgen::UnwrapThrowExt;

pub async fn make_loan_request(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
    collateral_amount: Amount,
    fee_rate: Amount,
) -> Result<LoanRequest, Error> {
    let btc_asset_id = {
        let guard = BTC_ASSET_ID.lock().expect_throw("can get lock");
        *guard
    };
    let usdt_asset_id = {
        let guard = USDT_ASSET_ID.lock().expect_throw("can get lock");
        *guard
    };

    let (address, blinding_key) = {
        let wallet = current(&name, current_wallet)
            .await
            .map_err(Error::LoadWallet)?;

        let address = wallet.get_address();
        let blinding_key = wallet.blinding_key();

        (address, blinding_key)
    };

    let coin_selector = {
        |amount, asset| async move {
            let wallet = current(&name, current_wallet).await?;

            let utxos = get_txouts(&wallet, |utxo, txout| {
                Ok({
                    let unblinded_txout = txout.unblind(SECP256K1, blinding_key)?;
                    let outpoint = OutPoint {
                        txid: utxo.txid,
                        vout: utxo.vout,
                    };
                    let candidate_asset = unblinded_txout.asset;

                    if candidate_asset == asset {
                        Some((
                            coin_selection::Utxo {
                                outpoint,
                                value: unblinded_txout.value,
                                script_pubkey: txout.script_pubkey.clone(),
                                asset: candidate_asset,
                            },
                            txout,
                        ))
                    } else {
                        log::debug!(
                            "utxo {} with asset id {} is not the target asset, ignoring",
                            outpoint,
                            candidate_asset
                        );
                        None
                    }
                })
            })
            .await?;

            let fee_offset = calculate_fee_offset(fee_rate);

            let output = coin_select(
                utxos.iter().map(|(utxo, _)| utxo).cloned().collect(),
                amount,
                fee_rate.as_sat() as f32,
                fee_offset,
            )?;
            let selection = output
                .coins
                .iter()
                .map(|coin| {
                    let original_txout = utxos
                        .iter()
                        .find_map(|(utxo, txout)| (utxo.outpoint == coin.outpoint).then(|| txout))
                        .expect("same source of utxos")
                        .clone();

                    Input {
                        txin: coin.outpoint,
                        original_txout,
                        blinding_key,
                    }
                })
                .collect();

            Ok(selection)
        }
    };

    let borrower_state_0 = Borrower0::new(
        &mut thread_rng(),
        coin_selector,
        address,
        blinding_key,
        collateral_amount,
        fee_rate,
        btc_asset_id,
        usdt_asset_id,
    )
    .await
    .map_err(Error::BuildBorrowerState)?;

    let storage = Storage::local_storage().map_err(Error::Storage)?;
    storage
        .set_json_item("borrower0_state", &borrower_state_0)
        .map_err(Error::Save)?;

    // TODO: Fix, use new API here
    #[allow(deprecated)]
    Ok(borrower_state_0.loan_request())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Wallet is not loaded {0}")]
    LoadWallet(anyhow::Error),
    #[error("Failed to construct borrower state: {0}")]
    BuildBorrowerState(anyhow::Error),
    #[error("Storage error: {0}")]
    Storage(anyhow::Error),
    #[error("Failed to save item to storage: {0}")]
    Save(anyhow::Error),
}

/// Calculate the fee offset required for the coin selection algorithm.
///
/// We are calculating this fee offset here so that we select enough coins to pay for the asset + the fee.
fn calculate_fee_offset(fee_sats_per_vbyte: Amount) -> Amount {
    let principal_outputs = 2; // one to pay the principal to the borrower and another as change for the lender
    let fee_offset = (principal_outputs * avg_vbytes::OUTPUT) * fee_sats_per_vbyte.as_sat();

    Amount::from_sat(fee_offset)
}
