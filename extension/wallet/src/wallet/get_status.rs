use crate::{
    storage::Storage,
    wallet::{ListOfWallets, Wallet},
};
use anyhow::Result;
use futures::lock::Mutex;

pub async fn get_status(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
) -> Result<WalletStatus> {
    let storage = Storage::local_storage()?;

    let wallets = storage
        .get_json_item::<ListOfWallets>("wallets")?
        .unwrap_or_default();
    let exists = wallets.has(&name);

    let guard = current_wallet.lock().await;
    let loaded = guard.as_ref().map_or(false, |w| w.name == name);

    Ok(WalletStatus { loaded, exists })
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct WalletStatus {
    pub loaded: bool,
    pub exists: bool,
}
