use crate::{
    assets::{self, lookup},
    esplora,
    esplora::Utxo,
    CHAIN, DEFAULT_SAT_PER_VBYTE,
};
use aes_gcm_siv::{
    aead::{Aead, NewAead},
    Aes256GcmSiv,
};
use anyhow::{bail, Context, Result};
use elements::{
    bitcoin::{
        self,
        secp256k1::{SecretKey, SECP256K1},
        util::amount::Amount,
    },
    confidential,
    secp256k1_zkp::{rand, PublicKey},
    Address, AssetId, OutPoint, TxOut, Txid,
};
use futures::{
    lock::{MappedMutexGuard, Mutex, MutexGuard},
    stream::FuturesUnordered,
    StreamExt, TryStreamExt,
};
use hkdf::Hkdf;
use itertools::Itertools;
use rand::{thread_rng, Rng};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{digest::generic_array::GenericArray, Sha256};
use std::{
    convert::Infallible,
    fmt,
    ops::{Add, Sub},
    str,
};
use wasm_bindgen::UnwrapThrowExt;

use bip32::{ExtendedPrivateKey, Prefix};
pub use create_new::{bip39_seed_words, create_from_bip39};
pub use extract_loan::{extract_loan, Error as ExtractLoanError};
pub use extract_trade::{extract_trade, Trade};
pub use get_address::get_address;
pub use get_balances::get_balances;
pub use get_status::{get_status, WalletStatus};
pub use get_transaction_history::get_transaction_history;
pub use load_existing::load_existing;
pub use loan_backup::{create_loan_backup, load_loan_backup, BackupDetails};
pub use make_create_swap_payload::{
    make_buy_create_swap_payload, make_sell_create_swap_payload, Error as MakePayloadError,
};
pub use make_loan_request::{make_loan_request, Error as MakeLoanRequestError};
pub use repay_loan::{repay_loan, Error as RepayLoanError};
pub(crate) use sign_and_send_swap_transaction::sign_and_send_swap_transaction;
pub(crate) use sign_loan::sign_loan;
use std::str::FromStr;
pub use unload_current::unload_current;
pub use withdraw_everything_to::withdraw_everything_to;

mod create_new;
mod extract_loan;
mod extract_trade;
mod get_address;
mod get_balances;
mod get_status;
mod get_transaction_history;
mod load_existing;
mod loan_backup;
mod make_create_swap_payload;
mod make_loan_request;
mod repay_loan;
mod sign_and_send_swap_transaction;
mod sign_loan;
mod unload_current;
mod withdraw_everything_to;

async fn get_txouts<T, FM: Fn(Utxo, TxOut) -> Result<Option<T>> + Copy>(
    wallet: &Wallet,
    filter_map: FM,
) -> Result<Vec<T>> {
    let address = wallet.get_address();

    let utxos = esplora::fetch_utxos(&address).await?;

    let txouts = utxos
        .into_iter()
        .map(move |utxo| async move {
            let mut tx = esplora::fetch_transaction(utxo.txid).await?;
            let txout = tx.output.remove(utxo.vout as usize);

            filter_map(utxo, txout)
        })
        .collect::<FuturesUnordered<_>>()
        .filter_map(|r| std::future::ready(r.transpose()))
        .try_collect::<Vec<_>>()
        .await?;

    Ok(txouts)
}

async fn current<'n, 'w>(
    name: &'n str,
    current_wallet: &'w Mutex<Option<Wallet>>,
) -> Result<MappedMutexGuard<'w, Option<Wallet>, Wallet>> {
    let mut guard = current_wallet.lock().await;

    match &mut *guard {
        Some(wallet) if wallet.name == name => {}
        _ => bail!("wallet with name '{}' is currently not loaded", name),
    };

    Ok(MutexGuard::map(guard, |w| w.as_mut().unwrap()))
}

#[derive(Debug)]
pub struct Wallet {
    name: String,
    encryption_key: [u8; 32],
    secret_key: SecretKey,
    xprv: ExtendedPrivateKey<SecretKey>,
    sk_salt: [u8; 32],
}

const SECRET_KEY_ENCRYPTION_NONCE: &[u8; 12] = b"SECRET_KEY!!";

impl Wallet {
    pub fn initialize_new(
        name: String,
        password: String,
        root_xprv: ExtendedPrivateKey<SecretKey>,
    ) -> Result<Self> {
        let sk_salt = thread_rng().gen::<[u8; 32]>();

        let encryption_key = Self::derive_encryption_key(&password, &sk_salt)?;

        // TODO: derive key according to some derivation path
        let secret_key = root_xprv.to_bytes();

        Ok(Self {
            name,
            encryption_key,
            sk_salt,
            secret_key: SecretKey::from_slice(&secret_key)?,
            xprv: root_xprv,
        })
    }

    pub fn initialize_existing(
        name: String,
        password: String,
        xprv_ciphertext: String,
    ) -> Result<Self> {
        let mut parts = xprv_ciphertext.split('$');

        let salt = parts.next().context("no salt in cipher text")?;
        let xprv = parts.next().context("no secret key in cipher text")?;

        let mut sk_salt = [0u8; 32];
        hex::decode_to_slice(salt, &mut sk_salt).context("failed to decode salt as hex")?;

        let encryption_key = Self::derive_encryption_key(&password, &sk_salt)?;

        let cipher = Aes256GcmSiv::new(GenericArray::from_slice(&encryption_key));
        let nonce = GenericArray::from_slice(SECRET_KEY_ENCRYPTION_NONCE);
        let xprv = cipher
            .decrypt(
                nonce,
                hex::decode(xprv)
                    .context("failed to decode xpk as hex")?
                    .as_slice(),
            )
            .context("failed to decrypt secret key")?;

        let xprv = String::from_utf8(xprv)?;
        let root_xprv = ExtendedPrivateKey::from_str(xprv.as_str())?;

        // TODO: derive key according to some derivation path
        let secret_key = root_xprv.to_bytes();

        Ok(Self {
            name,
            encryption_key,
            secret_key: SecretKey::from_slice(&secret_key)?,
            xprv: root_xprv,
            sk_salt,
        })
    }

    pub fn get_public_key(&self) -> PublicKey {
        PublicKey::from_secret_key(SECP256K1, &self.secret_key)
    }

    pub fn get_address(&self) -> Address {
        let chain = {
            let guard = CHAIN.lock().expect_throw("can get lock");
            *guard
        };
        let public_key = self.get_public_key();
        let blinding_key = PublicKey::from_secret_key(SECP256K1, &self.blinding_key());

        Address::p2wpkh(
            &bitcoin::PublicKey {
                compressed: true,
                key: public_key,
            },
            Some(blinding_key),
            chain.into(),
        )
    }

    /// Encrypts the extended private key with the encryption key.
    ///
    /// # Choice of nonce
    ///
    /// We store the extended private key on disk and as such have to use a constant nonce, otherwise we would not be able to decrypt it again.
    /// The encryption only happens once and as such, there is conceptually only one message and we are not "reusing" the nonce which would be insecure.
    fn encrypted_xprv_key(&self) -> Result<Vec<u8>> {
        let cipher = Aes256GcmSiv::new(GenericArray::from_slice(&self.encryption_key));
        let xprv = &self.xprv.to_string(Prefix::XPRV);
        let enc_sk = cipher
            .encrypt(
                GenericArray::from_slice(SECRET_KEY_ENCRYPTION_NONCE),
                xprv.as_bytes(),
            )
            .context("failed to encrypt secret key")?;

        Ok(enc_sk)
    }

    /// Derive the blinding key.
    ///
    /// # Choice of salt
    ///
    /// We choose to not add a salt because the ikm is already a randomly-generated, secret value with decent entropy.
    ///
    /// # Choice of ikm
    ///
    /// We derive the blinding key from the secret key to avoid having to store two secret values on disk.
    ///
    /// # Choice of info
    ///
    /// We choose to tag the derived key with `b"BLINDING_KEY"` in case we ever want to derive something else from the secret key.
    fn blinding_key(&self) -> SecretKey {
        let h = Hkdf::<sha2::Sha256>::new(None, self.secret_key.as_ref());

        let mut bk = [0u8; 32];
        h.expand(b"BLINDING_KEY", &mut bk)
            .expect("output length aligns with sha256");

        SecretKey::from_slice(bk.as_ref()).expect("always a valid secret key")
    }

    /// Derive the encryption key from the wallet's password and a salt.
    ///
    /// # Choice of salt
    ///
    /// The salt of HKDF can be public or secret and while it can operate without a salt, it is better to pass a salt value [0].
    ///
    /// # Choice of ikm
    ///
    /// The user's password is our input key material. The stronger the password, the better the resulting encryption key.
    ///
    /// # Choice of info
    ///
    /// HKDF can operate without `info`, however, it is useful to "tag" the derived key with its usage.
    /// In our case, we use the encryption key to encrypt the secret key and as such, tag it with `b"ENCRYPTION_KEY"`.
    ///
    /// [0]: https://tools.ietf.org/html/rfc5869#section-3.1
    fn derive_encryption_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
        let h = Hkdf::<Sha256>::new(Some(salt), password.as_bytes());
        let mut enc_key = [0u8; 32];
        h.expand(b"ENCRYPTION_KEY", &mut enc_key)
            .context("failed to derive encryption key")?;

        Ok(enc_key)
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct ListOfWallets(Vec<String>);

impl ListOfWallets {
    fn has(&self, wallet: &str) -> bool {
        self.0.iter().any(|w| w == wallet)
    }

    fn add(&mut self, wallet: String) {
        self.0.push(wallet);
    }
}

impl str::FromStr for ListOfWallets {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split = s.split('\t');

        Ok(ListOfWallets(split.map(|s| s.to_owned()).collect()))
    }
}

impl fmt::Display for ListOfWallets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join("\t"))
    }
}

/// Represents the payload for creating a swap.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CreateSwapPayload {
    pub alice_inputs: Vec<SwapUtxo>,
    pub address: Address,
    #[serde(with = "elements::bitcoin::util::amount::serde::as_sat")]
    pub amount: elements::bitcoin::Amount,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct SwapUtxo {
    pub outpoint: OutPoint,
    pub blinding_key: SecretKey,
}

/// A single balance entry as returned by [`get_balances`].
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct BalanceEntry {
    pub asset: AssetId,
    pub ticker: String,
    pub value: Decimal,
}

impl BalanceEntry {
    pub fn for_asset(asset: AssetId, ticker: String, value: u64, precision: u32) -> Self {
        let mut decimal = Decimal::from(value);
        decimal
            .set_scale(precision)
            .expect("precision must be < 28");

        Self {
            asset,
            ticker,
            value: decimal,
        }
    }
}

/// A pure function to compute the balances of the wallet given a set of [`TxOut`]s.
fn compute_balances(wallet: &Wallet, txouts: &[TxOut]) -> Vec<BalanceEntry> {
    let grouped_txouts = txouts
        .iter()
        .filter_map(|utxo| match utxo {
            TxOut {
                asset: confidential::Asset::Explicit(asset),
                value: confidential::Value::Explicit(value),
                ..
            } => Some((*asset, *value)),
            txout => match txout.unblind(SECP256K1, wallet.blinding_key()) {
                Ok(unblinded_txout) => Some((unblinded_txout.asset, unblinded_txout.value)),
                Err(e) => {
                    log::warn!("failed to unblind txout: {}", e);
                    None
                }
            },
        })
        .into_group_map();

    grouped_txouts
        .into_iter()
        .filter_map(|(asset, utxos)| {
            let total_sum = utxos.into_iter().sum();
            let (ticker, precision) = lookup(asset)?;

            Some(BalanceEntry::for_asset(
                asset,
                ticker.to_owned(),
                total_sum,
                precision as u32,
            ))
        })
        .collect()
}

#[derive(Clone, Deserialize, Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TradeSide {
    pub ticker: String,
    pub amount: Decimal,
    pub balance_before: Decimal,
    pub balance_after: Decimal,
}

impl TradeSide {
    fn new_sell(asset: AssetId, amount: u64, current_balance: Decimal) -> Result<Self> {
        Self::new(asset, amount, current_balance, Decimal::sub)
    }

    fn new_buy(asset: AssetId, amount: u64, current_balance: Decimal) -> Result<Self> {
        Self::new(asset, amount, current_balance, Decimal::add)
    }

    fn new(
        asset: AssetId,
        amount: u64,
        current_balance: Decimal,
        balance_after: impl Fn(Decimal, Decimal) -> Decimal,
    ) -> Result<Self> {
        let (ticker, precision) = assets::lookup(asset).context("asset not found")?;

        let mut amount = Decimal::from(amount);
        amount
            .set_scale(precision as u32)
            .expect("precision must be < 28");

        Ok(Self {
            ticker: ticker.to_owned(),
            amount,
            balance_before: current_balance,
            balance_after: balance_after(current_balance, amount),
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoanDetails {
    pub collateral: TradeSide,
    pub principal: TradeSide,
    pub principal_repayment: Decimal,
    // TODO: Express as target date or number of days instead?
    pub term: u32,
    pub txid: Txid,
}

impl LoanDetails {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        collateral_asset: AssetId,
        collateral_amount: Amount,
        collateral_balance: Decimal,
        principal_asset: AssetId,
        principal_amount: Amount,
        principal_balance: Decimal,
        timelock: u32,
        txid: Txid,
    ) -> Result<Self> {
        let collateral = TradeSide::new_sell(
            collateral_asset,
            collateral_amount.as_sat(),
            collateral_balance,
        )?;

        let principal = TradeSide::new_buy(
            principal_asset,
            principal_amount.as_sat(),
            principal_balance,
        )?;

        Ok(Self {
            collateral,
            principal_repayment: principal.amount,
            principal,
            term: timelock,
            txid,
        })
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser_tests {
    use wasm_bindgen_test::*;

    use super::*;
    use bip32::{Language, Mnemonic};

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    async fn create_new(
        name: String,
        password: String,
        current_wallet: &Mutex<Option<Wallet>>,
    ) -> Result<()> {
        let mnemonic = Mnemonic::new("globe favorite camp draw action kid soul junk space soda genre vague name brisk female circle equal fix decade gloom elbow address genius noodle", Language::English).unwrap();
        create_from_bip39(name, mnemonic, password, current_wallet).await
    }

    fn set_elements_chain_in_local_storage() {
        crate::Storage::local_storage()
            .unwrap()
            .set_item("CHAIN", "ELEMENTS")
            .unwrap();
    }

    #[wasm_bindgen_test]
    pub async fn given_no_wallet_when_getting_address_then_fails() {
        set_elements_chain_in_local_storage();

        let current_wallet = Mutex::default();
        let error = get_address("no-existent-wallet".to_owned(), &current_wallet)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "wallet with name 'no-existent-wallet' is currently not loaded"
        );
    }

    #[wasm_bindgen_test]
    pub async fn given_a_wallet_can_get_an_address() {
        set_elements_chain_in_local_storage();

        let current_wallet = Mutex::default();
        create_new("wallet-1".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();

        let result = get_address("wallet-1".to_owned(), &current_wallet).await;

        assert!(result.is_ok());
    }

    #[wasm_bindgen_test]
    pub async fn given_a_wallet_when_unloaded_cannot_get_address() {
        let current_wallet = Mutex::default();
        create_new("wallet-2".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();

        unload_current(&current_wallet).await;
        let error = get_address("wallet-2".to_owned(), &current_wallet)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "wallet with name 'wallet-2' is currently not loaded"
        );
    }

    #[wasm_bindgen_test]
    pub async fn cannot_create_two_wallets_with_same_name() {
        let current_wallet = Mutex::default();

        create_new("wallet-3".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();
        let error = create_new("wallet-3".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "wallet with name 'wallet-3' already exists"
        );
    }

    #[wasm_bindgen_test]
    pub async fn cannot_load_multiple_wallets_at_the_same_time() {
        let current_wallet = Mutex::default();

        create_new("wallet-4".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();
        create_new("wallet-5".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();

        let error = load_existing("wallet-4".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "cannot load wallet 'wallet-4' because wallet 'wallet-5' is currently loaded"
        );
    }

    #[wasm_bindgen_test]
    pub async fn cannot_load_wallet_with_wrong_password() {
        let current_wallet = Mutex::default();

        create_new("wallet-6".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();
        unload_current(&current_wallet).await;

        let error = load_existing("wallet-6".to_owned(), "bar".to_owned(), &current_wallet)
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "bad password for wallet 'wallet-6'");
    }

    #[wasm_bindgen_test]
    pub async fn cannot_load_wallet_that_doesnt_exist() {
        let current_wallet = Mutex::default();

        let error = load_existing("foobar".to_owned(), "bar".to_owned(), &current_wallet)
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "wallet 'foobar' does not exist");
    }

    #[wasm_bindgen_test]
    pub async fn new_wallet_is_automatically_loaded() {
        let current_wallet = Mutex::default();

        create_new("wallet-7".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();
        let status = get_status("wallet-7".to_owned(), &current_wallet)
            .await
            .unwrap();

        assert_eq!(status.loaded, true);
    }

    #[wasm_bindgen_test]
    pub async fn given_unknown_wallet_status_returns_that_it_doesnt_exist() {
        let current_wallet = Mutex::default();

        let status = get_status("wallet-8".to_owned(), &current_wallet)
            .await
            .unwrap();

        assert_eq!(status.exists, false);
    }

    #[wasm_bindgen_test]
    pub async fn secret_key_can_be_successfully_decrypted() {
        let current_wallet = Mutex::default();

        create_new("wallet-9".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();
        let initial_sk = {
            let guard = current_wallet.lock().await;
            let wallet = guard.as_ref().unwrap();

            wallet.secret_key.clone()
        };

        unload_current(&current_wallet).await;

        load_existing("wallet-9".to_owned(), "foo".to_owned(), &current_wallet)
            .await
            .unwrap();
        let loaded_sk = {
            let guard = current_wallet.lock().await;
            let wallet = guard.as_ref().unwrap();

            wallet.secret_key.clone()
        };

        assert_eq!(initial_sk, loaded_sk);
    }
}
