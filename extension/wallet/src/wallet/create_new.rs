use anyhow::{bail, Context, Result};
use futures::lock::Mutex;

use crate::{
    storage::Storage,
    wallet::{ListOfWallets, Wallet},
};
use bip32::{ExtendedPrivateKey, Language, Mnemonic};
use rand::{thread_rng, RngCore};

pub fn bip39_seed_words(language: Language) -> Mnemonic {
    let mut entropy = [0u8; 32];
    thread_rng().fill_bytes(&mut entropy);
    Mnemonic::from_entropy(entropy, language)
}

pub async fn create_from_bip39(
    name: String,
    mnemonic: Mnemonic,
    password: String,
    current_wallet: &Mutex<Option<Wallet>>,
) -> Result<()> {
    let storage = Storage::local_storage()?;

    let mut wallets = storage
        .get_json_item::<ListOfWallets>("wallets")?
        .unwrap_or_default();

    if wallets.has(&name) {
        bail!("wallet with name '{}' already exists", name);
    }

    let params = if cfg!(debug_assertions) {
        // use weak parameters in debug mode, otherwise this is awfully slow
        log::warn!("using extremely weak scrypt parameters for password hashing");
        scrypt::ScryptParams::new(1, 1, 1).unwrap()
    } else {
        scrypt::ScryptParams::recommended()
    };

    let hashed_password =
        scrypt::scrypt_simple(&password, &params).context("failed to hash password")?;

    let secret_key_seed = mnemonic.to_seed(password.as_str());
    let xprv = ExtendedPrivateKey::new(secret_key_seed)?;
    let new_wallet = Wallet::initialize_new(name.clone(), password, xprv)?;

    storage.set_item(&format!("wallets.{}.password", name), hashed_password)?;
    storage.set_item(
        &format!("wallets.{}.xprv", name),
        format!(
            "{}${}",
            hex::encode(new_wallet.sk_salt),
            hex::encode(new_wallet.encrypted_xprv_key()?)
        ),
    )?;
    wallets.add(name);
    storage.set_json_item("wallets", wallets)?;

    current_wallet.lock().await.replace(new_wallet);

    log::info!("New wallet successfully initialized");

    Ok(())
}
