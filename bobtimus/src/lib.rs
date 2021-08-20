#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;

use std::collections::HashMap;

use crate::{
    database::{queries, Sqlite},
    elements_rpc::{Client, ElementsRpc},
};
use anyhow::{Context, Result};
use baru::{
    input::Input,
    loan::{Lender0, Lender1, LoanResponse},
    swap,
};
use database::LiquidationForm;
use elements::{
    bitcoin::{
        secp256k1::{All, Secp256k1},
        Amount,
    },
    secp256k1_zkp::{
        rand::{CryptoRng, RngCore},
        SecretKey, SECP256K1,
    },
    Address, AssetId, OutPoint, Transaction, Txid,
};
use futures::{stream, stream::FuturesUnordered, Stream, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::watch::Receiver;

mod amounts;

pub mod cli;
pub mod database;
pub mod elements_rpc;
pub mod fixed_rate;
pub mod http;
pub mod kraken;
pub mod loan;
pub mod problem;
pub mod schema;

use crate::loan::{
    loan_calculation_and_validation, LoanOffer, LoanRequest, Ltv, Term, ValidatedLoan,
};
pub use amounts::*;
use elements::bitcoin::PublicKey;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::{
    convert::TryFrom,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const USDT_ASSET_ID: &str = "ce091c998b83c78bb71a632313ba3760f1763d9cfcffae02258ffa9865a37bd2";

pub struct Bobtimus<R, RS> {
    pub rng: R,
    pub rate_service: RS,
    pub secp: Secp256k1<All>,
    pub elementsd: Client,
    pub btc_asset_id: AssetId,
    pub usdt_asset_id: AssetId,
    pub db: Sqlite,
    pub lender_states: HashMap<Txid, Lender1>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSwapPayload {
    pub alice_inputs: Vec<AliceInput>,
    pub address: Address,
    pub amount: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct AliceInput {
    pub outpoint: OutPoint,
    pub blinding_key: SecretKey,
}

impl<R, RS> Bobtimus<R, RS>
where
    R: RngCore + CryptoRng,
    RS: LatestRate,
{
    /// Handle Alice's request to create a swap transaction in which
    /// she buys L-BTC from us and in return we get L-USDt from her.
    pub async fn handle_create_buy_swap(
        &mut self,
        payload: CreateSwapPayload,
    ) -> Result<Transaction> {
        let usdt_amount = LiquidUsdt::from_satodollar(payload.amount);
        let latest_rate = self.rate_service.latest_rate();
        let btc_amount = latest_rate.sell_base(usdt_amount)?;

        let transaction = self
            .swap_transaction(
                (self.usdt_asset_id, usdt_amount.into()),
                (self.btc_asset_id, btc_amount.into()),
                payload.alice_inputs,
                payload.address,
                self.btc_asset_id,
            )
            .await?;

        Ok(transaction)
    }

    /// Handle Alice's request to create a swap transaction in which
    /// she sells L-BTC and we give her L-USDt.
    pub async fn handle_create_sell_swap(
        &mut self,
        payload: CreateSwapPayload,
    ) -> Result<Transaction> {
        let btc_amount = Amount::from_sat(payload.amount);
        let latest_rate = self.rate_service.latest_rate();
        let usdt_amount = latest_rate.buy_quote(btc_amount.into())?;

        let transaction = self
            .swap_transaction(
                (self.btc_asset_id, btc_amount),
                (self.usdt_asset_id, usdt_amount.into()),
                payload.alice_inputs,
                payload.address,
                self.btc_asset_id,
            )
            .await?;

        Ok(transaction)
    }

    async fn find_inputs(
        elements_client: &Client,
        asset_id: AssetId,
        input_amount: Amount,
    ) -> Result<Vec<Input>> {
        let bob_inputs = elements_client
            .select_inputs_for(asset_id, input_amount, false)
            .await
            .context("failed to select inputs for swap")?;

        let master_blinding_key = elements_client
            .dumpmasterblindingkey()
            .await
            .context("failed to dump master blinding key")?;

        let master_blinding_key = hex::decode(master_blinding_key)?;

        let bob_inputs = bob_inputs
            .into_iter()
            .map(|(outpoint, txout)| {
                use hmac::{Hmac, Mac, NewMac};
                use sha2::Sha256;

                let mut mac = Hmac::<Sha256>::new_varkey(&master_blinding_key)
                    .expect("HMAC can take key of any size");
                mac.update(txout.script_pubkey.as_bytes());

                let result = mac.finalize();
                let input_blinding_sk = SecretKey::from_slice(&result.into_bytes())?;

                Result::<_, anyhow::Error>::Ok(Input {
                    txin: outpoint,
                    original_txout: txout,
                    blinding_key: input_blinding_sk,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(bob_inputs)
    }

    async fn swap_transaction(
        &mut self,
        (alice_input_asset_id, alice_input_amount): (AssetId, Amount),
        (bob_input_asset_id, bob_input_amount): (AssetId, Amount),
        alice_inputs: Vec<AliceInput>,
        alice_address: Address,
        btc_asset_id: AssetId,
    ) -> Result<Transaction> {
        let bob_inputs = Self::find_inputs(&self.elementsd, bob_input_asset_id, bob_input_amount)
            .await
            .context("could not find transaction inputs for Bob")?;

        let bob_address = self
            .elementsd
            .get_new_segwit_confidential_address()
            .await
            .context("failed to get redeem address")?;

        let alice_inputs = alice_inputs
            .iter()
            .copied()
            .map(
                |AliceInput {
                     outpoint,
                     blinding_key,
                 }| {
                    let client = self.elementsd.clone();
                    async move {
                        let transaction = client
                            .get_raw_transaction(outpoint.txid)
                            .await
                            .with_context(|| {
                                format!("failed to fetch transaction {}", outpoint.txid)
                            })?;

                        let txout = transaction
                            .output
                            .get(outpoint.vout as usize)
                            .with_context(|| {
                                format!(
                                    "vout index {} is not valid for transaction {}",
                                    outpoint.vout, outpoint.txid
                                )
                            })?
                            .clone();

                        Result::<_, anyhow::Error>::Ok(Input {
                            txin: outpoint,
                            original_txout: txout,
                            blinding_key,
                        })
                    }
                },
            )
            .collect::<FuturesUnordered<_>>()
            .try_collect::<Vec<_>>()
            .await?;

        let alice = swap::Actor::new(
            &self.secp,
            alice_inputs,
            alice_address,
            bob_input_asset_id,
            bob_input_amount,
        )?;

        let bob = swap::Actor::new(
            &self.secp,
            bob_inputs,
            bob_address,
            alice_input_asset_id,
            alice_input_amount,
        )?;

        let transaction = swap::bob_create_transaction(
            &mut self.rng,
            &self.secp,
            alice,
            bob,
            btc_asset_id,
            Amount::from_sat(1), // TODO: Make this dynamic once there is something going on on Liquid
            {
                let elementsd = self.elementsd.clone();
                move |transaction| async move {
                    let tx = elementsd.sign_raw_transaction(&transaction).await?;

                    Result::<_, anyhow::Error>::Ok(tx)
                }
            },
        )
        .await?;

        Ok(transaction)
    }

    /// Handle the borrower's loan offer request
    ///
    /// We return the range of possible loan terms to the borrower.
    /// The borrower can then request a loan using parameters that are within our terms.
    pub async fn handle_loan_offer_request(&mut self) -> Result<LoanOffer> {
        Ok(self.current_loan_offer())
    }

    fn current_loan_offer(&mut self) -> LoanOffer {
        LoanOffer {
            rate: self.rate_service.latest_rate(),
            // TODO: Dynamic fee estimation
            fee_sats_per_vbyte: Amount::from_sat(1),
            min_principal: LiquidUsdt::from_str_in_dollar("100")
                .expect("static value to be convertible"),
            max_principal: LiquidUsdt::from_str_in_dollar("10000")
                .expect("static value to be convertible"),
            // TODO: Maximum LTV to be decided by a model
            max_ltv: dec!(0.8),
            // TODO: Interest to be decided by a model
            base_interest_rate: dec!(0.05),
            // TODO: Potentially fine-tune the model with these values
            terms: vec![
                Term {
                    days: 30,
                    interest_mod: Decimal::ZERO,
                },
                Term {
                    days: 60,
                    interest_mod: Decimal::ZERO,
                },
                Term {
                    days: 120,
                    interest_mod: Decimal::ZERO,
                },
            ],
            initial_ltvs: vec![Ltv { ltv: dec!(0.5) }, Ltv { ltv: dec!(0.7) }],
        }
    }

    /// Handle the borrower's loan request in which she puts up L-BTC as
    /// collateral and we lend L-USDt to her which she will have to
    /// repay in the future.
    pub async fn handle_loan_request(&mut self, loan_request: LoanRequest) -> Result<LoanResponse> {
        let loan_offer = self.current_loan_offer();
        // TODO: Make configurable
        let price_fluctuation_interval = (dec!(0.99), dec!(1.01));

        // The bid price is used so the lender is covered under the assumption of selling the asset
        let current_price = self.rate_service.latest_rate().bid;

        let ValidatedLoan {
            repayment_amount,
            liquidation_price,
        } = loan_calculation_and_validation(
            &loan_request,
            &loan_offer,
            price_fluctuation_interval,
            current_price,
        )?;

        let oracle_secret_key = elements::secp256k1_zkp::key::ONE_KEY;
        let oralce_priv_key = elements::bitcoin::PrivateKey::new(
            oracle_secret_key,
            elements::bitcoin::Network::Regtest,
        );
        let oracle_pk = PublicKey::from_private_key(&self.secp, &oralce_priv_key);

        let timelock = days_to_unix_timestamp_timelock(loan_request.term, SystemTime::now())?;

        let lender_address = self
            .elementsd
            .get_new_segwit_confidential_address()
            .await
            .context("failed to get lender address")?;

        let address_blinder = self
            .elementsd
            .get_address_blinding_key(&lender_address)
            .await?;

        let lender0 = Lender0::new(
            &mut self.rng,
            self.btc_asset_id,
            self.usdt_asset_id,
            lender_address,
            address_blinder,
            oracle_pk,
        )
        .unwrap();

        let elementsd_client = self.elementsd.clone();
        let principal_inputs = Self::find_inputs(
            &elementsd_client,
            self.usdt_asset_id,
            loan_request.principal_amount.into(),
        )
        .await?;

        let lender1 = lender0
            .build_loan_transaction(
                &mut self.rng,
                SECP256K1,
                loan_offer.fee_sats_per_vbyte,
                (
                    loan_request.collateral_amount.into(),
                    loan_request.collateral_inputs,
                ),
                (loan_request.principal_amount.into(), principal_inputs),
                repayment_amount.into(),
                liquidation_price.as_satodollar(),
                (loan_request.borrower_pk, loan_request.borrower_address),
                timelock,
            )
            .await
            .context("Failed to build loan transaction")?;

        let loan_response = lender1.loan_response();

        self.lender_states
            .insert(loan_response.transaction().txid(), lender1);

        Ok(loan_response)
    }

    /// Handle the borrower's request to finalize a loan.
    ///
    /// If we still agree with the loan transaction sent by the borrower, we
    /// will sign and broadcast it.
    ///
    /// Additionally, we save the signed liquidation transaction so
    /// that we can broadcast it when the locktime is reached.
    pub async fn finalize_loan(&mut self, transaction: Transaction) -> Result<Txid> {
        // TODO: We should only take into account loan transactions which
        // are relatively recent e.g. within 1 minute. We expect the
        // borrower to quickly perform the protocol and let us broadcast
        // the loan transaction

        let lender = self
            .lender_states
            .get(&transaction.txid())
            .context("unknown loan transaction")?;

        let transaction = lender
            .finalise_loan(transaction, {
                let elementsd = self.elementsd.clone();
                |transaction| async move { elementsd.sign_raw_transaction(&transaction).await }
            })
            .await?;

        let txid = self.elementsd.send_raw_transaction(&transaction).await?;

        let liquidation_tx = lender
            .liquidation_transaction(&mut self.rng, &self.secp, Amount::ONE_SAT)
            .await?;
        let locktime = lender.collateral_contract().timelock();

        self.db
            .do_in_transaction(|conn| {
                LiquidationForm::new(txid, &liquidation_tx, *locktime).insert(conn)?;

                Ok(())
            })
            .await?;

        Ok(txid)
    }
}

pub trait LatestRate {
    fn latest_rate(&mut self) -> Rate;
}

#[derive(Clone)]
pub struct RateSubscription {
    receiver: Receiver<Rate>,
}

impl From<Receiver<Rate>> for RateSubscription {
    fn from(receiver: Receiver<Rate>) -> Self {
        Self { receiver }
    }
}

impl RateSubscription {
    pub fn into_stream(self) -> impl Stream<Item = Result<Rate>> {
        stream::try_unfold(self.receiver, |mut receiver| async move {
            receiver
                .changed()
                .await
                .context("failed to receive latest rate update")?;

            let latest_rate = *receiver.borrow();

            Ok(Some((latest_rate, receiver)))
        })
    }
}

pub async fn liquidate_loans(elementsd: &Client, db: Sqlite) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let secs_since_epoch = now.as_secs();

    let liquidation_txs = db
        .do_in_transaction(|conn| {
            let txs = queries::get_publishable_liquidations_txs(conn, secs_since_epoch)?;
            Ok(txs)
        })
        .await?;

    for tx in liquidation_txs.iter() {
        match elementsd.send_raw_transaction(tx).await {
            Ok(txid) => log::info!("Broadcast liquidation transaction {}", txid),
            Err(e) => log::error!("Failed to broadcast liquidation transaction: {}", e),
        };
    }

    Ok(())
}
/// Calculates the absolute timelock from the loan term in days
///
/// The timelock is represented as Unix timestamp (seconds since the epoch).
/// Note: Miniscript uses u32 for representing the timestamp so we return a u32.
fn days_to_unix_timestamp_timelock(term_in_days: u32, now: SystemTime) -> Result<u32> {
    let since_the_epoch = now.duration_since(UNIX_EPOCH).expect("Time went backwards");

    let term = Duration::from_secs((term_in_days * 24 * 60 * 60) as u64);

    let timelock = (since_the_epoch + term).as_secs();
    let timelock = u32::try_from(timelock)
        .context("Overflow, the given timestamp appears to be too far in the future")?;

    Ok(timelock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        elements_rpc::{Client, ElementsRpc, ListUnspentOptions},
        fixed_rate,
    };
    use anyhow::{Context, Result};
    use baru::swap::sign_with_key;
    use elements::{
        bitcoin::{secp256k1::Secp256k1, Amount, Network, PrivateKey, PublicKey},
        secp256k1_zkp::{rand::thread_rng, SecretKey, SECP256K1},
        sighash::SigHashCache,
        Address, AddressParams, OutPoint, Transaction, TxOut,
    };
    use elements_harness::Elementsd;
    use proptest::proptest;
    use testcontainers::clients::Cli;

    // This test ensures that this function will not panic on different systems now and in the future.
    // At the point of writing 30868 days were supported, equivalent to 84.569863 calendar years.
    // We allow a maximum of 18250 days = 50 years for loan terms.
    // This test will pass for the next ~34.5 years given a correct system time.
    proptest! {
        #[test]
        fn timelock_calculation_does_not_panic_between_1_day_and_100_years(
            term_in_days in 1u32..18250, // 18250 days = 50 years
        ) {
            let now = SystemTime::now();
            let _ = days_to_unix_timestamp_timelock(term_in_days, now).unwrap();
        }
    }

    #[test]
    fn timelock_calculation_30_days() {
        let term_in_days = 30;
        let now = SystemTime::now();

        let since_epoch = u32::try_from(now.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();

        let timelock = days_to_unix_timestamp_timelock(term_in_days, now).unwrap();

        let difference = timelock - since_epoch;

        // 2_592_000 = 30 days in secs
        assert_eq!(difference, 2_592_000)
    }

    #[tokio::test]
    async fn test_handle_btc_sell_swap_request() {
        let db = Sqlite::new_ephemeral_db().expect("A ephemeral db");

        let tc_client = Cli::default();
        let (client, _container) = {
            let blockchain = Elementsd::new(&tc_client, "0.18.1.9").unwrap();

            (
                Client::new(blockchain.node_url.clone().into()).unwrap(),
                blockchain,
            )
        };
        let mining_address = client.get_new_segwit_confidential_address().await.unwrap();

        let have_asset_id_alice = client.get_bitcoin_asset_id().await.unwrap();
        let have_asset_id_bob = client.issueasset(100_000.0, 0.0, true).await.unwrap().asset;

        let rate_service = fixed_rate::Service::new();
        let redeem_amount_bob = Amount::ONE_BTC;

        let (
            fund_address_alice,
            fund_sk_alice,
            _fund_pk_alice,
            fund_blinding_sk_alice,
            _fund_blinding_pk_alice,
        ) = make_confidential_address();

        let fund_alice_txid = client
            .send_asset_to_address(
                &fund_address_alice,
                redeem_amount_bob + Amount::ONE_BTC,
                Some(have_asset_id_alice),
            )
            .await
            .unwrap();
        client.generatetoaddress(1, &mining_address).await.unwrap();

        let input_alice = extract_input(
            &client.get_raw_transaction(fund_alice_txid).await.unwrap(),
            fund_address_alice,
        )
        .unwrap();

        let (
            final_address_alice,
            _final_sk_alice,
            _final_pk_alice,
            _final_blinding_sk_alice,
            _final_blinding_pk_alice,
        ) = make_confidential_address();

        // move issued asset to wallet address
        let address = client.get_new_segwit_confidential_address().await.unwrap();
        let _txid = client
            .send_asset_to_address(
                &address,
                Amount::from_btc(10.0).unwrap(),
                Some(have_asset_id_bob),
            )
            .await
            .unwrap();
        client.generatetoaddress(1, &mining_address).await.unwrap();

        let mut bob = Bobtimus {
            rng: &mut thread_rng(),
            rate_service,
            secp: Secp256k1::new(),
            elementsd: client.clone(),
            btc_asset_id: have_asset_id_alice,
            usdt_asset_id: have_asset_id_bob,
            db,
            lender_states: HashMap::new(),
        };

        let transaction = bob
            .handle_create_sell_swap(CreateSwapPayload {
                alice_inputs: vec![AliceInput {
                    outpoint: input_alice.0,
                    blinding_key: fund_blinding_sk_alice,
                }],
                address: final_address_alice,
                amount: redeem_amount_bob.as_sat(),
            })
            .await
            .unwrap();

        let transaction = swap::alice_finalize_transaction(transaction, {
            let value = input_alice.1.value;
            move |mut tx| async move {
                let input_index = tx
                    .input
                    .iter()
                    .position(|txin| fund_alice_txid == txin.previous_output.txid)
                    .context("transaction does not contain input")?;
                let mut cache = SigHashCache::new(&tx);

                tx.input[input_index].witness.script_witness =
                    sign_with_key(SECP256K1, &mut cache, input_index, &fund_sk_alice, value);

                Ok(tx)
            }
        })
        .await
        .unwrap();

        let _txid = client.send_raw_transaction(&transaction).await.unwrap();
        let _txid = client.generatetoaddress(1, &mining_address).await.unwrap();

        let utxos = client
            .listunspent(
                None,
                None,
                None,
                None,
                Some(ListUnspentOptions {
                    asset: Some(have_asset_id_alice),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();

        let fee = transaction.fee_in(have_asset_id_bob);
        assert!(utxos.iter().any(|utxo| (utxo.amount
            - (redeem_amount_bob.as_btc() - Amount::from_sat(fee).as_btc()))
        .abs()
            < f64::EPSILON
            && utxo.spendable));
    }

    #[tokio::test]
    async fn test_handle_btc_buy_swap_request() {
        let db = Sqlite::new_ephemeral_db().expect("A ephemeral db");

        let tc_client = Cli::default();
        let (client, _container) = {
            let blockchain = Elementsd::new(&tc_client, "0.18.1.9").unwrap();

            (
                Client::new(blockchain.node_url.clone().into()).unwrap(),
                blockchain,
            )
        };
        let mining_address = client.get_new_segwit_confidential_address().await.unwrap();

        let have_asset_id_alice = client.issueasset(100_000.0, 0.0, true).await.unwrap().asset;
        let have_asset_id_bob = client.get_bitcoin_asset_id().await.unwrap();

        let rate_service = fixed_rate::Service::new();
        let redeem_amount_bob = LiquidUsdt::from_str_in_dollar("20000.0").unwrap();

        let (
            fund_address_alice,
            fund_sk_alice,
            _fund_pk_alice,
            fund_blinding_sk_alice,
            _fund_blinding_pk_alice,
        ) = make_confidential_address();

        let fund_alice_txid = client
            .send_asset_to_address(
                &fund_address_alice,
                Amount::from_btc(10_000.0).unwrap() + redeem_amount_bob.into(),
                Some(have_asset_id_alice),
            )
            .await
            .unwrap();
        client.generatetoaddress(1, &mining_address).await.unwrap();

        let input_alice = extract_input(
            &client.get_raw_transaction(fund_alice_txid).await.unwrap(),
            fund_address_alice,
        )
        .unwrap();

        let (
            final_address_alice,
            _final_sk_alice,
            _final_pk_alice,
            _final_blinding_sk_alice,
            _final_blinding_pk_alice,
        ) = make_confidential_address();

        let mut bob = Bobtimus {
            rng: &mut thread_rng(),
            rate_service,
            secp: Secp256k1::new(),
            elementsd: client.clone(),
            btc_asset_id: have_asset_id_bob,
            usdt_asset_id: have_asset_id_alice,
            db,
            lender_states: HashMap::new(),
        };

        let transaction = bob
            .handle_create_buy_swap(CreateSwapPayload {
                alice_inputs: vec![AliceInput {
                    outpoint: input_alice.0,
                    blinding_key: fund_blinding_sk_alice,
                }],
                address: final_address_alice,
                amount: redeem_amount_bob.as_satodollar(),
            })
            .await
            .unwrap();

        let transaction = swap::alice_finalize_transaction(transaction, {
            let value = input_alice.1.value;
            move |mut tx| async move {
                let input_index = tx
                    .input
                    .iter()
                    .position(|txin| fund_alice_txid == txin.previous_output.txid)
                    .context("transaction does not contain input")?;
                let mut cache = SigHashCache::new(&tx);

                tx.input[input_index].witness.script_witness =
                    sign_with_key(SECP256K1, &mut cache, input_index, &fund_sk_alice, value);

                Ok(tx)
            }
        })
        .await
        .unwrap();

        let _txid = client.send_raw_transaction(&transaction).await.unwrap();
        let _txid = client.generatetoaddress(1, &mining_address).await.unwrap();

        let utxos = client
            .listunspent(
                None,
                None,
                None,
                None,
                Some(ListUnspentOptions {
                    asset: Some(have_asset_id_alice),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();

        assert!(utxos.iter().any(
            |utxo| (utxo.amount - Amount::from(redeem_amount_bob).as_btc()).abs() < f64::EPSILON
                && utxo.spendable
        ));
    }

    fn extract_input(tx: &Transaction, address: Address) -> Result<(OutPoint, TxOut)> {
        let vout = tx
            .output
            .iter()
            .position(|output| output.script_pubkey == address.script_pubkey())
            .context("Tx doesn't pay to address")?;

        let outpoint = OutPoint {
            txid: tx.txid(),
            vout: vout as u32,
        };
        let txout = tx.output[vout].clone();
        Ok((outpoint, txout))
    }

    fn make_keypair() -> (SecretKey, PublicKey) {
        let sk = SecretKey::new(&mut thread_rng());
        let pk = PublicKey::from_private_key(
            SECP256K1,
            &PrivateKey {
                compressed: true,
                network: Network::Regtest,
                key: sk,
            },
        );

        (sk, pk)
    }

    fn make_confidential_address() -> (Address, SecretKey, PublicKey, SecretKey, PublicKey) {
        let (sk, pk) = make_keypair();
        let (blinding_sk, blinding_pk) = make_keypair();

        (
            Address::p2wpkh(&pk, Some(blinding_pk.key), &AddressParams::ELEMENTS),
            sk,
            pk,
            blinding_sk,
            blinding_pk,
        )
    }
}
