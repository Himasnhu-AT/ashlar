//! Secrets (API keys, tokens) in the OS keychain.
//!
//! | System | Store | Item |
//! |---|---|---|
//! | macOS | the login Keychain (never iCloud) | generic password, service = the app's service name, account = the secret's name |
//! | Windows | Credential Manager | generic credential, persistence "local machine" (does not roam) |
//! | Linux | the Secret Service over D-Bus (GNOME Keyring, KWallet, KeePassXC…) | item in the default collection |
//! | anything else | none | `Unsupported` |
//!
//! There is no fallback to a file: when no store answers, saving fails with the reason. Nothing
//! here logs, and no error text is built from a `Debug` of the binding's errors (some of their
//! variants carry the stored bytes). No `unsafe`: the `keyring-core` store crates have safe APIs.

use crate::{NativeError, Result};

/// The secrets of one app, filed under `service` (use the app's bundle identifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keychain {
    service: String,
}

/// A secret's name: `A-Z`, `a-z`, `0-9`, `_`, `-`, `.`; at most 64.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

fn check_name(name: &str) -> Result<()> {
    if valid_name(name) { Ok(()) } else { Err(NativeError::Invalid("a secret's name may hold letters, digits, `_`, `-` and `.` only".into())) }
}

/// Longest secret accepted.
pub const MAX_SECRET: usize = 16 * 1024;

impl Keychain {
    /// The store for `service` (`com.example.editor`). Fails on an empty or odd service name.
    pub fn new(service: impl Into<String>) -> Result<Self> {
        let service = service.into();
        if !crate::secrets::valid_service(&service) {
            return Err(NativeError::Invalid("a service name may hold letters, digits, `_`, `-` and `.` only".into()));
        }
        Ok(Self { service })
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    /// The secret named `name`, `None` when there is none.
    pub fn get(&self, name: &str) -> Result<Option<String>> {
        check_name(name)?;
        imp::get(&self.service, name)
    }

    /// Save (or replace) the secret named `name`.
    pub fn set(&self, name: &str, value: &str) -> Result<()> {
        check_name(name)?;
        if value.is_empty() || value.len() > MAX_SECRET {
            return Err(NativeError::Invalid(format!("a secret must be 1 to {MAX_SECRET} bytes")));
        }
        imp::set(&self.service, name, value)
    }

    /// Remove the secret named `name`; `false` when there was none.
    pub fn delete(&self, name: &str) -> Result<bool> {
        check_name(name)?;
        imp::delete(&self.service, name)
    }

    /// Whether a store answers at all (a Linux session without a keyring does not).
    pub fn available(&self) -> Result<()> {
        imp::available()
    }
}

fn valid_service(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
mod imp {
    use std::collections::HashMap;
    use std::sync::Arc;

    use keyring_core::api::CredentialStoreApi;
    use keyring_core::{Entry, Error};

    use super::*;

    #[cfg(target_os = "macos")]
    const NAME: &str = "the macOS Keychain";
    #[cfg(target_os = "windows")]
    const NAME: &str = "Windows Credential Manager";
    #[cfg(target_os = "linux")]
    const NAME: &str = "the Secret Service keyring";

    type Store = Arc<dyn CredentialStoreApi + Send + Sync>;

    #[cfg(target_os = "macos")]
    fn open() -> keyring_core::Result<Store> {
        Ok(apple_native_keyring_store::keychain::Store::new()?)
    }
    #[cfg(target_os = "windows")]
    fn open() -> keyring_core::Result<Store> {
        Ok(windows_native_keyring_store::Store::new()?)
    }
    #[cfg(target_os = "linux")]
    fn open() -> keyring_core::Result<Store> {
        Ok(zbus_secret_service_keyring_store::Store::new()?)
    }

    fn modifiers() -> Option<HashMap<&'static str, &'static str>> {
        #[cfg(target_os = "windows")]
        {
            // Stay on this computer instead of roaming with a domain profile.
            Some(HashMap::from([("persistence", "local")]))
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    fn os_text(e: &(dyn std::error::Error + Send + Sync)) -> String {
        crate::os_text(e)
    }

    /// A binding error as ours, from fixed texts and the OS's message only: never from the
    /// error's `Debug`, which for `BadEncoding` / `BadDataFormat` holds the stored bytes.
    pub(super) fn map(e: Error) -> NativeError {
        match e {
            Error::NoStorageAccess(os) => {
                NativeError::Denied(format!("{NAME} refused ({}); unlock it, or allow the app when the system asks", os_text(os.as_ref())))
            }
            Error::PlatformFailure(os) => NativeError::Os(format!("{NAME} failed ({})", os_text(os.as_ref()))),
            Error::NoDefaultStore => NativeError::Unsupported,
            Error::BadEncoding(_) | Error::BadDataFormat(..) => {
                NativeError::Os(format!("the item saved in {NAME} is not text; remove it and save it again"))
            }
            Error::Ambiguous(found) => NativeError::Os(format!("{NAME} holds {} items for this name; remove the extra ones", found.len())),
            Error::TooLong(..) => NativeError::Invalid(format!("the value is longer than {NAME} accepts")),
            Error::NoEntry => NativeError::Os(format!("{NAME} has no such item")),
            _ => NativeError::Os(format!("{NAME} could not do that")),
        }
    }

    fn entry(service: &str, name: &str) -> Result<Entry> {
        let store = open().map_err(map)?;
        store.build(service, name, modifiers().as_ref()).map_err(map)
    }

    pub fn get(service: &str, name: &str) -> Result<Option<String>> {
        crate::guarded("the keychain", || match entry(service, name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(map(e)),
        })
    }

    pub fn set(service: &str, name: &str, value: &str) -> Result<()> {
        crate::guarded("the keychain", || entry(service, name)?.set_password(value).map_err(map))
    }

    pub fn delete(service: &str, name: &str) -> Result<bool> {
        crate::guarded("the keychain", || match entry(service, name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(Error::NoEntry) => Ok(false),
            Err(e) => Err(map(e)),
        })
    }

    pub fn available() -> Result<()> {
        crate::guarded("the keychain", || open().map(|_| ()).map_err(map))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn get(_: &str, _: &str) -> Result<Option<String>> {
        Err(NativeError::Unsupported)
    }
    pub fn set(_: &str, _: &str, _: &str) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn delete(_: &str, _: &str) -> Result<bool> {
        Err(NativeError::Unsupported)
    }
    pub fn available() -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_before_the_os_is_asked() {
        assert!(Keychain::new("").is_err());
        assert!(Keychain::new("com.example/app").is_err());
        let k = Keychain::new("com.example.ashlar-test").unwrap();
        for bad in ["", "a b", "../x", "a/b", "KEY\n", "ÜBER", &"K".repeat(65)] {
            assert!(matches!(k.get(bad), Err(NativeError::Invalid(_))), "{bad:?}");
            assert!(matches!(k.delete(bad), Err(NativeError::Invalid(_))), "{bad:?}");
        }
        assert!(matches!(k.set("OK_NAME", ""), Err(NativeError::Invalid(_))));
        assert!(matches!(k.set("OK_NAME", &"x".repeat(MAX_SECRET + 1)), Err(NativeError::Invalid(_))));
    }

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    fn binding_errors_never_carry_the_stored_bytes() {
        use keyring_core::Error;
        let stored = b"FAKE-stored-value-0123456789".to_vec();
        let boxed = |s: &str| -> Box<dyn std::error::Error + Send + Sync> { s.to_string().into() };
        for e in [
            Error::BadEncoding(stored.clone()),
            Error::BadDataFormat(stored.clone(), boxed("bad format")),
            Error::NoStorageAccess(boxed("locked")),
            Error::PlatformFailure(boxed("line one\nline two")),
            Error::NoEntry,
        ] {
            let text = format!("{0} {0:?}", imp::map(e));
            assert!(!text.contains("FAKE-stored") && !text.contains("70, 65, 75"), "{text}");
            assert!(!text.contains('\n'), "{text}");
        }
        assert!(matches!(imp::map(Error::NoStorageAccess(boxed("locked"))), NativeError::Denied(_)));
    }

    /// The real store, by hand: `cargo test -p ashlar-native --features secrets -- --ignored keychain`.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    #[ignore = "writes to the real OS keychain"]
    fn real_keychain_round_trip() {
        let k = Keychain::new("com.example.ashlar-native-test").unwrap();
        const NAME: &str = "THROWAWAY";
        let _ = k.delete(NAME);
        assert_eq!(k.get(NAME).unwrap(), None);
        k.set(NAME, "one").unwrap();
        k.set(NAME, "two").unwrap();
        assert_eq!(k.get(NAME).unwrap().as_deref(), Some("two"));
        assert!(k.delete(NAME).unwrap());
        assert!(!k.delete(NAME).unwrap());
    }
}
