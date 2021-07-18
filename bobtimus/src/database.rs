use std::{convert::TryFrom, path::Path, sync::Arc};

use anyhow::Result;
use diesel::{prelude::*, Connection, SqliteConnection};
use elements::{encode::serialize_hex, Transaction, Txid};
use tokio::sync::Mutex;

use crate::schema::liquidations;

embed_migrations!("./migrations");

#[derive(Clone)]
pub struct Sqlite {
    connection: Arc<Mutex<SqliteConnection>>,
}

impl Sqlite {
    /// Return a handle that can be used to access the database.
    ///
    /// Reads or creates an SQLite database file at 'file'. When this returns
    /// an Sqlite database exists, a successful connection to the database has
    /// been made, and the database migrations have been run.
    pub fn new(file: &Path) -> Result<Self> {
        ensure_folder_tree_exists(file)?;

        let connection = SqliteConnection::establish(&format!("file:{}", file.display()))?;
        embedded_migrations::run(&connection)?;

        tracing::info!("SQLite database file loaded: {}", file.display());

        Ok(Sqlite {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Return a ephemeral db handle to be used for tests.
    ///
    /// The db file will be removed at the end of the process lifetime.
    #[cfg(test)]
    pub fn new_ephemeral_db() -> Result<Self> {
        let temp_file = tempfile::Builder::new()
            .suffix(".sqlite")
            .tempfile()
            .unwrap();
        Self::new(temp_file.path())
    }

    pub async fn do_in_transaction<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&SqliteConnection) -> anyhow::Result<T>,
    {
        let guard = self.connection.lock().await;
        let connection = &*guard;

        let result = connection.transaction(|| f(&connection))?;

        Ok(result)
    }
}

fn ensure_folder_tree_exists(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    Ok(())
}

#[derive(Insertable)]
#[table_name = "liquidations"]
pub struct LiquidationForm {
    id: String,
    tx_hex: String,
    locktime: i64,
}

impl LiquidationForm {
    pub fn new(loan_txid: Txid, liquidation_tx: &Transaction, locktime: u32) -> Self {
        let id = loan_txid.to_string();
        let tx_hex = serialize_hex(liquidation_tx);
        let locktime = i64::try_from(locktime).expect("every u32 fits into a i64");

        Self {
            id,
            tx_hex,
            locktime,
        }
    }

    pub fn insert(self, conn: &SqliteConnection) -> Result<()> {
        diesel::insert_into(liquidations::table)
            .values(self)
            .execute(conn)?;

        Ok(())
    }
}

pub mod queries {
    use super::*;

    use elements::encode::deserialize;

    #[derive(Associations, Clone, Debug, Queryable, PartialEq)]
    #[table_name = "liquidations"]
    struct Liquidation {
        id: String,
        tx_hex: String,
        locktime: i64,
    }

    pub fn get_publishable_liquidations_txs(
        conn: &SqliteConnection,
        blockcount: u32,
    ) -> Result<Vec<Transaction>> {
        let txs = liquidations::table
            .filter(liquidations::locktime.le(blockcount as i64))
            .get_results::<Liquidation>(conn)?;

        let txs = txs
            .into_iter()
            .map(|liquidation| Ok(deserialize(&hex::decode(liquidation.tx_hex)?)?))
            .collect::<Result<Vec<_>>>()?;

        Ok(txs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db() -> PathBuf {
        let temp_file = tempfile::Builder::new()
            .suffix(".sqlite")
            .tempfile()
            .unwrap();

        temp_file.into_temp_path().to_path_buf()
    }

    #[test]
    fn can_create_a_new_temp_db() {
        let path = temp_db();

        let db = Sqlite::new(&path);

        assert!(&db.is_ok());
    }

    #[test]
    fn given_no_database_exists_calling_new_creates_it() {
        let path = temp_db();
        // validate assumptions: the db does not exist yet
        assert!(!path.as_path().exists());

        let db = Sqlite::new(&path);

        assert!(&db.is_ok());
        assert!(&path.as_path().exists());
    }

    #[test]
    fn given_db_in_non_existing_directory_tree_calling_new_creates_it() {
        let tempfile = tempfile::tempdir().unwrap();
        let mut path = PathBuf::new();

        path.push(tempfile);
        path.push("i_dont_exist");
        path.push("database.sqlite");

        // validate assumptions:
        // 1. the db does not exist yet
        // 2. the parent folder does not exist yet
        assert!(!path.as_path().exists());
        assert!(!path.parent().unwrap().exists());

        let db = Sqlite::new(&path);

        assert!(&db.is_ok());
        assert!(&path.exists());
    }
}
