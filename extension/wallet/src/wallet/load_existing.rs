use crate::{
    storage::Storage,
    wallet::{ListOfWallets, Wallet},
};
use anyhow::{bail, Context, Result};
use futures::lock::Mutex;

pub async fn load_existing(
    name: String,
    password: String,
    current_wallet: &Mutex<Option<Wallet>>,
) -> Result<()> {
    let mut guard = current_wallet.lock().await;

    if let Some(Wallet { name: loaded, .. }) = &*guard {
        bail!(
            "cannot load wallet '{}' because wallet '{}' is currently loaded",
            name,
            loaded
        )
    }

    let storage = Storage::local_storage()?;
    let wallets = storage
        .get_json_item::<ListOfWallets>("wallets")?
        .unwrap_or_default();

    if !wallets.has(&name) {
        bail!("wallet '{}' does not exist", name)
    }

    let stored_password = storage
        .get_item::<String>(&format!("wallets.{}.password", name))?
        .context("no password stored for wallet")?;

    scrypt::scrypt_check(&password, &stored_password)
        .with_context(|| format!("bad password for wallet '{}'", name))?;

    let xprv_ciphertext = storage
        .get_item::<String>(&format!("wallets.{}.xprv", name))?
        .context("no xprv key for wallet")?;

    let wallet = Wallet::initialize_existing(name, password, xprv_ciphertext)?;

    guard.replace(wallet);

    log::info!("Wallet successfully loaded");

    Ok(())
}
