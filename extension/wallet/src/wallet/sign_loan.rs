use baru::{loan::Borrower1, swap::sign_with_key};
use elements::{secp256k1_zkp::SECP256K1, sighash::SigHashCache, Transaction};
use futures::lock::Mutex;

use crate::{
    storage::Storage,
    wallet::{current, get_txouts, LoanDetails},
    Error, Wallet,
};

/// Load temporary loan_borrower state
//
/// When the frontend _asks_ the extension to
/// sign a loan, the information gets stored in the background script first.
/// When a request to bobtimus was made to actually take the loan,
/// this temporary loan details are saved in localStorage.
/// There can only be one pending loans at the time hence there is no identifier.
pub(crate) async fn sign_loan(
    name: String,
    current_wallet: &Mutex<Option<Wallet>>,
) -> Result<Transaction, Error> {
    let storage = Storage::local_storage().map_err(Error::Storage)?;

    let (borrower, loan_details) = storage
        .get_json_item::<(Borrower1, LoanDetails)>("borrower_state")
        .map_err(Error::Load)?
        .ok_or(Error::EmptyBorrowerState)?;
    let loan_transaction = sign_transaction(&name, current_wallet, &borrower).await?;
    // We don't broadcast this transaction ourselves, but we expect
    // the lender to do so very soon. We therefore save the borrower
    // state so that we can later on build, sign and broadcast the
    // repayment transaction
    update_open_loans(storage, &borrower, loan_details)?;
    Ok(loan_transaction)
}

async fn sign_transaction(
    name: &str,
    current_wallet: &Mutex<Option<Wallet>>,
    borrower: &Borrower1,
) -> Result<Transaction, Error> {
    let loan_transaction = borrower
        .sign(|mut transaction| async {
            let wallet = current(name, current_wallet).await?;
            let txouts = get_txouts(&wallet, |utxo, txout| Ok(Some((utxo, txout)))).await?;

            let mut cache = SigHashCache::new(&transaction);
            let witnesses = transaction
                .clone()
                .input
                .iter()
                .enumerate()
                .filter_map(|(index, input)| {
                    txouts
                        .iter()
                        .find(|(utxo, _)| {
                            utxo.txid == input.previous_output.txid
                                && utxo.vout == input.previous_output.vout
                        })
                        .map(|(_, txout)| (index, txout))
                })
                .map(|(index, output)| {
                    // TODO: It is convenient to use this import, but
                    // it is weird to use an API from the swap library
                    // here. Maybe we should move it to a common
                    // place, so it can be used for different
                    // protocols
                    let script_witness = sign_with_key(
                        SECP256K1,
                        &mut cache,
                        index,
                        &wallet.secret_key,
                        output.value,
                    );

                    (index, script_witness)
                })
                .collect::<Vec<_>>();

            for (index, witness) in witnesses {
                transaction.input[index].witness.script_witness = witness
            }

            Ok(transaction)
        })
        .await
        .map_err(Error::Sign)?;
    Ok(loan_transaction)
}

pub fn update_open_loans(
    storage: Storage,
    borrower: &Borrower1,
    loan_details: LoanDetails,
) -> Result<(), Error> {
    let mut open_loans = storage
        .get_json_item::<Vec<LoanDetails>>("open_loans")
        .map_err(Error::Load)?
        .unwrap_or_default();

    let txid = loan_details.txid;
    if open_loans.iter().all(|item| item.txid != txid) {
        open_loans.push(loan_details);
        storage
            .set_json_item("open_loans", &open_loans)
            .map_err(Error::Save)?;
        log::debug!("Stored new open loan: {} ", txid);
    }

    let loan_state_key = &format!("loan_state:{}", txid);
    if storage
        .get_json_item::<Borrower1>(loan_state_key)
        .map_err(Error::Load)?
        .is_none()
    {
        storage
            .set_json_item(loan_state_key, &borrower)
            .map_err(Error::Save)?;
        log::debug!("Stored new loan state: {} ", txid);
    }

    Ok(())
}
