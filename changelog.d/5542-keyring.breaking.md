- Replace Harn's bespoke Keychain and Windows Credential Manager
  implementations with the maintained `keyring-core` platform stores, use
  Secret Service by default on Linux/Unix, and normalize native hostlib
  responses to `backend = "keyring"`. Set
  `HARN_SECRET_STORE_BACKEND=file` for headless environments.
