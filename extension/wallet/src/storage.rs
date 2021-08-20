use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::{error::Error as StdError, str::FromStr};
use web_sys::window;

/// A wrapper type around the cache storage.
pub struct Storage {
    inner: web_sys::Storage,
}

impl Storage {
    pub fn local_storage() -> Result<Self> {
        let storage = map_err_to_anyhow!(window()
            .context("failed to access window object")?
            .local_storage())?
        .context("no local storage available")?;

        Ok(storage.into())
    }

    /// return an item parsing it using `from_str`
    pub fn get_item<T>(&self, name: &str) -> Result<Option<T>>
    where
        T: FromStr,
        <T as FromStr>::Err: StdError + Send + Sync + 'static,
    {
        let value = map_err_to_anyhow!(self.inner.get_item(name))?;

        let value = match value {
            Some(value) => value,
            None => return Ok(None),
        };

        let t = T::from_str(&value).context("failed to parse item from string")?;

        Ok(Some(t))
    }

    /// store an item in local storage using `to_string`
    pub fn set_item<V>(&self, name: &str, value: V) -> Result<()>
    where
        V: ToString,
    {
        map_err_to_anyhow!(self.inner.set_item(name, &value.to_string()))?;

        Ok(())
    }

    /// return an item parsing it using `serde_json::from_str`
    pub fn get_json_item<T>(&self, name: &str) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let value = map_err_to_anyhow!(self.inner.get_item(name))?;

        let value = match value {
            Some(value) => value,
            None => return Ok(None),
        };

        let t = serde_json::from_str::<T>(value.as_str()).context("failed to deserialize item")?;

        Ok(Some(t))
    }

    /// store an item in local storage using `serde_json::to_string`
    pub fn set_json_item<V>(&self, name: &str, value: V) -> Result<()>
    where
        V: Serialize,
    {
        let value = serde_json::to_string(&value).context("Could not serialize value")?;

        map_err_to_anyhow!(self.inner.set_item(name, &value))?;

        Ok(())
    }

    pub fn remove_item(&self, name: &str) -> Result<()> {
        map_err_to_anyhow!(self.inner.remove_item(name))?;

        Ok(())
    }
}

impl From<web_sys::Storage> for Storage {
    fn from(inner: web_sys::Storage) -> Self {
        Self { inner }
    }
}
