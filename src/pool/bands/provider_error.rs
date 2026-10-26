//! Provider-error classification over the runtime vocabulary.
//!
//! Hook point for a dependent slice: this module will own classification —
//! reading a provider response and naming its [`ProviderErrorClass`] and
//! cooldown, using the vocabulary in [`super::types`]. Nothing is defined
//! here yet.
