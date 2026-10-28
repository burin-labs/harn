Trusted launchers can confirm credential-bound provider account settings for
conditional no-training terms. Gemini requires both paid billing and disabled
Contributor Tier sharing; unknown settings continue to refuse no-training calls.
The reserved confirmation input is withheld from scripts and child processes,
including sessions that do not attach confirmations, and cannot be re-granted
under another environment name.

Migration: Rust callers constructing `DataControlsDef` or
`CatalogProviderDataControls` must supply `no_training_requires`, using an empty
vector when there are no account-dependent requirements. Callers constructing
`InferenceAdmissionSnapshot` must supply `required_account_facts` and
`account_confirmation_applied`; callers constructing `DataControlsReceipt` must
supply `account_confirmation_applied`. Use an empty vector and `false` when no
account confirmation is involved, and use the actual trusted policy and receipt
when one is involved.
