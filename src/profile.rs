//! Which game profile reads a container (SPEC §2.2).
//!
//! Phase 1 always uses the vanilla profile. Custom-game support (from CUE4Parse) will
//! choose a profile per folder (stored like AES keys, `uetoc.profiles.json`) and offer a
//! choice when a vanilla read fails; every open goes through [`profile_for`] so that
//! only this module changes.

use std::path::Path;
use std::sync::Arc;

use iostore::{GameProfile, Vanilla};

pub fn profile_for(_container: &Path) -> Arc<dyn GameProfile> {
    Arc::new(Vanilla)
}

/// Errors after which a different game profile might succeed (later: offer the choice).
pub fn could_be_custom_game(e: &iostore::Error) -> bool {
    matches!(
        e,
        iostore::Error::UnknownMagic
            | iostore::Error::Corrupt(_)
            | iostore::Error::BadDirectoryIndex(_)
            | iostore::Error::UnknownCompression(_)
            | iostore::Error::DecodeFailed { .. }
    )
}
